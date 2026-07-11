use std::fmt;
use std::marker::PhantomData;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::thread::{self, JoinHandle};

use bangbang_runtime::BackendError;
use bangbang_runtime::mmio::{MmioDispatchOutcome, MmioDispatcher};

use crate::backend::HvfBackend;
use crate::exit::{
    HvfHvcExit, HvfResolvedMmioAccess, HvfSys64Direction, HvfSys64Exit, HvfSys64Register,
    HvfVcpuExit, HvfVcpuExitResolveError,
};
use crate::gic::{
    HvfArm64GicIccRegisterState, HvfGicDeviceState, HvfGicError, HvfGicIccRegisterReader,
    HvfGicPpiPendingWriter, HvfGicStateSnapshotter, validate_gic_ppi_pending_intid,
};
use crate::mmio::HvfMmioDispatchError;
use crate::psci::{
    PsciCall, PsciCallAction, call_uses_arg0, handle_call as handle_psci_call, not_supported_result,
};
use crate::vcpu::{
    HvfArm64BootRegisters, HvfArm64VcpuCoreSystemRegisterState, HvfArm64VcpuExceptionRegisterState,
    HvfArm64VcpuExecutionControlRegisterState, HvfArm64VcpuGeneralRegisterState,
    HvfArm64VcpuIdentificationRegisterState, HvfArm64VcpuPendingInterruptState,
    HvfArm64VcpuPhysicalTimerState, HvfArm64VcpuPointerAuthenticationKeyState,
    HvfArm64VcpuSimdFpState, HvfArm64VcpuThreadContextRegisterState,
    HvfArm64VcpuTranslationRegisterState, HvfArm64VcpuVirtualTimerState, HvfInterruptType,
    HvfRegister, HvfSimdFpRegister, HvfSystemRegister, HvfVcpuOwner,
    capture_arm64_vcpu_core_system_register_state_with,
    capture_arm64_vcpu_exception_register_state_with,
    capture_arm64_vcpu_execution_control_register_state_with,
    capture_arm64_vcpu_general_register_state_with,
    capture_arm64_vcpu_identification_register_state_with,
    capture_arm64_vcpu_pending_interrupt_state_with, capture_arm64_vcpu_physical_timer_state_with,
    capture_arm64_vcpu_pointer_authentication_key_state_with,
    capture_arm64_vcpu_simd_fp_state_with, capture_arm64_vcpu_thread_context_register_state_with,
    capture_arm64_vcpu_translation_register_state_with,
};

const RUNNER_SHUT_DOWN_MESSAGE: &str = "vCPU runner is shut down";
const RUNNER_SHUTTING_DOWN_MESSAGE: &str = "vCPU runner shutdown is already in progress";
const RUN_IN_FLIGHT_MESSAGE: &str = "vCPU runner already has a run in flight";
const MMIO_DISPATCH_IN_FLIGHT_MESSAGE: &str = "vCPU runner already has MMIO dispatch in flight";
const RUN_NOT_STARTED_MESSAGE: &str = "vCPU runner has not started a run";
const BOOT_REGISTER_SETUP_IN_FLIGHT_MESSAGE: &str =
    "vCPU runner already has boot register setup in flight";
const BOOT_REGISTER_SETUP_FAILED_MESSAGE: &str =
    "vCPU runner boot register setup failed and must be retried";
const BOOT_REGISTERS_ALREADY_CONFIGURED_MESSAGE: &str =
    "vCPU runner boot registers are already configured";
const RUN_ALREADY_STARTED_MESSAGE: &str = "vCPU runner has already started a run";
const METADATA_READ_IN_FLIGHT_MESSAGE: &str = "vCPU runner already has metadata read in flight";
const CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE: &str =
    "vCPU runner already has core register capture in flight";
const TIMER_OPERATION_IN_FLIGHT_MESSAGE: &str = "vCPU runner already has timer operation in flight";
const INTERRUPT_OPERATION_IN_FLIGHT_MESSAGE: &str =
    "vCPU runner already has interrupt operation in flight";
const RUNNER_STATE_POISONED_MESSAGE: &str = "vCPU runner state lock is poisoned";
const MMIO_DISPATCHER_BUSY_MESSAGE: &str = "vCPU runner MMIO dispatcher lock is busy";
const MMIO_DISPATCHER_POISONED_MESSAGE: &str = "vCPU runner MMIO dispatcher lock is poisoned";
const COMMAND_CHANNEL_CLOSED_MESSAGE: &str = "vCPU runner command channel is closed";
const RESPONSE_CHANNEL_CLOSED_MESSAGE: &str = "vCPU runner response channel is closed";
const ARM64_INSTRUCTION_SIZE: u64 = 4;

type CancelVcpu = Arc<dyn Fn(crate::ffi::HvVcpu) -> Result<(), BackendError> + Send + Sync>;
type SharedMmioDispatcher = Arc<Mutex<MmioDispatcher>>;
type RunnerState = Arc<Mutex<RunnerHandleState>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfVcpuRunnerError {
    Backend(BackendError),
    Gic(HvfGicError),
    VcpuExitResolve(HvfVcpuExitResolveError),
    MmioDispatch(HvfMmioDispatchError),
    UnsupportedSys64 { exit: HvfSys64Exit },
    InvalidState(&'static str),
    ThreadSpawn(String),
    ChannelClosed(&'static str),
    ThreadPanicked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfVcpuRunStepOutcome {
    Canceled,
    Hvc {
        exit: HvfHvcExit,
        function_id: u64,
        return_value: u64,
    },
    GuestShutdown {
        exit: HvfHvcExit,
        function_id: u64,
        return_value: u64,
    },
    GuestReset {
        exit: HvfHvcExit,
        function_id: u64,
        return_value: u64,
    },
    Sys64 {
        exit: HvfSys64Exit,
    },
    Mmio {
        access: HvfResolvedMmioAccess,
        outcome: MmioDispatchOutcome,
    },
    VtimerActivated,
    Unknown {
        reason: u32,
    },
}

impl fmt::Display for HvfVcpuRunnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(err) => write!(f, "{err}"),
            Self::Gic(err) => write!(f, "{err}"),
            Self::VcpuExitResolve(err) => write!(f, "{err}"),
            Self::MmioDispatch(err) => write!(f, "{err}"),
            Self::UnsupportedSys64 { exit } => write!(
                f,
                "unsupported HVF SYS64 {:?} access to {} using Rt {}",
                exit.direction(),
                exit.register(),
                exit.target_register()
            ),
            Self::InvalidState(message) => write!(f, "invalid vCPU runner state: {message}"),
            Self::ThreadSpawn(message) => {
                write!(f, "failed to spawn vCPU runner thread: {message}")
            }
            Self::ChannelClosed(message) => write!(f, "vCPU runner channel closed: {message}"),
            Self::ThreadPanicked => f.write_str("vCPU runner thread panicked"),
        }
    }
}

impl std::error::Error for HvfVcpuRunnerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Backend(err) => Some(err),
            Self::Gic(err) => Some(err),
            Self::VcpuExitResolve(err) => Some(err),
            Self::MmioDispatch(err) => Some(err),
            Self::InvalidState(_)
            | Self::UnsupportedSys64 { .. }
            | Self::ThreadSpawn(_)
            | Self::ChannelClosed(_)
            | Self::ThreadPanicked => None,
        }
    }
}

impl From<BackendError> for HvfVcpuRunnerError {
    fn from(err: BackendError) -> Self {
        Self::Backend(err)
    }
}

impl From<HvfGicError> for HvfVcpuRunnerError {
    fn from(err: HvfGicError) -> Self {
        Self::Gic(err)
    }
}

impl From<HvfVcpuExitResolveError> for HvfVcpuRunnerError {
    fn from(err: HvfVcpuExitResolveError) -> Self {
        Self::VcpuExitResolve(err)
    }
}

impl From<HvfMmioDispatchError> for HvfVcpuRunnerError {
    fn from(err: HvfMmioDispatchError) -> Self {
        Self::MmioDispatch(err)
    }
}

pub struct HvfVcpuRunner<'vm> {
    command_sender: mpsc::Sender<RunnerCommand>,
    vcpu: crate::ffi::HvVcpu,
    cancel_vcpu: CancelVcpu,
    state: RunnerState,
    _vm: PhantomData<&'vm HvfBackend>,
}

#[derive(Clone)]
pub struct HvfVcpuRunCancelHandle {
    vcpu: crate::ffi::HvVcpu,
    cancel_vcpu: CancelVcpu,
    state: RunnerState,
}

impl HvfVcpuRunCancelHandle {
    /// Request cancellation of the runner's current `hv_vcpu_run` step.
    pub fn cancel(&self) -> Result<(), HvfVcpuRunnerError> {
        // Keep the state lock until the HVF exit request returns so shutdown
        // cannot destroy the vCPU while cancellation uses its raw id.
        let _state_guard = prepare_cancel(&self.state)?;
        cancel_vcpu(self.vcpu, &self.cancel_vcpu)
    }
}

impl fmt::Debug for HvfVcpuRunCancelHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfVcpuRunCancelHandle")
            .field("vcpu", &self.vcpu)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct RunnerHandleState {
    thread: Option<JoinHandle<()>>,
    shutting_down: bool,
    in_flight_runs: usize,
    mmio_dispatch_in_flight: bool,
    boot_register_setup_in_flight: bool,
    metadata_read_in_flight: bool,
    core_register_capture_in_flight: bool,
    timer_operation_in_flight: bool,
    interrupt_operation_in_flight: bool,
    boot_register_setup_failed: bool,
    boot_registers_configured: bool,
    run_started: bool,
}

enum RunnerCommand {
    ConfigureArm64BootRegisters {
        registers: HvfArm64BootRegisters,
        response_sender: mpsc::Sender<Result<(), HvfVcpuRunnerError>>,
    },
    RunOnce {
        response_sender: mpsc::Sender<Result<HvfVcpuExit, HvfVcpuRunnerError>>,
    },
    RunOnceAndHandleMmio {
        dispatcher: SharedMmioDispatcher,
        response_sender: mpsc::Sender<Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError>>,
    },
    DispatchMmioAccess {
        access: HvfResolvedMmioAccess,
        dispatcher: SharedMmioDispatcher,
        response_sender: mpsc::Sender<Result<MmioDispatchOutcome, HvfVcpuRunnerError>>,
    },
    ReadMpidrEl1 {
        response_sender: mpsc::Sender<Result<u64, HvfVcpuRunnerError>>,
    },
    CaptureArm64GeneralRegisterState {
        admission: InFlightCoreRegisterCapture,
        response_sender: mpsc::Sender<Result<HvfArm64VcpuGeneralRegisterState, HvfVcpuRunnerError>>,
    },
    CaptureArm64CoreSystemRegisterState {
        admission: InFlightCoreRegisterCapture,
        response_sender:
            mpsc::Sender<Result<HvfArm64VcpuCoreSystemRegisterState, HvfVcpuRunnerError>>,
    },
    CaptureArm64ExceptionRegisterState {
        admission: InFlightCoreRegisterCapture,
        response_sender:
            mpsc::Sender<Result<HvfArm64VcpuExceptionRegisterState, HvfVcpuRunnerError>>,
    },
    CaptureArm64ExecutionControlRegisterState {
        admission: InFlightCoreRegisterCapture,
        response_sender:
            mpsc::Sender<Result<HvfArm64VcpuExecutionControlRegisterState, HvfVcpuRunnerError>>,
    },
    CaptureArm64IdentificationRegisterState {
        admission: InFlightCoreRegisterCapture,
        response_sender:
            mpsc::Sender<Result<HvfArm64VcpuIdentificationRegisterState, HvfVcpuRunnerError>>,
    },
    CaptureArm64TranslationRegisterState {
        admission: InFlightCoreRegisterCapture,
        response_sender:
            mpsc::Sender<Result<HvfArm64VcpuTranslationRegisterState, HvfVcpuRunnerError>>,
    },
    CaptureArm64PointerAuthenticationKeyState {
        admission: InFlightCoreRegisterCapture,
        response_sender:
            mpsc::Sender<Result<HvfArm64VcpuPointerAuthenticationKeyState, HvfVcpuRunnerError>>,
    },
    CaptureArm64ThreadContextRegisterState {
        admission: InFlightCoreRegisterCapture,
        response_sender:
            mpsc::Sender<Result<HvfArm64VcpuThreadContextRegisterState, HvfVcpuRunnerError>>,
    },
    CaptureArm64SimdFpState {
        admission: InFlightCoreRegisterCapture,
        response_sender: mpsc::Sender<Result<HvfArm64VcpuSimdFpState, HvfVcpuRunnerError>>,
    },
    GetVtimerMask {
        response_sender: mpsc::Sender<Result<bool, HvfVcpuRunnerError>>,
    },
    SetVtimerMask {
        masked: bool,
        response_sender: mpsc::Sender<Result<(), HvfVcpuRunnerError>>,
    },
    GetVtimerOffset {
        response_sender: mpsc::Sender<Result<u64, HvfVcpuRunnerError>>,
    },
    SetVtimerOffset {
        offset: u64,
        response_sender: mpsc::Sender<Result<(), HvfVcpuRunnerError>>,
    },
    GetVtimerControl {
        response_sender: mpsc::Sender<Result<u64, HvfVcpuRunnerError>>,
    },
    SetVtimerControl {
        control: u64,
        response_sender: mpsc::Sender<Result<(), HvfVcpuRunnerError>>,
    },
    GetVtimerCompareValue {
        response_sender: mpsc::Sender<Result<u64, HvfVcpuRunnerError>>,
    },
    SetVtimerCompareValue {
        compare_value: u64,
        response_sender: mpsc::Sender<Result<(), HvfVcpuRunnerError>>,
    },
    CaptureArm64PhysicalTimerState {
        admission: InFlightTimerOperation,
        response_sender: mpsc::Sender<Result<HvfArm64VcpuPhysicalTimerState, HvfVcpuRunnerError>>,
    },
    CaptureArm64VirtualTimerState {
        admission: InFlightTimerOperation,
        response_sender: mpsc::Sender<Result<HvfArm64VcpuVirtualTimerState, HvfVcpuRunnerError>>,
    },
    GetPendingInterrupt {
        interrupt_type: HvfInterruptType,
        response_sender: mpsc::Sender<Result<bool, HvfVcpuRunnerError>>,
    },
    SetPendingInterrupt {
        interrupt_type: HvfInterruptType,
        pending: bool,
        response_sender: mpsc::Sender<Result<(), HvfVcpuRunnerError>>,
    },
    CaptureArm64PendingInterruptState {
        admission: InFlightInterruptOperation,
        response_sender:
            mpsc::Sender<Result<HvfArm64VcpuPendingInterruptState, HvfVcpuRunnerError>>,
    },
    CaptureGicDeviceState {
        admission: InFlightInterruptOperation,
        response_sender: mpsc::Sender<Result<HvfGicDeviceState, HvfVcpuRunnerError>>,
    },
    CaptureArm64GicIccRegisterState {
        admission: InFlightInterruptOperation,
        response_sender: mpsc::Sender<Result<HvfArm64GicIccRegisterState, HvfVcpuRunnerError>>,
    },
    SetGicPpiPending {
        intid: u32,
        pending: bool,
        response_sender: mpsc::Sender<Result<(), HvfVcpuRunnerError>>,
    },
    Shutdown {
        response_sender: mpsc::Sender<Result<(), HvfVcpuRunnerError>>,
    },
}

struct StartedRunner {
    command_sender: mpsc::Sender<RunnerCommand>,
    vcpu: crate::ffi::HvVcpu,
    thread: JoinHandle<()>,
}

trait RunnerVcpu {
    fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError>;
    fn configure_arm64_boot_registers(
        &mut self,
        registers: HvfArm64BootRegisters,
    ) -> Result<(), BackendError>;
    fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError>;
    fn dispatch_mmio_access(
        &mut self,
        access: HvfResolvedMmioAccess,
        dispatcher: &mut MmioDispatcher,
    ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError>;
    fn read_register(&mut self, _register: HvfRegister) -> Result<u64, BackendError> {
        Err(BackendError::InvalidState(
            "vCPU does not support general register reads",
        ))
    }
    fn write_register(&mut self, _register: HvfRegister, _value: u64) -> Result<(), BackendError> {
        Err(BackendError::InvalidState(
            "vCPU does not support general register writes",
        ))
    }
    fn capture_arm64_general_register_state(
        &mut self,
    ) -> Result<HvfArm64VcpuGeneralRegisterState, BackendError> {
        capture_arm64_vcpu_general_register_state_with(|register| self.read_register(register))
    }
    fn read_simd_fp_register(
        &mut self,
        _register: HvfSimdFpRegister,
    ) -> Result<[u8; 16], BackendError> {
        Err(BackendError::InvalidState(
            "vCPU does not support SIMD/FP register reads",
        ))
    }
    fn capture_arm64_simd_fp_state(&mut self) -> Result<HvfArm64VcpuSimdFpState, BackendError> {
        capture_arm64_vcpu_simd_fp_state_with(
            self,
            |vcpu, register| vcpu.read_simd_fp_register(register),
            |vcpu, register| vcpu.read_register(register),
        )
    }
    fn read_system_register(&mut self, _register: HvfSystemRegister) -> Result<u64, BackendError> {
        Err(BackendError::InvalidState(
            "vCPU does not support system register reads",
        ))
    }
    fn capture_arm64_core_system_register_state(
        &mut self,
    ) -> Result<HvfArm64VcpuCoreSystemRegisterState, BackendError> {
        capture_arm64_vcpu_core_system_register_state_with(|register| {
            self.read_system_register(register)
        })
    }
    fn capture_arm64_exception_register_state(
        &mut self,
    ) -> Result<HvfArm64VcpuExceptionRegisterState, BackendError> {
        capture_arm64_vcpu_exception_register_state_with(|register| {
            self.read_system_register(register)
        })
    }
    fn capture_arm64_execution_control_register_state(
        &mut self,
    ) -> Result<HvfArm64VcpuExecutionControlRegisterState, BackendError> {
        capture_arm64_vcpu_execution_control_register_state_with(|register| {
            self.read_system_register(register)
        })
    }
    fn capture_arm64_identification_register_state(
        &mut self,
    ) -> Result<HvfArm64VcpuIdentificationRegisterState, BackendError> {
        capture_arm64_vcpu_identification_register_state_with(|register| {
            self.read_system_register(register)
        })
    }
    fn capture_arm64_translation_register_state(
        &mut self,
    ) -> Result<HvfArm64VcpuTranslationRegisterState, BackendError> {
        capture_arm64_vcpu_translation_register_state_with(|register| {
            self.read_system_register(register)
        })
    }
    fn capture_arm64_pointer_authentication_key_state(
        &mut self,
    ) -> Result<HvfArm64VcpuPointerAuthenticationKeyState, BackendError> {
        capture_arm64_vcpu_pointer_authentication_key_state_with(|register| {
            self.read_system_register(register)
        })
    }
    fn capture_arm64_thread_context_register_state(
        &mut self,
    ) -> Result<HvfArm64VcpuThreadContextRegisterState, BackendError> {
        capture_arm64_vcpu_thread_context_register_state_with(|register| {
            self.read_system_register(register)
        })
    }
    fn mpidr_el1(&mut self) -> Result<u64, BackendError> {
        Err(BackendError::InvalidState(
            "vCPU does not support MPIDR_EL1 reads",
        ))
    }
    fn get_vtimer_mask(&mut self) -> Result<bool, BackendError> {
        Err(BackendError::InvalidState(
            "vCPU does not support virtual timer mask reads",
        ))
    }
    fn set_vtimer_mask(&mut self, _masked: bool) -> Result<(), BackendError> {
        Err(BackendError::InvalidState(
            "vCPU does not support virtual timer mask writes",
        ))
    }
    fn get_vtimer_offset(&mut self) -> Result<u64, BackendError> {
        Err(BackendError::InvalidState(
            "vCPU does not support virtual timer offset reads",
        ))
    }
    fn set_vtimer_offset(&mut self, _offset: u64) -> Result<(), BackendError> {
        Err(BackendError::InvalidState(
            "vCPU does not support virtual timer offset writes",
        ))
    }
    fn get_vtimer_control(&mut self) -> Result<u64, BackendError> {
        Err(BackendError::InvalidState(
            "vCPU does not support virtual timer control reads",
        ))
    }
    fn set_vtimer_control(&mut self, _control: u64) -> Result<(), BackendError> {
        Err(BackendError::InvalidState(
            "vCPU does not support virtual timer control writes",
        ))
    }
    fn get_vtimer_compare_value(&mut self) -> Result<u64, BackendError> {
        Err(BackendError::InvalidState(
            "vCPU does not support virtual timer compare-value reads",
        ))
    }
    fn set_vtimer_compare_value(&mut self, _compare_value: u64) -> Result<(), BackendError> {
        Err(BackendError::InvalidState(
            "vCPU does not support virtual timer compare-value writes",
        ))
    }
    fn capture_arm64_physical_timer_state(
        &mut self,
    ) -> Result<HvfArm64VcpuPhysicalTimerState, BackendError> {
        capture_arm64_vcpu_physical_timer_state_with(|register| self.read_system_register(register))
    }
    fn capture_arm64_virtual_timer_state(
        &mut self,
    ) -> Result<HvfArm64VcpuVirtualTimerState, BackendError> {
        let masked = self.get_vtimer_mask()?;
        let offset = self.get_vtimer_offset()?;
        let control = self.get_vtimer_control()?;
        let compare_value = self.get_vtimer_compare_value()?;
        Ok(HvfArm64VcpuVirtualTimerState::new(
            masked,
            offset,
            control,
            compare_value,
        ))
    }
    fn get_pending_interrupt(
        &mut self,
        _interrupt_type: HvfInterruptType,
    ) -> Result<bool, BackendError> {
        Err(BackendError::InvalidState(
            "vCPU does not support pending interrupt reads",
        ))
    }
    fn set_pending_interrupt(
        &mut self,
        _interrupt_type: HvfInterruptType,
        _pending: bool,
    ) -> Result<(), BackendError> {
        Err(BackendError::InvalidState(
            "vCPU does not support pending interrupt writes",
        ))
    }
    fn capture_arm64_pending_interrupt_state(
        &mut self,
    ) -> Result<HvfArm64VcpuPendingInterruptState, BackendError> {
        capture_arm64_vcpu_pending_interrupt_state_with(|interrupt_type| {
            self.get_pending_interrupt(interrupt_type)
        })
    }
    fn set_gic_ppi_pending(&mut self, _intid: u32, _pending: bool) -> Result<(), HvfGicError> {
        Err(HvfGicError::InvalidState(
            "vCPU does not support GIC PPI pending control",
        ))
    }
    fn capture_gic_device_state(&mut self) -> Result<HvfGicDeviceState, HvfGicError> {
        Err(HvfGicError::InvalidState(
            "vCPU does not support GIC device-state capture",
        ))
    }
    fn capture_arm64_gic_icc_register_state(
        &mut self,
    ) -> Result<HvfArm64GicIccRegisterState, HvfGicError> {
        Err(HvfGicError::InvalidState(
            "vCPU does not support GIC ICC register-state capture",
        ))
    }
    fn destroy(&mut self) -> Result<(), BackendError>;
}

struct RealRunnerVcpu {
    owner: HvfVcpuOwner,
    gic_ppi_pending_writer: Option<HvfGicPpiPendingWriter>,
    gic_state_snapshotter: Option<HvfGicStateSnapshotter>,
    gic_icc_register_reader: Option<HvfGicIccRegisterReader>,
}

impl RealRunnerVcpu {
    fn create() -> Result<Self, BackendError> {
        let mut owner = HvfVcpuOwner::new()?;
        // Hypervisor.framework GIC redistributor access requires vCPU
        // affinity before the topology is finalized. The current runner owns a
        // single primary vCPU, so affinity 0 is the deterministic topology.
        owner.set_system_register(HvfSystemRegister::MPIDR_EL1, 0)?;

        Ok(Self {
            owner,
            gic_ppi_pending_writer: None,
            gic_state_snapshotter: None,
            gic_icc_register_reader: None,
        })
    }
}

impl RunnerVcpu for RealRunnerVcpu {
    fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
        self.owner.raw_vcpu()
    }

    fn configure_arm64_boot_registers(
        &mut self,
        registers: HvfArm64BootRegisters,
    ) -> Result<(), BackendError> {
        self.owner.configure_arm64_boot_registers(registers)
    }

    fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
        self.owner.run_once()
    }

    fn dispatch_mmio_access(
        &mut self,
        access: HvfResolvedMmioAccess,
        dispatcher: &mut MmioDispatcher,
    ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
        self.owner
            .dispatch_mmio_access(access, dispatcher)
            .map_err(HvfVcpuRunnerError::MmioDispatch)
    }

    fn read_register(&mut self, register: HvfRegister) -> Result<u64, BackendError> {
        self.owner.get_register(register)
    }

    fn write_register(&mut self, register: HvfRegister, value: u64) -> Result<(), BackendError> {
        self.owner.set_register(register, value)
    }

    fn read_simd_fp_register(
        &mut self,
        register: HvfSimdFpRegister,
    ) -> Result<[u8; 16], BackendError> {
        self.owner.get_simd_fp_register(register)
    }

    fn read_system_register(&mut self, register: HvfSystemRegister) -> Result<u64, BackendError> {
        self.owner.get_system_register(register)
    }

    fn mpidr_el1(&mut self) -> Result<u64, BackendError> {
        self.owner.get_system_register(HvfSystemRegister::MPIDR_EL1)
    }

    fn get_vtimer_mask(&mut self) -> Result<bool, BackendError> {
        self.owner.get_vtimer_mask()
    }

    fn set_vtimer_mask(&mut self, masked: bool) -> Result<(), BackendError> {
        self.owner.set_vtimer_mask(masked)
    }

    fn get_vtimer_offset(&mut self) -> Result<u64, BackendError> {
        self.owner.get_vtimer_offset()
    }

    fn set_vtimer_offset(&mut self, offset: u64) -> Result<(), BackendError> {
        self.owner.set_vtimer_offset(offset)
    }

    fn get_vtimer_control(&mut self) -> Result<u64, BackendError> {
        self.owner
            .get_system_register(HvfSystemRegister::CNTV_CTL_EL0)
    }

    fn set_vtimer_control(&mut self, control: u64) -> Result<(), BackendError> {
        self.owner
            .set_system_register(HvfSystemRegister::CNTV_CTL_EL0, control)
    }

    fn get_vtimer_compare_value(&mut self) -> Result<u64, BackendError> {
        self.owner
            .get_system_register(HvfSystemRegister::CNTV_CVAL_EL0)
    }

    fn set_vtimer_compare_value(&mut self, compare_value: u64) -> Result<(), BackendError> {
        self.owner
            .set_system_register(HvfSystemRegister::CNTV_CVAL_EL0, compare_value)
    }

    fn get_pending_interrupt(
        &mut self,
        interrupt_type: HvfInterruptType,
    ) -> Result<bool, BackendError> {
        self.owner.get_pending_interrupt(interrupt_type)
    }

    fn set_pending_interrupt(
        &mut self,
        interrupt_type: HvfInterruptType,
        pending: bool,
    ) -> Result<(), BackendError> {
        self.owner.set_pending_interrupt(interrupt_type, pending)
    }

    fn set_gic_ppi_pending(&mut self, intid: u32, pending: bool) -> Result<(), HvfGicError> {
        validate_gic_ppi_pending_intid(intid)?;
        if self.gic_ppi_pending_writer.is_none() {
            self.gic_ppi_pending_writer = Some(HvfGicPpiPendingWriter::new()?);
        }
        let writer = self
            .gic_ppi_pending_writer
            .as_ref()
            .ok_or(HvfGicError::InvalidState(
                "GIC PPI pending writer was not initialized",
            ))?;

        self.owner.set_gic_ppi_pending(writer, intid, pending)
    }

    fn capture_gic_device_state(&mut self) -> Result<HvfGicDeviceState, HvfGicError> {
        if self.gic_state_snapshotter.is_none() {
            self.gic_state_snapshotter = Some(HvfGicStateSnapshotter::new()?);
        }
        let snapshotter = self
            .gic_state_snapshotter
            .as_ref()
            .ok_or(HvfGicError::InvalidState(
                "GIC state snapshotter was not initialized",
            ))?;

        snapshotter.capture()
    }

    fn capture_arm64_gic_icc_register_state(
        &mut self,
    ) -> Result<HvfArm64GicIccRegisterState, HvfGicError> {
        let vcpu = self.owner.raw_vcpu()?;
        if self.gic_icc_register_reader.is_none() {
            self.gic_icc_register_reader = Some(HvfGicIccRegisterReader::new()?);
        }
        let reader = self
            .gic_icc_register_reader
            .as_ref()
            .ok_or(HvfGicError::InvalidState(
                "GIC ICC register reader was not initialized",
            ))?;

        reader.capture(vcpu)
    }

    fn destroy(&mut self) -> Result<(), BackendError> {
        self.owner.destroy()
    }
}

impl<'vm> HvfVcpuRunner<'vm> {
    pub(crate) fn new() -> Result<Self, HvfVcpuRunnerError> {
        Self::from_started(
            spawn_runner_thread(RealRunnerVcpu::create)?,
            real_cancel_vcpu(),
        )
    }

    /// Configure the primary arm64 Linux boot-register state on the vCPU-owning runner thread.
    pub fn configure_arm64_boot_registers(
        &self,
        registers: HvfArm64BootRegisters,
    ) -> Result<(), HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        let mut setup = self.start_arm64_boot_register_setup(registers, response_sender)?;

        let result = response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?;
        match &result {
            Ok(()) => setup.mark_configured(),
            Err(HvfVcpuRunnerError::Backend(_)) => setup.mark_failed(),
            Err(
                HvfVcpuRunnerError::Gic(_)
                | HvfVcpuRunnerError::InvalidState(_)
                | HvfVcpuRunnerError::UnsupportedSys64 { .. }
                | HvfVcpuRunnerError::VcpuExitResolve(_)
                | HvfVcpuRunnerError::MmioDispatch(_)
                | HvfVcpuRunnerError::ThreadSpawn(_)
                | HvfVcpuRunnerError::ChannelClosed(_)
                | HvfVcpuRunnerError::ThreadPanicked,
            ) => {}
        }

        result
    }

    pub fn run_once(&self) -> Result<HvfVcpuExit, HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        let _in_flight_run = self.start_run_once(response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    /// Run the vCPU once and handle a resulting MMIO exit on the vCPU-owning runner thread.
    pub fn run_once_and_handle_mmio(
        &self,
        dispatcher: Arc<Mutex<MmioDispatcher>>,
    ) -> Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        let _in_flight_run = self.start_run_once_and_handle_mmio(dispatcher, response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    /// Dispatch one resolved HVF MMIO access on the vCPU-owning runner thread.
    pub fn dispatch_mmio_access(
        &self,
        access: HvfResolvedMmioAccess,
        dispatcher: Arc<Mutex<MmioDispatcher>>,
    ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        let _in_flight_dispatch = self.start_mmio_dispatch(access, dispatcher, response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    pub fn cancel(&self) -> Result<(), HvfVcpuRunnerError> {
        self.run_cancel_handle().cancel()
    }

    /// Return a cloneable handle that can request cancellation of an in-flight run step.
    pub fn run_cancel_handle(&self) -> HvfVcpuRunCancelHandle {
        HvfVcpuRunCancelHandle {
            vcpu: self.vcpu,
            cancel_vcpu: Arc::clone(&self.cancel_vcpu),
            state: Arc::clone(&self.state),
        }
    }

    /// Read the primary vCPU MPIDR on the vCPU-owning runner thread.
    pub fn mpidr_el1(&self) -> Result<u64, HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        let _in_flight_read = self.start_mpidr_el1_read(response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    /// Capture X0-X30, PC, and CPSR on the vCPU-owning runner thread.
    ///
    /// This value is a read-only architectural subset for later snapshot
    /// orchestration, not a complete or serialized vCPU state.
    pub fn capture_arm64_general_register_state(
        &self,
    ) -> Result<HvfArm64VcpuGeneralRegisterState, HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        self.start_arm64_general_register_capture(response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    /// Capture raw SP_EL0, SP_EL1, ELR_EL1, and SPSR_EL1 values on the
    /// vCPU-owning runner thread.
    ///
    /// This is a read-only core system-register subset for later snapshot
    /// orchestration, not a complete or serialized vCPU state.
    pub fn capture_arm64_core_system_register_state(
        &self,
    ) -> Result<HvfArm64VcpuCoreSystemRegisterState, HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        self.start_arm64_core_system_register_capture(response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    /// Capture raw EL1 exception-register state on the owner thread.
    ///
    /// This captures AFSR0/1, ESR, FAR, PAR, and VBAR as unvalidated
    /// observations. It does not capture vector-table memory, validate one
    /// coherent exception report, or provide a safe restore sequence.
    pub fn capture_arm64_exception_register_state(
        &self,
    ) -> Result<HvfArm64VcpuExceptionRegisterState, HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        self.start_arm64_exception_register_capture(response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    /// Capture raw EL1 ACTLR and CPACR execution controls on the owner thread.
    ///
    /// Complete capture requires macOS 15 because Hypervisor.framework exposes
    /// ACTLR_EL1.EnTSO there. This value is not feature-validated and does not
    /// provide writable-bit or ISB ordering policy for restore.
    pub fn capture_arm64_execution_control_register_state(
        &self,
    ) -> Result<HvfArm64VcpuExecutionControlRegisterState, HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        self.start_arm64_execution_control_register_capture(response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    /// Capture the guest-visible arm64 processor identification registers on
    /// the vCPU-owning runner thread.
    ///
    /// MIDR, MPIDR, and the baseline PFR/DFR/ISAR/MMFR values are raw
    /// virtual-CPU and Hypervisor.framework compatibility inputs. This method
    /// does not expose physical-host identity, decide destination compatibility,
    /// or provide mutable restore state or a serialized schema.
    pub fn capture_arm64_identification_register_state(
        &self,
    ) -> Result<HvfArm64VcpuIdentificationRegisterState, HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        self.start_arm64_identification_register_capture(response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    /// Capture raw EL1 translation-register state on the owner thread.
    ///
    /// This captures SCTLR, TTBR0/TTBR1, TCR, MAIR, AMAIR, and CONTEXTIDR as
    /// unvalidated observations. It does not capture table memory, validate
    /// features, define TLB/cache maintenance, or provide a restore sequence.
    pub fn capture_arm64_translation_register_state(
        &self,
    ) -> Result<HvfArm64VcpuTranslationRegisterState, HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        self.start_arm64_translation_register_capture(response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    /// Capture the five raw EL1 pointer-authentication keys on the vCPU-owning
    /// runner thread.
    ///
    /// These values are cryptographic secrets whose `Debug` output is redacted.
    /// This getter-only subset has no feature validation, persistence
    /// protection, restore ordering, or serialized schema policy.
    pub fn capture_arm64_pointer_authentication_key_state(
        &self,
    ) -> Result<HvfArm64VcpuPointerAuthenticationKeyState, HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        self.start_arm64_pointer_authentication_key_capture(response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    /// Capture raw TPIDR_EL0, TPIDRRO_EL0, and TPIDR_EL1 values on the
    /// vCPU-owning runner thread.
    ///
    /// These sensitive software thread-ID values can contain guest pointers.
    /// This is not a complete or serialized restorable vCPU state.
    pub fn capture_arm64_thread_context_register_state(
        &self,
    ) -> Result<HvfArm64VcpuThreadContextRegisterState, HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        self.start_arm64_thread_context_register_capture(response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    /// Capture raw Q0-Q31, FPCR, and FPSR values on the vCPU-owning runner
    /// thread.
    ///
    /// Q values are the baseline 128-bit SIMD view and alias the low 128 bits
    /// of Z registers in streaming SVE mode. This is not complete SVE/SME or
    /// serialized restorable vCPU state.
    pub fn capture_arm64_simd_fp_state(
        &self,
    ) -> Result<HvfArm64VcpuSimdFpState, HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        self.start_arm64_simd_fp_capture(response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    /// Read the HVF virtual timer mask on the vCPU-owning runner thread.
    pub fn get_vtimer_mask(&self) -> Result<bool, HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        let _in_flight_operation = self.start_get_vtimer_mask(response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    /// Set the HVF virtual timer mask on the vCPU-owning runner thread.
    pub fn set_vtimer_mask(&self, masked: bool) -> Result<(), HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        let _in_flight_operation = self.start_set_vtimer_mask(masked, response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    /// Read the raw HVF virtual-timer offset on the vCPU-owning runner thread.
    pub fn get_vtimer_offset(&self) -> Result<u64, HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        let _in_flight_operation = self.start_get_vtimer_offset(response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    /// Set the raw HVF virtual-timer offset on the vCPU-owning runner thread.
    pub fn set_vtimer_offset(&self, offset: u64) -> Result<(), HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        let _in_flight_operation = self.start_set_vtimer_offset(offset, response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    /// Read the raw `CNTV_CTL_EL0` value on the vCPU-owning runner thread.
    pub fn get_vtimer_control(&self) -> Result<u64, HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        let _in_flight_operation = self.start_get_vtimer_control(response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    /// Set the raw `CNTV_CTL_EL0` value on the vCPU-owning runner thread.
    pub fn set_vtimer_control(&self, control: u64) -> Result<(), HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        let _in_flight_operation = self.start_set_vtimer_control(control, response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    /// Read the raw `CNTV_CVAL_EL0` value on the vCPU-owning runner thread.
    pub fn get_vtimer_compare_value(&self) -> Result<u64, HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        let _in_flight_operation = self.start_get_vtimer_compare_value(response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    /// Set the raw `CNTV_CVAL_EL0` value on the vCPU-owning runner thread.
    pub fn set_vtimer_compare_value(&self, compare_value: u64) -> Result<(), HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        let _in_flight_operation =
            self.start_set_vtimer_compare_value(compare_value, response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    /// Capture raw EL1 physical-timer access, control, and compare state.
    ///
    /// The CNTP fields require macOS 15 and a GIC created before the vCPU. The
    /// control status bit is derived, and the absolute compare value has no
    /// portable snapshot-time adjustment or restore policy.
    pub fn capture_arm64_physical_timer_state(
        &self,
    ) -> Result<HvfArm64VcpuPhysicalTimerState, HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        self.start_arm64_physical_timer_capture(response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    /// Capture raw virtual-timer mask, offset, control, and compare state.
    ///
    /// The result does not include pending interrupts, GIC state, or a portable
    /// snapshot-time adjustment policy. Its control status bit is derived and
    /// may change as virtual time advances.
    pub fn capture_arm64_virtual_timer_state(
        &self,
    ) -> Result<HvfArm64VcpuVirtualTimerState, HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        self.start_arm64_virtual_timer_capture(response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    /// Read one CPU-level pending interrupt injection on the owner thread.
    pub fn get_pending_interrupt(
        &self,
        interrupt_type: HvfInterruptType,
    ) -> Result<bool, HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        let _in_flight_operation =
            self.start_get_pending_interrupt(interrupt_type, response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    /// Set one CPU-level pending interrupt injection on the owner thread.
    ///
    /// Hypervisor.framework clears this level after the next vCPU run returns.
    pub fn set_pending_interrupt(
        &self,
        interrupt_type: HvfInterruptType,
        pending: bool,
    ) -> Result<(), HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        let _in_flight_operation =
            self.start_set_pending_interrupt(interrupt_type, pending, response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    /// Capture CPU-level IRQ and FIQ pending state on the owner thread.
    ///
    /// This value excludes GIC/device state and is not a serialized snapshot
    /// schema. HVF clears both injection levels after a vCPU run returns.
    pub fn capture_arm64_pending_interrupt_state(
        &self,
    ) -> Result<HvfArm64VcpuPendingInterruptState, HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        self.start_arm64_pending_interrupt_capture(response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    /// Capture Hypervisor.framework's opaque GIC device state.
    ///
    /// The command runs while this current single-vCPU runner is stopped and
    /// serialized against runner-managed CPU/PPI interrupt mutations. It does
    /// not quiesce external SPI producers. The versioned bytes exclude GIC CPU
    /// system registers and are not a complete or restored snapshot.
    pub fn capture_gic_device_state(&self) -> Result<HvfGicDeviceState, HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        self.start_gic_device_state_capture(response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    /// Capture the arm64 EL1 GIC ICC CPU-interface registers on the owner thread.
    ///
    /// The command captures all ten EL1 ICC values exposed by the macOS 15
    /// Hypervisor.framework API and serializes them with other runner-managed
    /// interrupt/GIC operations. It covers only this current single vCPU and
    /// excludes `ICC_SRE_EL2`, ICH/ICV state, restore, and snapshot persistence.
    pub fn capture_arm64_gic_icc_register_state(
        &self,
    ) -> Result<HvfArm64GicIccRegisterState, HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        self.start_arm64_gic_icc_register_state_capture(response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    /// Set a GIC PPI pending bit on the vCPU-owning runner thread.
    pub fn set_gic_ppi_pending(&self, intid: u32) -> Result<(), HvfVcpuRunnerError> {
        self.set_gic_ppi_pending_to(intid, true)
    }

    /// Clear a GIC PPI pending bit on the vCPU-owning runner thread.
    pub fn clear_gic_ppi_pending(&self, intid: u32) -> Result<(), HvfVcpuRunnerError> {
        self.set_gic_ppi_pending_to(intid, false)
    }

    fn set_gic_ppi_pending_to(&self, intid: u32, pending: bool) -> Result<(), HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        let _in_flight_operation =
            self.start_gic_ppi_pending_operation(intid, pending, response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    pub fn shutdown(&self) -> Result<(), HvfVcpuRunnerError> {
        let (command_sender, should_cancel) = match self.prepare_shutdown() {
            Ok(prepared_shutdown) => prepared_shutdown,
            Err(HvfVcpuRunnerError::InvalidState(RUNNER_SHUT_DOWN_MESSAGE)) => return Ok(()),
            Err(err) => return Err(err),
        };

        if should_cancel && let Err(err) = self.cancel_vcpu() {
            self.cancel_shutdown();
            return Err(err);
        }

        let (response_sender, response_receiver) = mpsc::channel();
        let send_result = command_sender
            .send(RunnerCommand::Shutdown { response_sender })
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(COMMAND_CHANNEL_CLOSED_MESSAGE));

        let thread = self.take_thread()?;

        let response_result = match send_result {
            Ok(()) => response_receiver
                .recv()
                .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?,
            Err(err) => Err(err),
        };
        let join_result = join_runner_thread(thread);
        self.finish_shutdown();

        shutdown_result(response_result, join_result)
    }

    fn from_started(
        started: StartedRunner,
        cancel_vcpu: CancelVcpu,
    ) -> Result<Self, HvfVcpuRunnerError> {
        Ok(Self {
            command_sender: started.command_sender,
            vcpu: started.vcpu,
            cancel_vcpu,
            state: Arc::new(Mutex::new(RunnerHandleState {
                thread: Some(started.thread),
                shutting_down: false,
                in_flight_runs: 0,
                mmio_dispatch_in_flight: false,
                boot_register_setup_in_flight: false,
                metadata_read_in_flight: false,
                core_register_capture_in_flight: false,
                timer_operation_in_flight: false,
                interrupt_operation_in_flight: false,
                boot_register_setup_failed: false,
                boot_registers_configured: false,
                run_started: false,
            })),
            _vm: PhantomData,
        })
    }

    fn start_arm64_boot_register_setup(
        &self,
        registers: HvfArm64BootRegisters,
        response_sender: mpsc::Sender<Result<(), HvfVcpuRunnerError>>,
    ) -> Result<InFlightBootRegisterSetup, HvfVcpuRunnerError> {
        let mut state = self.lock_state()?;
        if state.thread.is_none() {
            return Err(HvfVcpuRunnerError::InvalidState(RUNNER_SHUT_DOWN_MESSAGE));
        }
        if state.shutting_down {
            return Err(HvfVcpuRunnerError::InvalidState(
                RUNNER_SHUTTING_DOWN_MESSAGE,
            ));
        }
        if state.in_flight_runs > 0 {
            return Err(HvfVcpuRunnerError::InvalidState(RUN_IN_FLIGHT_MESSAGE));
        }
        if state.mmio_dispatch_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                MMIO_DISPATCH_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.boot_register_setup_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                BOOT_REGISTER_SETUP_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.metadata_read_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                METADATA_READ_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.core_register_capture_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.timer_operation_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                TIMER_OPERATION_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.interrupt_operation_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                INTERRUPT_OPERATION_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.run_started {
            return Err(HvfVcpuRunnerError::InvalidState(
                RUN_ALREADY_STARTED_MESSAGE,
            ));
        }
        if state.boot_registers_configured {
            return Err(HvfVcpuRunnerError::InvalidState(
                BOOT_REGISTERS_ALREADY_CONFIGURED_MESSAGE,
            ));
        }

        state.boot_register_setup_in_flight = true;
        if self
            .command_sender
            .send(RunnerCommand::ConfigureArm64BootRegisters {
                registers,
                response_sender,
            })
            .is_err()
        {
            state.boot_register_setup_in_flight = false;
            return Err(HvfVcpuRunnerError::ChannelClosed(
                COMMAND_CHANNEL_CLOSED_MESSAGE,
            ));
        }

        Ok(InFlightBootRegisterSetup::new(&self.state))
    }

    fn start_run_once(
        &self,
        response_sender: mpsc::Sender<Result<HvfVcpuExit, HvfVcpuRunnerError>>,
    ) -> Result<InFlightRun, HvfVcpuRunnerError> {
        let mut state = self.lock_state()?;
        if state.thread.is_none() || state.shutting_down {
            return Err(HvfVcpuRunnerError::InvalidState(RUNNER_SHUT_DOWN_MESSAGE));
        }
        if state.in_flight_runs > 0 {
            return Err(HvfVcpuRunnerError::InvalidState(RUN_IN_FLIGHT_MESSAGE));
        }
        if state.mmio_dispatch_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                MMIO_DISPATCH_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.boot_register_setup_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                BOOT_REGISTER_SETUP_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.metadata_read_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                METADATA_READ_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.core_register_capture_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.timer_operation_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                TIMER_OPERATION_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.interrupt_operation_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                INTERRUPT_OPERATION_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.boot_register_setup_failed {
            return Err(HvfVcpuRunnerError::InvalidState(
                BOOT_REGISTER_SETUP_FAILED_MESSAGE,
            ));
        }

        state.in_flight_runs = 1;
        if self
            .command_sender
            .send(RunnerCommand::RunOnce { response_sender })
            .is_err()
        {
            state.in_flight_runs = 0;
            return Err(HvfVcpuRunnerError::ChannelClosed(
                COMMAND_CHANNEL_CLOSED_MESSAGE,
            ));
        }

        state.run_started = true;

        Ok(InFlightRun::new(&self.state))
    }

    fn start_run_once_and_handle_mmio(
        &self,
        dispatcher: SharedMmioDispatcher,
        response_sender: mpsc::Sender<Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError>>,
    ) -> Result<InFlightRun, HvfVcpuRunnerError> {
        let mut state = self.lock_state()?;
        if state.thread.is_none() || state.shutting_down {
            return Err(HvfVcpuRunnerError::InvalidState(RUNNER_SHUT_DOWN_MESSAGE));
        }
        if state.in_flight_runs > 0 {
            return Err(HvfVcpuRunnerError::InvalidState(RUN_IN_FLIGHT_MESSAGE));
        }
        if state.mmio_dispatch_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                MMIO_DISPATCH_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.boot_register_setup_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                BOOT_REGISTER_SETUP_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.metadata_read_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                METADATA_READ_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.core_register_capture_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.timer_operation_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                TIMER_OPERATION_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.interrupt_operation_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                INTERRUPT_OPERATION_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.boot_register_setup_failed {
            return Err(HvfVcpuRunnerError::InvalidState(
                BOOT_REGISTER_SETUP_FAILED_MESSAGE,
            ));
        }

        state.in_flight_runs = 1;
        if self
            .command_sender
            .send(RunnerCommand::RunOnceAndHandleMmio {
                dispatcher,
                response_sender,
            })
            .is_err()
        {
            state.in_flight_runs = 0;
            return Err(HvfVcpuRunnerError::ChannelClosed(
                COMMAND_CHANNEL_CLOSED_MESSAGE,
            ));
        }

        state.run_started = true;

        Ok(InFlightRun::new(&self.state))
    }

    fn start_mmio_dispatch(
        &self,
        access: HvfResolvedMmioAccess,
        dispatcher: SharedMmioDispatcher,
        response_sender: mpsc::Sender<Result<MmioDispatchOutcome, HvfVcpuRunnerError>>,
    ) -> Result<InFlightMmioDispatch, HvfVcpuRunnerError> {
        let mut state = self.lock_state()?;
        if state.thread.is_none() {
            return Err(HvfVcpuRunnerError::InvalidState(RUNNER_SHUT_DOWN_MESSAGE));
        }
        if state.shutting_down {
            return Err(HvfVcpuRunnerError::InvalidState(
                RUNNER_SHUTTING_DOWN_MESSAGE,
            ));
        }
        if state.in_flight_runs > 0 {
            return Err(HvfVcpuRunnerError::InvalidState(RUN_IN_FLIGHT_MESSAGE));
        }
        if state.boot_register_setup_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                BOOT_REGISTER_SETUP_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.metadata_read_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                METADATA_READ_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.core_register_capture_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.timer_operation_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                TIMER_OPERATION_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.interrupt_operation_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                INTERRUPT_OPERATION_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.boot_register_setup_failed {
            return Err(HvfVcpuRunnerError::InvalidState(
                BOOT_REGISTER_SETUP_FAILED_MESSAGE,
            ));
        }
        if !state.run_started {
            return Err(HvfVcpuRunnerError::InvalidState(RUN_NOT_STARTED_MESSAGE));
        }
        if state.mmio_dispatch_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                MMIO_DISPATCH_IN_FLIGHT_MESSAGE,
            ));
        }

        state.mmio_dispatch_in_flight = true;
        if self
            .command_sender
            .send(RunnerCommand::DispatchMmioAccess {
                access,
                dispatcher,
                response_sender,
            })
            .is_err()
        {
            state.mmio_dispatch_in_flight = false;
            return Err(HvfVcpuRunnerError::ChannelClosed(
                COMMAND_CHANNEL_CLOSED_MESSAGE,
            ));
        }

        Ok(InFlightMmioDispatch::new(&self.state))
    }

    fn start_mpidr_el1_read(
        &self,
        response_sender: mpsc::Sender<Result<u64, HvfVcpuRunnerError>>,
    ) -> Result<InFlightMetadataRead, HvfVcpuRunnerError> {
        let mut state = self.lock_state()?;
        if state.thread.is_none() {
            return Err(HvfVcpuRunnerError::InvalidState(RUNNER_SHUT_DOWN_MESSAGE));
        }
        if state.shutting_down {
            return Err(HvfVcpuRunnerError::InvalidState(
                RUNNER_SHUTTING_DOWN_MESSAGE,
            ));
        }
        if state.in_flight_runs > 0 {
            return Err(HvfVcpuRunnerError::InvalidState(RUN_IN_FLIGHT_MESSAGE));
        }
        if state.mmio_dispatch_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                MMIO_DISPATCH_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.boot_register_setup_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                BOOT_REGISTER_SETUP_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.metadata_read_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                METADATA_READ_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.core_register_capture_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.timer_operation_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                TIMER_OPERATION_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.interrupt_operation_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                INTERRUPT_OPERATION_IN_FLIGHT_MESSAGE,
            ));
        }

        state.metadata_read_in_flight = true;
        if self
            .command_sender
            .send(RunnerCommand::ReadMpidrEl1 { response_sender })
            .is_err()
        {
            state.metadata_read_in_flight = false;
            return Err(HvfVcpuRunnerError::ChannelClosed(
                COMMAND_CHANNEL_CLOSED_MESSAGE,
            ));
        }

        Ok(InFlightMetadataRead::new(&self.state))
    }

    fn start_arm64_general_register_capture(
        &self,
        response_sender: mpsc::Sender<Result<HvfArm64VcpuGeneralRegisterState, HvfVcpuRunnerError>>,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.start_core_register_capture(
            |admission, response_sender| RunnerCommand::CaptureArm64GeneralRegisterState {
                admission,
                response_sender,
            },
            response_sender,
        )
    }

    fn start_arm64_core_system_register_capture(
        &self,
        response_sender: mpsc::Sender<
            Result<HvfArm64VcpuCoreSystemRegisterState, HvfVcpuRunnerError>,
        >,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.start_core_register_capture(
            |admission, response_sender| RunnerCommand::CaptureArm64CoreSystemRegisterState {
                admission,
                response_sender,
            },
            response_sender,
        )
    }

    fn start_arm64_exception_register_capture(
        &self,
        response_sender: mpsc::Sender<
            Result<HvfArm64VcpuExceptionRegisterState, HvfVcpuRunnerError>,
        >,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.start_core_register_capture(
            |admission, response_sender| RunnerCommand::CaptureArm64ExceptionRegisterState {
                admission,
                response_sender,
            },
            response_sender,
        )
    }

    fn start_arm64_execution_control_register_capture(
        &self,
        response_sender: mpsc::Sender<
            Result<HvfArm64VcpuExecutionControlRegisterState, HvfVcpuRunnerError>,
        >,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.start_core_register_capture(
            |admission, response_sender| RunnerCommand::CaptureArm64ExecutionControlRegisterState {
                admission,
                response_sender,
            },
            response_sender,
        )
    }

    fn start_arm64_identification_register_capture(
        &self,
        response_sender: mpsc::Sender<
            Result<HvfArm64VcpuIdentificationRegisterState, HvfVcpuRunnerError>,
        >,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.start_core_register_capture(
            |admission, response_sender| RunnerCommand::CaptureArm64IdentificationRegisterState {
                admission,
                response_sender,
            },
            response_sender,
        )
    }

    fn start_arm64_thread_context_register_capture(
        &self,
        response_sender: mpsc::Sender<
            Result<HvfArm64VcpuThreadContextRegisterState, HvfVcpuRunnerError>,
        >,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.start_core_register_capture(
            |admission, response_sender| RunnerCommand::CaptureArm64ThreadContextRegisterState {
                admission,
                response_sender,
            },
            response_sender,
        )
    }

    fn start_arm64_simd_fp_capture(
        &self,
        response_sender: mpsc::Sender<Result<HvfArm64VcpuSimdFpState, HvfVcpuRunnerError>>,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.start_core_register_capture(
            |admission, response_sender| RunnerCommand::CaptureArm64SimdFpState {
                admission,
                response_sender,
            },
            response_sender,
        )
    }

    fn start_core_register_capture<T>(
        &self,
        command: impl FnOnce(
            InFlightCoreRegisterCapture,
            mpsc::Sender<Result<T, HvfVcpuRunnerError>>,
        ) -> RunnerCommand,
        response_sender: mpsc::Sender<Result<T, HvfVcpuRunnerError>>,
    ) -> Result<(), HvfVcpuRunnerError> {
        // Capture admission follows queued owner-thread work rather than the
        // caller's response lifetime, so abandoning the response cannot admit
        // cancellation while register reads are still active.
        let admission = self.reserve_core_register_capture()?;

        // Reservation returns after releasing the state lock. If the command
        // channel is closed, dropping the unsent command can therefore lock
        // the state through its admission guard and restore the capture bit.
        self.command_sender
            .send(command(admission, response_sender))
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(COMMAND_CHANNEL_CLOSED_MESSAGE))
    }

    fn start_arm64_translation_register_capture(
        &self,
        response_sender: mpsc::Sender<
            Result<HvfArm64VcpuTranslationRegisterState, HvfVcpuRunnerError>,
        >,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.start_core_register_capture(
            |admission, response_sender| RunnerCommand::CaptureArm64TranslationRegisterState {
                admission,
                response_sender,
            },
            response_sender,
        )
    }

    fn start_arm64_pointer_authentication_key_capture(
        &self,
        response_sender: mpsc::Sender<
            Result<HvfArm64VcpuPointerAuthenticationKeyState, HvfVcpuRunnerError>,
        >,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.start_core_register_capture(
            |admission, response_sender| RunnerCommand::CaptureArm64PointerAuthenticationKeyState {
                admission,
                response_sender,
            },
            response_sender,
        )
    }

    fn reserve_core_register_capture(
        &self,
    ) -> Result<InFlightCoreRegisterCapture, HvfVcpuRunnerError> {
        let mut state = self.lock_state()?;
        if state.thread.is_none() {
            return Err(HvfVcpuRunnerError::InvalidState(RUNNER_SHUT_DOWN_MESSAGE));
        }
        if state.shutting_down {
            return Err(HvfVcpuRunnerError::InvalidState(
                RUNNER_SHUTTING_DOWN_MESSAGE,
            ));
        }
        if state.in_flight_runs > 0 {
            return Err(HvfVcpuRunnerError::InvalidState(RUN_IN_FLIGHT_MESSAGE));
        }
        if state.mmio_dispatch_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                MMIO_DISPATCH_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.boot_register_setup_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                BOOT_REGISTER_SETUP_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.metadata_read_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                METADATA_READ_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.core_register_capture_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.timer_operation_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                TIMER_OPERATION_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.interrupt_operation_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                INTERRUPT_OPERATION_IN_FLIGHT_MESSAGE,
            ));
        }

        state.core_register_capture_in_flight = true;
        Ok(InFlightCoreRegisterCapture::new(&self.state))
    }

    fn start_get_vtimer_mask(
        &self,
        response_sender: mpsc::Sender<Result<bool, HvfVcpuRunnerError>>,
    ) -> Result<InFlightTimerOperation, HvfVcpuRunnerError> {
        self.start_timer_operation(
            |response_sender| RunnerCommand::GetVtimerMask { response_sender },
            response_sender,
        )
    }

    fn start_set_vtimer_mask(
        &self,
        masked: bool,
        response_sender: mpsc::Sender<Result<(), HvfVcpuRunnerError>>,
    ) -> Result<InFlightTimerOperation, HvfVcpuRunnerError> {
        self.start_timer_operation(
            |response_sender| RunnerCommand::SetVtimerMask {
                masked,
                response_sender,
            },
            response_sender,
        )
    }

    fn start_get_vtimer_offset(
        &self,
        response_sender: mpsc::Sender<Result<u64, HvfVcpuRunnerError>>,
    ) -> Result<InFlightTimerOperation, HvfVcpuRunnerError> {
        self.start_timer_operation(
            |response_sender| RunnerCommand::GetVtimerOffset { response_sender },
            response_sender,
        )
    }

    fn start_set_vtimer_offset(
        &self,
        offset: u64,
        response_sender: mpsc::Sender<Result<(), HvfVcpuRunnerError>>,
    ) -> Result<InFlightTimerOperation, HvfVcpuRunnerError> {
        self.start_timer_operation(
            |response_sender| RunnerCommand::SetVtimerOffset {
                offset,
                response_sender,
            },
            response_sender,
        )
    }

    fn start_get_vtimer_control(
        &self,
        response_sender: mpsc::Sender<Result<u64, HvfVcpuRunnerError>>,
    ) -> Result<InFlightTimerOperation, HvfVcpuRunnerError> {
        self.start_timer_operation(
            |response_sender| RunnerCommand::GetVtimerControl { response_sender },
            response_sender,
        )
    }

    fn start_set_vtimer_control(
        &self,
        control: u64,
        response_sender: mpsc::Sender<Result<(), HvfVcpuRunnerError>>,
    ) -> Result<InFlightTimerOperation, HvfVcpuRunnerError> {
        self.start_timer_operation(
            |response_sender| RunnerCommand::SetVtimerControl {
                control,
                response_sender,
            },
            response_sender,
        )
    }

    fn start_get_vtimer_compare_value(
        &self,
        response_sender: mpsc::Sender<Result<u64, HvfVcpuRunnerError>>,
    ) -> Result<InFlightTimerOperation, HvfVcpuRunnerError> {
        self.start_timer_operation(
            |response_sender| RunnerCommand::GetVtimerCompareValue { response_sender },
            response_sender,
        )
    }

    fn start_set_vtimer_compare_value(
        &self,
        compare_value: u64,
        response_sender: mpsc::Sender<Result<(), HvfVcpuRunnerError>>,
    ) -> Result<InFlightTimerOperation, HvfVcpuRunnerError> {
        self.start_timer_operation(
            |response_sender| RunnerCommand::SetVtimerCompareValue {
                compare_value,
                response_sender,
            },
            response_sender,
        )
    }

    fn start_arm64_virtual_timer_capture(
        &self,
        response_sender: mpsc::Sender<Result<HvfArm64VcpuVirtualTimerState, HvfVcpuRunnerError>>,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.start_timer_capture(
            |admission, response_sender| RunnerCommand::CaptureArm64VirtualTimerState {
                admission,
                response_sender,
            },
            response_sender,
        )
    }

    fn start_arm64_physical_timer_capture(
        &self,
        response_sender: mpsc::Sender<Result<HvfArm64VcpuPhysicalTimerState, HvfVcpuRunnerError>>,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.start_timer_capture(
            |admission, response_sender| RunnerCommand::CaptureArm64PhysicalTimerState {
                admission,
                response_sender,
            },
            response_sender,
        )
    }

    fn start_timer_capture<T>(
        &self,
        command: impl FnOnce(
            InFlightTimerOperation,
            mpsc::Sender<Result<T, HvfVcpuRunnerError>>,
        ) -> RunnerCommand,
        response_sender: mpsc::Sender<Result<T, HvfVcpuRunnerError>>,
    ) -> Result<(), HvfVcpuRunnerError> {
        // Aggregate capture admission follows queued owner-thread work rather
        // than the caller's response lifetime.
        let admission = {
            let _state = self.reserve_timer_operation()?;
            InFlightTimerOperation::new(&self.state)
        };

        // Release the state lock before send so a rejected command can drop
        // its admission guard without recursively acquiring the same lock.
        self.command_sender
            .send(command(admission, response_sender))
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(COMMAND_CHANNEL_CLOSED_MESSAGE))
    }

    fn start_timer_operation<T>(
        &self,
        command: impl FnOnce(mpsc::Sender<Result<T, HvfVcpuRunnerError>>) -> RunnerCommand,
        response_sender: mpsc::Sender<Result<T, HvfVcpuRunnerError>>,
    ) -> Result<InFlightTimerOperation, HvfVcpuRunnerError> {
        let mut state = self.reserve_timer_operation()?;
        if self.command_sender.send(command(response_sender)).is_err() {
            state.timer_operation_in_flight = false;
            return Err(HvfVcpuRunnerError::ChannelClosed(
                COMMAND_CHANNEL_CLOSED_MESSAGE,
            ));
        }

        Ok(InFlightTimerOperation::new(&self.state))
    }

    fn reserve_timer_operation(
        &self,
    ) -> Result<MutexGuard<'_, RunnerHandleState>, HvfVcpuRunnerError> {
        let mut state = self.lock_state()?;
        if state.thread.is_none() {
            return Err(HvfVcpuRunnerError::InvalidState(RUNNER_SHUT_DOWN_MESSAGE));
        }
        if state.shutting_down {
            return Err(HvfVcpuRunnerError::InvalidState(
                RUNNER_SHUTTING_DOWN_MESSAGE,
            ));
        }
        if state.in_flight_runs > 0 {
            return Err(HvfVcpuRunnerError::InvalidState(RUN_IN_FLIGHT_MESSAGE));
        }
        if state.mmio_dispatch_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                MMIO_DISPATCH_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.boot_register_setup_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                BOOT_REGISTER_SETUP_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.metadata_read_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                METADATA_READ_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.core_register_capture_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.timer_operation_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                TIMER_OPERATION_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.interrupt_operation_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                INTERRUPT_OPERATION_IN_FLIGHT_MESSAGE,
            ));
        }

        state.timer_operation_in_flight = true;
        Ok(state)
    }

    fn start_gic_ppi_pending_operation(
        &self,
        intid: u32,
        pending: bool,
        response_sender: mpsc::Sender<Result<(), HvfVcpuRunnerError>>,
    ) -> Result<InFlightInterruptOperation, HvfVcpuRunnerError> {
        self.start_interrupt_operation(
            || validate_gic_ppi_pending_intid(intid).map_err(HvfVcpuRunnerError::Gic),
            |response_sender| RunnerCommand::SetGicPpiPending {
                intid,
                pending,
                response_sender,
            },
            response_sender,
        )
    }

    fn start_get_pending_interrupt(
        &self,
        interrupt_type: HvfInterruptType,
        response_sender: mpsc::Sender<Result<bool, HvfVcpuRunnerError>>,
    ) -> Result<InFlightInterruptOperation, HvfVcpuRunnerError> {
        self.start_interrupt_operation(
            || Ok(()),
            |response_sender| RunnerCommand::GetPendingInterrupt {
                interrupt_type,
                response_sender,
            },
            response_sender,
        )
    }

    fn start_set_pending_interrupt(
        &self,
        interrupt_type: HvfInterruptType,
        pending: bool,
        response_sender: mpsc::Sender<Result<(), HvfVcpuRunnerError>>,
    ) -> Result<InFlightInterruptOperation, HvfVcpuRunnerError> {
        self.start_interrupt_operation(
            || Ok(()),
            |response_sender| RunnerCommand::SetPendingInterrupt {
                interrupt_type,
                pending,
                response_sender,
            },
            response_sender,
        )
    }

    fn start_arm64_pending_interrupt_capture(
        &self,
        response_sender: mpsc::Sender<
            Result<HvfArm64VcpuPendingInterruptState, HvfVcpuRunnerError>,
        >,
    ) -> Result<(), HvfVcpuRunnerError> {
        let admission = {
            let _state = self.reserve_interrupt_operation(|| Ok(()))?;
            InFlightInterruptOperation::new(&self.state)
        };

        self.command_sender
            .send(RunnerCommand::CaptureArm64PendingInterruptState {
                admission,
                response_sender,
            })
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(COMMAND_CHANNEL_CLOSED_MESSAGE))
    }

    fn start_gic_device_state_capture(
        &self,
        response_sender: mpsc::Sender<Result<HvfGicDeviceState, HvfVcpuRunnerError>>,
    ) -> Result<(), HvfVcpuRunnerError> {
        let admission = {
            let _state = self.reserve_interrupt_operation(|| Ok(()))?;
            InFlightInterruptOperation::new(&self.state)
        };

        self.command_sender
            .send(RunnerCommand::CaptureGicDeviceState {
                admission,
                response_sender,
            })
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(COMMAND_CHANNEL_CLOSED_MESSAGE))
    }

    fn start_arm64_gic_icc_register_state_capture(
        &self,
        response_sender: mpsc::Sender<Result<HvfArm64GicIccRegisterState, HvfVcpuRunnerError>>,
    ) -> Result<(), HvfVcpuRunnerError> {
        let admission = {
            let _state = self.reserve_interrupt_operation(|| Ok(()))?;
            InFlightInterruptOperation::new(&self.state)
        };

        self.command_sender
            .send(RunnerCommand::CaptureArm64GicIccRegisterState {
                admission,
                response_sender,
            })
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(COMMAND_CHANNEL_CLOSED_MESSAGE))
    }

    fn start_interrupt_operation<T>(
        &self,
        validate: impl FnOnce() -> Result<(), HvfVcpuRunnerError>,
        command: impl FnOnce(mpsc::Sender<Result<T, HvfVcpuRunnerError>>) -> RunnerCommand,
        response_sender: mpsc::Sender<Result<T, HvfVcpuRunnerError>>,
    ) -> Result<InFlightInterruptOperation, HvfVcpuRunnerError> {
        let mut state = self.reserve_interrupt_operation(validate)?;
        if self.command_sender.send(command(response_sender)).is_err() {
            state.interrupt_operation_in_flight = false;
            return Err(HvfVcpuRunnerError::ChannelClosed(
                COMMAND_CHANNEL_CLOSED_MESSAGE,
            ));
        }

        Ok(InFlightInterruptOperation::new(&self.state))
    }

    fn reserve_interrupt_operation(
        &self,
        validate: impl FnOnce() -> Result<(), HvfVcpuRunnerError>,
    ) -> Result<MutexGuard<'_, RunnerHandleState>, HvfVcpuRunnerError> {
        let mut state = self.lock_state()?;
        if state.thread.is_none() {
            return Err(HvfVcpuRunnerError::InvalidState(RUNNER_SHUT_DOWN_MESSAGE));
        }
        if state.shutting_down {
            return Err(HvfVcpuRunnerError::InvalidState(
                RUNNER_SHUTTING_DOWN_MESSAGE,
            ));
        }
        if state.in_flight_runs > 0 {
            return Err(HvfVcpuRunnerError::InvalidState(RUN_IN_FLIGHT_MESSAGE));
        }
        if state.mmio_dispatch_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                MMIO_DISPATCH_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.boot_register_setup_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                BOOT_REGISTER_SETUP_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.metadata_read_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                METADATA_READ_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.core_register_capture_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.timer_operation_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                TIMER_OPERATION_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.interrupt_operation_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                INTERRUPT_OPERATION_IN_FLIGHT_MESSAGE,
            ));
        }
        validate()?;

        state.interrupt_operation_in_flight = true;
        Ok(state)
    }

    fn prepare_shutdown(&self) -> Result<(mpsc::Sender<RunnerCommand>, bool), HvfVcpuRunnerError> {
        let mut state = self.lock_state()?;
        if state.shutting_down {
            return Err(HvfVcpuRunnerError::InvalidState(
                RUNNER_SHUTTING_DOWN_MESSAGE,
            ));
        }
        if state.thread.is_none() {
            return Err(HvfVcpuRunnerError::InvalidState(RUNNER_SHUT_DOWN_MESSAGE));
        }
        if state.metadata_read_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                METADATA_READ_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.core_register_capture_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.timer_operation_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                TIMER_OPERATION_IN_FLIGHT_MESSAGE,
            ));
        }
        if state.interrupt_operation_in_flight {
            return Err(HvfVcpuRunnerError::InvalidState(
                INTERRUPT_OPERATION_IN_FLIGHT_MESSAGE,
            ));
        }

        state.shutting_down = true;

        Ok((self.command_sender.clone(), state.in_flight_runs > 0))
    }

    fn cancel_shutdown(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.shutting_down = false;
        }
    }

    fn finish_shutdown(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.shutting_down = false;
        }
    }

    fn cancel_vcpu(&self) -> Result<(), HvfVcpuRunnerError> {
        cancel_vcpu(self.vcpu, &self.cancel_vcpu)
    }

    fn take_thread(&self) -> Result<Option<JoinHandle<()>>, HvfVcpuRunnerError> {
        Ok(self.lock_state()?.thread.take())
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, RunnerHandleState>, HvfVcpuRunnerError> {
        lock_runner_state(&self.state)
    }
}

fn prepare_cancel(
    state: &RunnerState,
) -> Result<MutexGuard<'_, RunnerHandleState>, HvfVcpuRunnerError> {
    let state = lock_runner_state(state)?;
    if state.thread.is_none() {
        return Err(HvfVcpuRunnerError::InvalidState(RUNNER_SHUT_DOWN_MESSAGE));
    }
    if state.shutting_down {
        return Err(HvfVcpuRunnerError::InvalidState(
            RUNNER_SHUTTING_DOWN_MESSAGE,
        ));
    }
    if state.mmio_dispatch_in_flight {
        return Err(HvfVcpuRunnerError::InvalidState(
            MMIO_DISPATCH_IN_FLIGHT_MESSAGE,
        ));
    }
    if state.metadata_read_in_flight {
        return Err(HvfVcpuRunnerError::InvalidState(
            METADATA_READ_IN_FLIGHT_MESSAGE,
        ));
    }
    if state.core_register_capture_in_flight {
        return Err(HvfVcpuRunnerError::InvalidState(
            CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE,
        ));
    }
    if state.timer_operation_in_flight {
        return Err(HvfVcpuRunnerError::InvalidState(
            TIMER_OPERATION_IN_FLIGHT_MESSAGE,
        ));
    }
    if state.interrupt_operation_in_flight {
        return Err(HvfVcpuRunnerError::InvalidState(
            INTERRUPT_OPERATION_IN_FLIGHT_MESSAGE,
        ));
    }
    Ok(state)
}

fn cancel_vcpu(
    vcpu: crate::ffi::HvVcpu,
    cancel_vcpu: &CancelVcpu,
) -> Result<(), HvfVcpuRunnerError> {
    cancel_vcpu(vcpu).map_err(HvfVcpuRunnerError::Backend)
}

fn lock_runner_state(
    state: &RunnerState,
) -> Result<MutexGuard<'_, RunnerHandleState>, HvfVcpuRunnerError> {
    state
        .lock()
        .map_err(|_| HvfVcpuRunnerError::InvalidState(RUNNER_STATE_POISONED_MESSAGE))
}

impl Drop for HvfVcpuRunner<'_> {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

impl fmt::Debug for HvfVcpuRunner<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.lock().map(|state| {
            (
                state.thread.is_some(),
                state.shutting_down,
                state.in_flight_runs,
                state.mmio_dispatch_in_flight,
                state.boot_register_setup_in_flight,
                state.metadata_read_in_flight,
                state.core_register_capture_in_flight,
                state.timer_operation_in_flight,
                state.interrupt_operation_in_flight,
                state.boot_register_setup_failed,
                state.boot_registers_configured,
                state.run_started,
            )
        });

        match state {
            Ok((
                active,
                shutting_down,
                in_flight_runs,
                mmio_dispatch_in_flight,
                boot_register_setup_in_flight,
                metadata_read_in_flight,
                core_register_capture_in_flight,
                timer_operation_in_flight,
                interrupt_operation_in_flight,
                boot_register_setup_failed,
                boot_registers_configured,
                run_started,
            )) => f
                .debug_struct("HvfVcpuRunner")
                .field("active", &active)
                .field("shutting_down", &shutting_down)
                .field("in_flight_runs", &in_flight_runs)
                .field("mmio_dispatch_in_flight", &mmio_dispatch_in_flight)
                .field(
                    "boot_register_setup_in_flight",
                    &boot_register_setup_in_flight,
                )
                .field("metadata_read_in_flight", &metadata_read_in_flight)
                .field(
                    "core_register_capture_in_flight",
                    &core_register_capture_in_flight,
                )
                .field("timer_operation_in_flight", &timer_operation_in_flight)
                .field(
                    "interrupt_operation_in_flight",
                    &interrupt_operation_in_flight,
                )
                .field("boot_register_setup_failed", &boot_register_setup_failed)
                .field("boot_registers_configured", &boot_registers_configured)
                .field("run_started", &run_started)
                .finish_non_exhaustive(),
            Err(_) => f
                .debug_struct("HvfVcpuRunner")
                .field("state", &RUNNER_STATE_POISONED_MESSAGE)
                .finish_non_exhaustive(),
        }
    }
}

struct InFlightBootRegisterSetup {
    state: RunnerState,
    configured: bool,
    failed: bool,
}

impl InFlightBootRegisterSetup {
    fn new(state: &RunnerState) -> Self {
        Self {
            state: Arc::clone(state),
            configured: false,
            failed: false,
        }
    }

    fn mark_configured(&mut self) {
        self.configured = true;
    }

    fn mark_failed(&mut self) {
        self.failed = true;
    }
}

impl Drop for InFlightBootRegisterSetup {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.boot_register_setup_in_flight = false;
            if self.configured {
                state.boot_register_setup_failed = false;
                state.boot_registers_configured = true;
            } else if self.failed {
                state.boot_register_setup_failed = true;
            }
        }
    }
}

struct InFlightRun {
    state: RunnerState,
}

impl InFlightRun {
    fn new(state: &RunnerState) -> Self {
        Self {
            state: Arc::clone(state),
        }
    }
}

impl Drop for InFlightRun {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.in_flight_runs = state.in_flight_runs.saturating_sub(1);
        }
    }
}

struct InFlightMmioDispatch {
    state: RunnerState,
}

impl InFlightMmioDispatch {
    fn new(state: &RunnerState) -> Self {
        Self {
            state: Arc::clone(state),
        }
    }
}

impl Drop for InFlightMmioDispatch {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.mmio_dispatch_in_flight = false;
        }
    }
}

fn real_cancel_vcpu() -> CancelVcpu {
    Arc::new(|vcpu| {
        let mut vcpus = [vcpu];
        crate::ffi::exit_vcpus(&mut vcpus)
    })
}

struct InFlightMetadataRead {
    state: RunnerState,
}

impl InFlightMetadataRead {
    fn new(state: &RunnerState) -> Self {
        Self {
            state: Arc::clone(state),
        }
    }
}

impl Drop for InFlightMetadataRead {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.metadata_read_in_flight = false;
        }
    }
}

/// Command-owned admission for one core-register capture command.
struct InFlightCoreRegisterCapture {
    state: Option<RunnerState>,
}

impl InFlightCoreRegisterCapture {
    fn new(state: &RunnerState) -> Self {
        Self {
            state: Some(Arc::clone(state)),
        }
    }

    fn release(&mut self) {
        let Some(state) = self.state.take() else {
            return;
        };
        if let Ok(mut state) = state.lock() {
            state.core_register_capture_in_flight = false;
        }
    }
}

impl Drop for InFlightCoreRegisterCapture {
    fn drop(&mut self) {
        self.release();
    }
}

struct InFlightTimerOperation {
    state: Option<RunnerState>,
}

impl InFlightTimerOperation {
    fn new(state: &RunnerState) -> Self {
        Self {
            state: Some(Arc::clone(state)),
        }
    }

    fn release(&mut self) {
        let Some(state) = self.state.take() else {
            return;
        };
        if let Ok(mut state) = state.lock() {
            state.timer_operation_in_flight = false;
        }
    }
}

impl Drop for InFlightTimerOperation {
    fn drop(&mut self) {
        self.release();
    }
}

struct InFlightInterruptOperation {
    state: Option<RunnerState>,
}

impl InFlightInterruptOperation {
    fn new(state: &RunnerState) -> Self {
        Self {
            state: Some(Arc::clone(state)),
        }
    }

    fn release(&mut self) {
        let Some(state) = self.state.take() else {
            return;
        };
        if let Ok(mut state) = state.lock() {
            state.interrupt_operation_in_flight = false;
        }
    }
}

impl Drop for InFlightInterruptOperation {
    fn drop(&mut self) {
        self.release();
    }
}

fn spawn_runner_thread<C, V>(create_vcpu: C) -> Result<StartedRunner, HvfVcpuRunnerError>
where
    C: FnOnce() -> Result<V, BackendError> + Send + 'static,
    V: RunnerVcpu + 'static,
{
    let (command_sender, command_receiver) = mpsc::channel();
    let (startup_sender, startup_receiver) = mpsc::channel();
    let thread = thread::Builder::new()
        .name("bangbang-hvf-vcpu".to_string())
        .spawn(move || run_runner_thread(command_receiver, startup_sender, create_vcpu))
        .map_err(|err| HvfVcpuRunnerError::ThreadSpawn(err.to_string()))?;

    let startup_result = match startup_receiver.recv() {
        Ok(startup_result) => startup_result,
        Err(_) => {
            join_runner_thread(Some(thread))?;
            return Err(HvfVcpuRunnerError::ChannelClosed(
                RESPONSE_CHANNEL_CLOSED_MESSAGE,
            ));
        }
    };

    match startup_result {
        Ok(vcpu) => Ok(StartedRunner {
            command_sender,
            vcpu,
            thread,
        }),
        Err(err) => {
            join_runner_thread(Some(thread))?;
            Err(err)
        }
    }
}

fn run_runner_thread<C, V>(
    command_receiver: mpsc::Receiver<RunnerCommand>,
    startup_sender: mpsc::Sender<Result<crate::ffi::HvVcpu, HvfVcpuRunnerError>>,
    create_vcpu: C,
) where
    C: FnOnce() -> Result<V, BackendError>,
    V: RunnerVcpu,
{
    let mut vcpu = match create_vcpu() {
        Ok(vcpu) => vcpu,
        Err(err) => {
            let _ = startup_sender.send(Err(HvfVcpuRunnerError::Backend(err)));
            return;
        }
    };

    let vcpu_id = match vcpu.raw_vcpu() {
        Ok(vcpu_id) => vcpu_id,
        Err(err) => {
            let _ = startup_sender.send(Err(HvfVcpuRunnerError::Backend(err)));
            return;
        }
    };

    if startup_sender.send(Ok(vcpu_id)).is_err() {
        return;
    }

    while let Ok(command) = command_receiver.recv() {
        match command {
            RunnerCommand::ConfigureArm64BootRegisters {
                registers,
                response_sender,
            } => {
                let result = vcpu
                    .configure_arm64_boot_registers(registers)
                    .map_err(HvfVcpuRunnerError::Backend);
                let _ = response_sender.send(result);
            }
            RunnerCommand::RunOnce { response_sender } => {
                let result = vcpu.run_once().map_err(HvfVcpuRunnerError::Backend);
                let _ = response_sender.send(result);
            }
            RunnerCommand::RunOnceAndHandleMmio {
                dispatcher,
                response_sender,
            } => {
                let result = run_once_and_handle_mmio_on_runner_thread(&mut vcpu, &dispatcher);
                let _ = response_sender.send(result);
            }
            RunnerCommand::DispatchMmioAccess {
                access,
                dispatcher,
                response_sender,
            } => {
                let result = dispatch_mmio_access_on_runner_thread(&mut vcpu, access, &dispatcher);
                let _ = response_sender.send(result);
            }
            RunnerCommand::ReadMpidrEl1 { response_sender } => {
                let result = vcpu.mpidr_el1().map_err(HvfVcpuRunnerError::Backend);
                let _ = response_sender.send(result);
            }
            RunnerCommand::CaptureArm64GeneralRegisterState {
                mut admission,
                response_sender,
            } => {
                let result = vcpu
                    .capture_arm64_general_register_state()
                    .map_err(HvfVcpuRunnerError::Backend);
                // The last owner-thread read has finished. Restore admission
                // before responding so even a dropped receiver is not part of
                // the capture lifetime.
                admission.release();
                let _ = response_sender.send(result);
            }
            RunnerCommand::CaptureArm64CoreSystemRegisterState {
                mut admission,
                response_sender,
            } => {
                let result = vcpu
                    .capture_arm64_core_system_register_state()
                    .map_err(HvfVcpuRunnerError::Backend);
                // The last owner-thread read has finished. Restore admission
                // before responding so even a dropped receiver is not part of
                // the capture lifetime.
                admission.release();
                let _ = response_sender.send(result);
            }
            RunnerCommand::CaptureArm64ExceptionRegisterState {
                mut admission,
                response_sender,
            } => {
                let result = vcpu
                    .capture_arm64_exception_register_state()
                    .map_err(HvfVcpuRunnerError::Backend);
                // All owner-thread exception-register reads have finished.
                // Restore admission before response send so receiver failure
                // is not part of the capture lifetime.
                admission.release();
                let _ = response_sender.send(result);
            }
            RunnerCommand::CaptureArm64ExecutionControlRegisterState {
                mut admission,
                response_sender,
            } => {
                let result = vcpu
                    .capture_arm64_execution_control_register_state()
                    .map_err(HvfVcpuRunnerError::Backend);
                // Both owner-thread execution-control reads have finished.
                // Restore admission before response send so receiver failure
                // is not part of the capture lifetime.
                admission.release();
                let _ = response_sender.send(result);
            }
            RunnerCommand::CaptureArm64IdentificationRegisterState {
                mut admission,
                response_sender,
            } => {
                let result = vcpu
                    .capture_arm64_identification_register_state()
                    .map_err(HvfVcpuRunnerError::Backend);
                // All owner-thread identification reads have finished.
                // Restore admission before response send so receiver failure
                // is not part of the capture lifetime.
                admission.release();
                let _ = response_sender.send(result);
            }
            RunnerCommand::CaptureArm64TranslationRegisterState {
                mut admission,
                response_sender,
            } => {
                let result = vcpu
                    .capture_arm64_translation_register_state()
                    .map_err(HvfVcpuRunnerError::Backend);
                // All owner-thread translation-register reads have finished.
                // Restore admission before response send so receiver failure
                // is not part of the capture lifetime.
                admission.release();
                let _ = response_sender.send(result);
            }
            RunnerCommand::CaptureArm64PointerAuthenticationKeyState {
                mut admission,
                response_sender,
            } => {
                let result = vcpu
                    .capture_arm64_pointer_authentication_key_state()
                    .map_err(HvfVcpuRunnerError::Backend);
                // All owner-thread key reads have finished. Restore admission
                // before responding so receiver failure is not cleanup.
                admission.release();
                let _ = response_sender.send(result);
            }
            RunnerCommand::CaptureArm64ThreadContextRegisterState {
                mut admission,
                response_sender,
            } => {
                let result = vcpu
                    .capture_arm64_thread_context_register_state()
                    .map_err(HvfVcpuRunnerError::Backend);
                // The last owner-thread read has finished. Restore admission
                // before responding so a dropped receiver is not part of the
                // capture lifetime.
                admission.release();
                let _ = response_sender.send(result);
            }
            RunnerCommand::CaptureArm64SimdFpState {
                mut admission,
                response_sender,
            } => {
                let result = vcpu
                    .capture_arm64_simd_fp_state()
                    .map_err(HvfVcpuRunnerError::Backend);
                // The last owner-thread read has finished. Restore admission
                // before responding so even a dropped receiver is not part of
                // the capture lifetime.
                admission.release();
                let _ = response_sender.send(result);
            }
            RunnerCommand::GetVtimerMask { response_sender } => {
                let result = vcpu.get_vtimer_mask().map_err(HvfVcpuRunnerError::Backend);
                let _ = response_sender.send(result);
            }
            RunnerCommand::SetVtimerMask {
                masked,
                response_sender,
            } => {
                let result = vcpu
                    .set_vtimer_mask(masked)
                    .map_err(HvfVcpuRunnerError::Backend);
                let _ = response_sender.send(result);
            }
            RunnerCommand::GetVtimerOffset { response_sender } => {
                let result = vcpu
                    .get_vtimer_offset()
                    .map_err(HvfVcpuRunnerError::Backend);
                let _ = response_sender.send(result);
            }
            RunnerCommand::SetVtimerOffset {
                offset,
                response_sender,
            } => {
                let result = vcpu
                    .set_vtimer_offset(offset)
                    .map_err(HvfVcpuRunnerError::Backend);
                let _ = response_sender.send(result);
            }
            RunnerCommand::GetVtimerControl { response_sender } => {
                let result = vcpu
                    .get_vtimer_control()
                    .map_err(HvfVcpuRunnerError::Backend);
                let _ = response_sender.send(result);
            }
            RunnerCommand::SetVtimerControl {
                control,
                response_sender,
            } => {
                let result = vcpu
                    .set_vtimer_control(control)
                    .map_err(HvfVcpuRunnerError::Backend);
                let _ = response_sender.send(result);
            }
            RunnerCommand::GetVtimerCompareValue { response_sender } => {
                let result = vcpu
                    .get_vtimer_compare_value()
                    .map_err(HvfVcpuRunnerError::Backend);
                let _ = response_sender.send(result);
            }
            RunnerCommand::SetVtimerCompareValue {
                compare_value,
                response_sender,
            } => {
                let result = vcpu
                    .set_vtimer_compare_value(compare_value)
                    .map_err(HvfVcpuRunnerError::Backend);
                let _ = response_sender.send(result);
            }
            RunnerCommand::CaptureArm64PhysicalTimerState {
                mut admission,
                response_sender,
            } => {
                let result = vcpu
                    .capture_arm64_physical_timer_state()
                    .map_err(HvfVcpuRunnerError::Backend);
                // All owner-thread reads have finished. Restore admission
                // before response send so receiver failure is not cleanup.
                admission.release();
                let _ = response_sender.send(result);
            }
            RunnerCommand::CaptureArm64VirtualTimerState {
                mut admission,
                response_sender,
            } => {
                let result = vcpu
                    .capture_arm64_virtual_timer_state()
                    .map_err(HvfVcpuRunnerError::Backend);
                // All owner-thread reads have finished. Restore admission
                // before response send so receiver failure is not cleanup.
                admission.release();
                let _ = response_sender.send(result);
            }
            RunnerCommand::GetPendingInterrupt {
                interrupt_type,
                response_sender,
            } => {
                let result = vcpu
                    .get_pending_interrupt(interrupt_type)
                    .map_err(HvfVcpuRunnerError::Backend);
                let _ = response_sender.send(result);
            }
            RunnerCommand::SetPendingInterrupt {
                interrupt_type,
                pending,
                response_sender,
            } => {
                let result = vcpu
                    .set_pending_interrupt(interrupt_type, pending)
                    .map_err(HvfVcpuRunnerError::Backend);
                let _ = response_sender.send(result);
            }
            RunnerCommand::CaptureArm64PendingInterruptState {
                mut admission,
                response_sender,
            } => {
                let result = vcpu
                    .capture_arm64_pending_interrupt_state()
                    .map_err(HvfVcpuRunnerError::Backend);
                // Both owner-thread reads have finished. Restore admission
                // before response send so receiver failure is not cleanup.
                admission.release();
                let _ = response_sender.send(result);
            }
            RunnerCommand::CaptureGicDeviceState {
                mut admission,
                response_sender,
            } => {
                let result = vcpu
                    .capture_gic_device_state()
                    .map_err(HvfVcpuRunnerError::Gic);
                // State-object creation, sizing, and data copy have finished.
                // Restore admission before response send so receiver failure
                // is not part of the capture lifetime.
                admission.release();
                let _ = response_sender.send(result);
            }
            RunnerCommand::CaptureArm64GicIccRegisterState {
                mut admission,
                response_sender,
            } => {
                let result = vcpu
                    .capture_arm64_gic_icc_register_state()
                    .map_err(HvfVcpuRunnerError::Gic);
                // Every owner-thread ICC read has finished. Restore admission
                // before response send so receiver failure is not cleanup.
                admission.release();
                let _ = response_sender.send(result);
            }
            RunnerCommand::SetGicPpiPending {
                intid,
                pending,
                response_sender,
            } => {
                let result = vcpu
                    .set_gic_ppi_pending(intid, pending)
                    .map_err(HvfVcpuRunnerError::Gic);
                let _ = response_sender.send(result);
            }
            RunnerCommand::Shutdown { response_sender } => {
                let result = vcpu.destroy().map_err(HvfVcpuRunnerError::Backend);
                let _ = response_sender.send(result);
                return;
            }
        }
    }
}

fn run_once_and_handle_mmio_on_runner_thread(
    vcpu: &mut impl RunnerVcpu,
    dispatcher: &SharedMmioDispatcher,
) -> Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError> {
    match vcpu.run_once().map_err(HvfVcpuRunnerError::Backend)? {
        HvfVcpuExit::Canceled => Ok(HvfVcpuRunStepOutcome::Canceled),
        HvfVcpuExit::VtimerActivated => Ok(HvfVcpuRunStepOutcome::VtimerActivated),
        HvfVcpuExit::Unknown { reason } => Ok(HvfVcpuRunStepOutcome::Unknown { reason }),
        HvfVcpuExit::Exception(exit) => {
            if let Ok(hvc) = exit.decode_hvc() {
                return handle_hvc_on_runner_thread(vcpu, hvc);
            }
            if let Ok(sys64) = exit.decode_sys64() {
                return handle_sys64_on_runner_thread(vcpu, sys64);
            }

            let access = exit
                .decode_mmio_access()
                .map_err(|source| HvfVcpuExitResolveError::MmioDecode { exit, source })?;
            let mut dispatcher = lock_shared_mmio_dispatcher(dispatcher)?;
            let access = access
                .resolve(dispatcher.bus())
                .map_err(|source| HvfVcpuExitResolveError::MmioResolve { source })?;
            let outcome = vcpu.dispatch_mmio_access(access, &mut dispatcher)?;
            advance_arm64_pc(vcpu)?;

            Ok(HvfVcpuRunStepOutcome::Mmio { access, outcome })
        }
    }
}

fn handle_sys64_on_runner_thread(
    vcpu: &mut impl RunnerVcpu,
    exit: HvfSys64Exit,
) -> Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError> {
    if !is_supported_os_lock_sys64_register(exit.register()) {
        return Err(HvfVcpuRunnerError::UnsupportedSys64 { exit });
    }

    if exit.direction() == HvfSys64Direction::Read
        && let Some(register) = HvfRegister::general_purpose(exit.target_register())
    {
        vcpu.write_register(register, 0)
            .map_err(HvfVcpuRunnerError::Backend)?;
    }

    advance_arm64_pc(vcpu)?;

    Ok(HvfVcpuRunStepOutcome::Sys64 { exit })
}

const fn is_supported_os_lock_sys64_register(register: HvfSys64Register) -> bool {
    matches!(
        register,
        HvfSys64Register::OSDLR_EL1 | HvfSys64Register::OSLAR_EL1
    )
}

fn advance_arm64_pc(vcpu: &mut impl RunnerVcpu) -> Result<(), HvfVcpuRunnerError> {
    let pc = vcpu
        .read_register(HvfRegister::PC)
        .map_err(HvfVcpuRunnerError::Backend)?;
    let next_pc =
        pc.checked_add(ARM64_INSTRUCTION_SIZE)
            .ok_or(HvfVcpuRunnerError::InvalidState(
                "arm64 PC overflow while advancing handled synchronous exit",
            ))?;

    vcpu.write_register(HvfRegister::PC, next_pc)
        .map_err(HvfVcpuRunnerError::Backend)
}

fn handle_hvc_on_runner_thread(
    vcpu: &mut impl RunnerVcpu,
    exit: HvfHvcExit,
) -> Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError> {
    let function_id = vcpu
        .read_register(HvfRegister::X0)
        .map_err(HvfVcpuRunnerError::Backend)?;
    let arg0 = if exit.immediate() == 0 && call_uses_arg0(function_id) {
        vcpu.read_register(HvfRegister::X1)
            .map_err(HvfVcpuRunnerError::Backend)?
    } else {
        0
    };
    let result = if exit.immediate() == 0 {
        handle_psci_call(PsciCall::new(function_id, arg0))
    } else {
        not_supported_result()
    };
    let return_value = result.return_value();
    vcpu.write_register(HvfRegister::X0, return_value)
        .map_err(HvfVcpuRunnerError::Backend)?;

    match result.action() {
        PsciCallAction::Return => Ok(HvfVcpuRunStepOutcome::Hvc {
            exit,
            function_id,
            return_value,
        }),
        PsciCallAction::SystemOff => Ok(HvfVcpuRunStepOutcome::GuestShutdown {
            exit,
            function_id,
            return_value,
        }),
        PsciCallAction::SystemReset => Ok(HvfVcpuRunStepOutcome::GuestReset {
            exit,
            function_id,
            return_value,
        }),
    }
}

fn dispatch_mmio_access_on_runner_thread(
    vcpu: &mut impl RunnerVcpu,
    access: HvfResolvedMmioAccess,
    dispatcher: &SharedMmioDispatcher,
) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
    let mut dispatcher = lock_shared_mmio_dispatcher(dispatcher)?;

    let outcome = vcpu.dispatch_mmio_access(access, &mut dispatcher)?;
    advance_arm64_pc(vcpu)?;

    Ok(outcome)
}

fn lock_shared_mmio_dispatcher(
    dispatcher: &SharedMmioDispatcher,
) -> Result<MutexGuard<'_, MmioDispatcher>, HvfVcpuRunnerError> {
    dispatcher.try_lock().map_err(|err| match err {
        TryLockError::WouldBlock => HvfVcpuRunnerError::InvalidState(MMIO_DISPATCHER_BUSY_MESSAGE),
        TryLockError::Poisoned(_) => {
            HvfVcpuRunnerError::InvalidState(MMIO_DISPATCHER_POISONED_MESSAGE)
        }
    })
}

fn join_runner_thread(thread: Option<JoinHandle<()>>) -> Result<(), HvfVcpuRunnerError> {
    if let Some(thread) = thread {
        thread
            .join()
            .map_err(|_| HvfVcpuRunnerError::ThreadPanicked)?;
    }

    Ok(())
}

fn shutdown_result(
    first: Result<(), HvfVcpuRunnerError>,
    second: Result<(), HvfVcpuRunnerError>,
) -> Result<(), HvfVcpuRunnerError> {
    match (first, second) {
        (_, Err(HvfVcpuRunnerError::ThreadPanicked)) => Err(HvfVcpuRunnerError::ThreadPanicked),
        (Err(err), _) => Err(err),
        (Ok(()), result) => result,
    }
}

#[cfg(test)]
mod tests {
    use std::panic::{self, AssertUnwindSafe};
    use std::sync::{Arc, Condvar, Mutex, mpsc};
    use std::thread;

    use bangbang_runtime::BackendError;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::{MmioDispatchOutcome, MmioDispatcher, MmioRegionId};

    use super::{
        CancelVcpu, HvfVcpuRunStepOutcome, HvfVcpuRunner, HvfVcpuRunnerError, RunnerVcpu,
        spawn_runner_thread,
    };
    use crate::exit::{
        HvfExceptionExit, HvfHvcExit, HvfMmioAccessSize, HvfMmioDirection, HvfMmioRegister,
        HvfResolvedMmioAccess, HvfResolvedVcpuExit, HvfSys64Direction, HvfSys64Exit,
        HvfSys64Register, HvfVcpuExit, HvfVcpuExitResolveError,
    };
    use crate::gic::{HvfArm64GicIccRegisterState, HvfGicDeviceState, HvfGicError};
    use crate::mmio::{HvfMmioCompletionError, HvfMmioDispatchError};
    use crate::vcpu::{
        HvfArm64BootRegisters, HvfArm64VcpuCoreSystemRegisterState,
        HvfArm64VcpuExceptionRegisterState, HvfArm64VcpuExecutionControlRegisterState,
        HvfArm64VcpuGeneralRegisterState, HvfArm64VcpuIdentificationRegisterState,
        HvfArm64VcpuPendingInterruptState, HvfArm64VcpuPhysicalTimerState,
        HvfArm64VcpuPointerAuthenticationKeyState, HvfArm64VcpuSimdFpState,
        HvfArm64VcpuThreadContextRegisterState, HvfArm64VcpuTranslationRegisterState,
        HvfArm64VcpuVirtualTimerState, HvfInterruptType, HvfRegister, HvfSimdFpRegister,
        HvfSystemRegister,
    };

    const ESR_EC_HVC: u64 = 0x16;
    const ESR_EC_SYS64: u64 = 0x18;
    const ESR_EC_DATA_ABORT_LOWER_EL: u64 = 0x24;
    const ESR_EC_SHIFT: u64 = 26;
    const ESR_ISS_ISV: u64 = 1 << 24;
    const ESR_ISS_SYS64_DIRECTION: u64 = 1;
    const ESR_ISS_SYS64_CRM_SHIFT: u64 = 1;
    const ESR_ISS_SYS64_RT_SHIFT: u64 = 5;
    const ESR_ISS_SYS64_CRN_SHIFT: u64 = 10;
    const ESR_ISS_SYS64_OP1_SHIFT: u64 = 14;
    const ESR_ISS_SYS64_OP2_SHIFT: u64 = 17;
    const ESR_ISS_SYS64_OP0_SHIFT: u64 = 20;
    const ESR_ISS_SAS_SHIFT: u64 = 22;
    const ESR_ISS_SRT_SHIFT: u64 = 16;
    const ESR_ISS_WNR: u64 = 1 << 6;
    const ESR_ISS_SF: u64 = 1 << 15;
    const PSCI_VERSION: u64 = 0x8400_0000;
    const PSCI_CPU_ON: u64 = 0x8400_0003;
    const PSCI_SYSTEM_OFF: u64 = 0x8400_0008;
    const PSCI_SYSTEM_RESET: u64 = 0x8400_0009;
    const PSCI_FEATURES: u64 = 0x8400_000a;
    const PSCI_VERSION_0_2: u64 = 0x0000_0002;
    const PSCI_RET_SUCCESS: u64 = 0;
    const PSCI_RET_NOT_SUPPORTED: u64 = u64::MAX;
    const GIC_DEVICE_STATE_TEST_BYTES: [u8; 4] = [0xde, 0xad, 0xbe, 0xef];
    const GIC_ICC_REGISTER_STATE_TEST_VALUES: [u64; 10] =
        [0x10, 0x21, 0x32, 0x43, 0x54, 0x65, 0x76, 0x87, 0x98, 0xa9];

    struct FakeVcpu {
        entered_run_sender: mpsc::Sender<()>,
        release_run_receiver: mpsc::Receiver<Result<HvfVcpuExit, BackendError>>,
        destroyed_sender: mpsc::Sender<()>,
    }

    struct PanicOnRunVcpu;

    struct BlockingPanicOnRunVcpu {
        entered_run_sender: mpsc::Sender<()>,
        release_run_receiver: mpsc::Receiver<()>,
    }

    struct ConfigureRecordingVcpu {
        configured_sender: mpsc::Sender<HvfArm64BootRegisters>,
    }

    struct FailingOnceConfigureVcpu {
        configured_sender: mpsc::Sender<HvfArm64BootRegisters>,
        fail_next_setup: bool,
    }

    struct PanicOnConfigureVcpu;

    struct BlockingConfigureVcpu {
        entered_setup_sender: mpsc::Sender<()>,
        release_setup_receiver: mpsc::Receiver<Result<(), BackendError>>,
    }

    struct MpidrRecordingVcpu {
        mpidr: u64,
        fail_next_read: bool,
    }

    struct BlockingMpidrVcpu {
        entered_read_sender: mpsc::Sender<()>,
        release_read_receiver: mpsc::Receiver<Result<u64, BackendError>>,
    }

    struct GeneralRegisterCaptureRecordingVcpu {
        read_sender: mpsc::Sender<HvfRegister>,
        fail_next_register: Option<HvfRegister>,
    }

    struct BlockingGeneralRegisterCaptureVcpu {
        entered_capture_sender: mpsc::Sender<()>,
        release_capture_receiver: mpsc::Receiver<Result<(), BackendError>>,
        barrier_sender: mpsc::Sender<()>,
    }

    struct PanicOnGeneralRegisterCaptureVcpu;

    type BlockingGeneralRegisterCaptureRunner = (
        HvfVcpuRunner<'static>,
        mpsc::Receiver<()>,
        mpsc::Sender<Result<(), BackendError>>,
        mpsc::Receiver<()>,
    );

    struct CoreSystemRegisterCaptureRecordingVcpu {
        read_sender: mpsc::Sender<HvfSystemRegister>,
        fail_next_register: Option<HvfSystemRegister>,
    }

    struct BlockingCoreSystemRegisterCaptureVcpu {
        entered_capture_sender: mpsc::Sender<()>,
        release_capture_receiver: mpsc::Receiver<Result<(), BackendError>>,
        barrier_sender: mpsc::Sender<()>,
    }

    struct PanicOnCoreSystemRegisterCaptureVcpu;

    type BlockingCoreSystemRegisterCaptureRunner = (
        HvfVcpuRunner<'static>,
        mpsc::Receiver<()>,
        mpsc::Sender<Result<(), BackendError>>,
        mpsc::Receiver<()>,
    );

    struct ExceptionRegisterCaptureRecordingVcpu {
        read_sender: mpsc::Sender<HvfSystemRegister>,
        fail_next_register: Option<HvfSystemRegister>,
    }

    struct BlockingExceptionRegisterCaptureVcpu {
        entered_capture_sender: mpsc::Sender<()>,
        release_capture_receiver: mpsc::Receiver<Result<(), BackendError>>,
        barrier_sender: mpsc::Sender<()>,
    }

    struct PanicOnExceptionRegisterCaptureVcpu;

    type BlockingExceptionRegisterCaptureRunner = (
        HvfVcpuRunner<'static>,
        mpsc::Receiver<()>,
        mpsc::Sender<Result<(), BackendError>>,
        mpsc::Receiver<()>,
    );

    struct ExecutionControlRegisterCaptureRecordingVcpu {
        read_sender: mpsc::Sender<HvfSystemRegister>,
        fail_next_register: Option<HvfSystemRegister>,
    }

    struct BlockingExecutionControlRegisterCaptureVcpu {
        entered_capture_sender: mpsc::Sender<()>,
        release_capture_receiver: mpsc::Receiver<Result<(), BackendError>>,
        barrier_sender: mpsc::Sender<()>,
    }

    struct PanicOnExecutionControlRegisterCaptureVcpu;

    type BlockingExecutionControlRegisterCaptureRunner = (
        HvfVcpuRunner<'static>,
        mpsc::Receiver<()>,
        mpsc::Sender<Result<(), BackendError>>,
        mpsc::Receiver<()>,
    );

    struct IdentificationRegisterCaptureRecordingVcpu {
        read_sender: mpsc::Sender<HvfSystemRegister>,
        fail_next_register: Option<HvfSystemRegister>,
    }

    struct BlockingIdentificationRegisterCaptureVcpu {
        entered_capture_sender: mpsc::Sender<()>,
        release_capture_receiver: mpsc::Receiver<Result<(), BackendError>>,
        barrier_sender: mpsc::Sender<()>,
    }

    struct PanicOnIdentificationRegisterCaptureVcpu;

    type BlockingIdentificationRegisterCaptureRunner = (
        HvfVcpuRunner<'static>,
        mpsc::Receiver<()>,
        mpsc::Sender<Result<(), BackendError>>,
        mpsc::Receiver<()>,
    );

    struct TranslationRegisterCaptureRecordingVcpu {
        read_sender: mpsc::Sender<HvfSystemRegister>,
        fail_next_register: Option<HvfSystemRegister>,
    }

    struct BlockingTranslationRegisterCaptureVcpu {
        entered_capture_sender: mpsc::Sender<()>,
        release_capture_receiver: mpsc::Receiver<Result<(), BackendError>>,
        barrier_sender: mpsc::Sender<()>,
    }

    struct PanicOnTranslationRegisterCaptureVcpu;

    type BlockingTranslationRegisterCaptureRunner = (
        HvfVcpuRunner<'static>,
        mpsc::Receiver<()>,
        mpsc::Sender<Result<(), BackendError>>,
        mpsc::Receiver<()>,
    );

    struct PointerAuthenticationKeyCaptureRecordingVcpu {
        read_sender: mpsc::Sender<HvfSystemRegister>,
        fail_next_register: Option<HvfSystemRegister>,
    }

    struct BlockingPointerAuthenticationKeyCaptureVcpu {
        entered_capture_sender: mpsc::Sender<()>,
        release_capture_receiver: mpsc::Receiver<Result<(), BackendError>>,
        barrier_sender: mpsc::Sender<()>,
    }

    struct PanicOnPointerAuthenticationKeyCaptureVcpu;

    type BlockingPointerAuthenticationKeyCaptureRunner = (
        HvfVcpuRunner<'static>,
        mpsc::Receiver<()>,
        mpsc::Sender<Result<(), BackendError>>,
        mpsc::Receiver<()>,
    );

    struct ThreadContextRegisterCaptureRecordingVcpu {
        read_sender: mpsc::Sender<HvfSystemRegister>,
        fail_next_register: Option<HvfSystemRegister>,
    }

    struct BlockingThreadContextRegisterCaptureVcpu {
        entered_capture_sender: mpsc::Sender<()>,
        release_capture_receiver: mpsc::Receiver<Result<(), BackendError>>,
        barrier_sender: mpsc::Sender<()>,
    }

    struct PanicOnThreadContextRegisterCaptureVcpu;

    type BlockingThreadContextRegisterCaptureRunner = (
        HvfVcpuRunner<'static>,
        mpsc::Receiver<()>,
        mpsc::Sender<Result<(), BackendError>>,
        mpsc::Receiver<()>,
    );

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum SimdFpCaptureRead {
        Q(HvfSimdFpRegister),
        Scalar(HvfRegister),
    }

    struct SimdFpCaptureRecordingVcpu {
        read_sender: mpsc::Sender<SimdFpCaptureRead>,
        fail_next_read: Option<SimdFpCaptureRead>,
    }

    struct BlockingSimdFpCaptureVcpu {
        entered_capture_sender: mpsc::Sender<()>,
        release_capture_receiver: mpsc::Receiver<Result<(), BackendError>>,
        barrier_sender: mpsc::Sender<()>,
    }

    struct PanicOnSimdFpCaptureVcpu;

    type BlockingSimdFpCaptureRunner = (
        HvfVcpuRunner<'static>,
        mpsc::Receiver<()>,
        mpsc::Sender<Result<(), BackendError>>,
        mpsc::Receiver<()>,
    );

    struct PhysicalTimerCaptureRecordingVcpu {
        read_sender: mpsc::Sender<HvfSystemRegister>,
        fail_next_register: Option<HvfSystemRegister>,
    }

    struct BlockingPhysicalTimerCaptureVcpu {
        entered_capture_sender: mpsc::Sender<()>,
        release_capture_receiver: mpsc::Receiver<Result<(), BackendError>>,
        barrier_sender: mpsc::Sender<()>,
    }

    struct PanicOnPhysicalTimerCaptureVcpu;

    type BlockingPhysicalTimerCaptureRunner = (
        HvfVcpuRunner<'static>,
        mpsc::Receiver<()>,
        mpsc::Sender<Result<(), BackendError>>,
        mpsc::Receiver<()>,
    );

    struct VtimerMaskRecordingVcpu {
        masked: bool,
        offset: u64,
        control: u64,
        compare_value: u64,
        failures: VtimerFailures,
        operation_sender: Option<mpsc::Sender<VtimerOperation>>,
    }

    #[derive(Debug, Default)]
    struct VtimerFailures {
        get_mask: bool,
        set_mask: bool,
        get_offset: bool,
        set_offset: bool,
        get_control: bool,
        set_control: bool,
        get_compare_value: bool,
        set_compare_value: bool,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum VtimerOperation {
        GetMask,
        SetMask(bool),
        GetOffset,
        SetOffset(u64),
        GetControl,
        SetControl(u64),
        GetCompareValue,
        SetCompareValue(u64),
    }

    struct BlockingVtimerMaskVcpu {
        entered_get_sender: mpsc::Sender<()>,
        release_get_receiver: mpsc::Receiver<Result<bool, BackendError>>,
        offset: u64,
        control: u64,
        compare_value: u64,
        barrier_sender: Option<mpsc::Sender<()>>,
    }

    struct PanicOnVtimerMaskVcpu;

    type BlockingVirtualTimerCaptureRunner = (
        HvfVcpuRunner<'static>,
        mpsc::Receiver<()>,
        mpsc::Sender<Result<bool, BackendError>>,
        mpsc::Receiver<()>,
    );

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum PendingInterruptOperation {
        Get(HvfInterruptType),
        Set(HvfInterruptType, bool),
    }

    struct PendingInterruptRecordingVcpu {
        irq_pending: bool,
        fiq_pending: bool,
        fail_next_operation: Option<PendingInterruptOperation>,
        operation_sender: mpsc::Sender<PendingInterruptOperation>,
    }

    struct BlockingPendingInterruptVcpu {
        entered_get_sender: mpsc::Sender<()>,
        release_get_receiver: mpsc::Receiver<Result<(), BackendError>>,
        barrier_sender: Option<mpsc::Sender<()>>,
        irq_pending: bool,
        fiq_pending: bool,
    }

    struct PanicOnPendingInterruptVcpu;

    type BlockingPendingInterruptCaptureRunner = (
        HvfVcpuRunner<'static>,
        mpsc::Receiver<()>,
        mpsc::Sender<Result<(), BackendError>>,
        mpsc::Receiver<()>,
    );

    struct GicDeviceStateRecordingVcpu {
        fail_next_capture: bool,
        capture_sender: mpsc::Sender<()>,
    }

    struct BlockingGicDeviceStateVcpu {
        entered_capture_sender: mpsc::Sender<()>,
        release_capture_receiver: mpsc::Receiver<Result<Vec<u8>, HvfGicError>>,
        barrier_sender: Option<mpsc::Sender<()>>,
    }

    struct PanicOnGicDeviceStateVcpu;

    type BlockingGicDeviceStateCaptureRunner = (
        HvfVcpuRunner<'static>,
        mpsc::Receiver<()>,
        mpsc::Sender<Result<Vec<u8>, HvfGicError>>,
        mpsc::Receiver<()>,
    );

    struct GicIccRegisterStateRecordingVcpu {
        fail_next_capture: bool,
        capture_sender: mpsc::Sender<()>,
    }

    struct BlockingGicIccRegisterStateVcpu {
        entered_capture_sender: mpsc::Sender<()>,
        release_capture_receiver: mpsc::Receiver<Result<[u64; 10], HvfGicError>>,
        barrier_sender: Option<mpsc::Sender<()>>,
    }

    struct PanicOnGicIccRegisterStateVcpu;

    type BlockingGicIccRegisterStateCaptureRunner = (
        HvfVcpuRunner<'static>,
        mpsc::Receiver<()>,
        mpsc::Sender<Result<[u64; 10], HvfGicError>>,
        mpsc::Receiver<()>,
    );

    struct GicPpiPendingRecordingVcpu {
        operation_sender: mpsc::Sender<(u32, bool)>,
        fail_next_operation: bool,
    }

    struct BlockingGicPpiPendingVcpu {
        entered_operation_sender: mpsc::Sender<()>,
        release_operation_receiver: mpsc::Receiver<Result<(), HvfGicError>>,
    }

    struct PanicOnGicPpiPendingVcpu;

    struct MmioDispatchRecordingVcpu {
        dispatched_sender: mpsc::Sender<HvfResolvedMmioAccess>,
        result: Result<MmioDispatchOutcome, HvfVcpuRunnerError>,
        pc: u64,
    }

    struct BlockingMmioDispatchVcpu {
        entered_dispatch_sender: mpsc::Sender<()>,
        release_dispatch_receiver: mpsc::Receiver<Result<MmioDispatchOutcome, HvfVcpuRunnerError>>,
        pc: u64,
    }

    struct RunStepRecordingVcpu {
        run_once_result: Result<HvfVcpuExit, BackendError>,
        dispatched_sender: Option<mpsc::Sender<HvfResolvedMmioAccess>>,
        dispatch_result: Result<MmioDispatchOutcome, HvfVcpuRunnerError>,
        pc: u64,
        register_write_sender: mpsc::Sender<(HvfRegister, u64)>,
    }

    struct PsciRunStepRecordingVcpu {
        run_once_result: Result<HvfVcpuExit, BackendError>,
        x0: u64,
        x1: Result<u64, BackendError>,
        register_write_sender: mpsc::Sender<(HvfRegister, u64)>,
    }

    struct Sys64RunStepRecordingVcpu {
        run_once_result: Result<HvfVcpuExit, BackendError>,
        pc: u64,
        register_write_sender: mpsc::Sender<(HvfRegister, u64)>,
    }

    fn unsupported_mmio_dispatch() -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
        Err(HvfVcpuRunnerError::InvalidState(
            "fake vCPU does not support MMIO dispatch",
        ))
    }

    // Test-only wrapper used by panic-path tests to wait until run_runner_thread has unwound.
    fn start_panic_notifying_runner<C, V>(
        create_vcpu: C,
    ) -> (HvfVcpuRunner<'static>, mpsc::Receiver<()>)
    where
        C: FnOnce() -> Result<V, BackendError> + Send + 'static,
        V: RunnerVcpu + 'static,
    {
        let (command_sender, command_receiver) = mpsc::channel();
        let (startup_sender, startup_receiver) = mpsc::channel();
        let (runner_unwind_sender, runner_unwind_receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("bangbang-hvf-vcpu".to_string())
            .spawn(move || {
                let result = panic::catch_unwind(AssertUnwindSafe(|| {
                    super::run_runner_thread(command_receiver, startup_sender, create_vcpu);
                }));
                let _ = runner_unwind_sender.send(());
                if let Err(payload) = result {
                    panic::resume_unwind(payload);
                }
            })
            .expect("panic-notifying runner thread should spawn");

        let startup_result = match startup_receiver.recv() {
            Ok(startup_result) => startup_result,
            Err(_) => {
                super::join_runner_thread(Some(thread))
                    .expect("panic-notifying startup failure should join");
                panic!("panic-notifying runner startup channel should not close");
            }
        };
        let vcpu = startup_result.expect("panic-notifying runner should start");
        let started = super::StartedRunner {
            command_sender,
            vcpu,
            thread,
        };

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            runner_unwind_receiver,
        )
    }

    fn wait_for_panic_notifying_runner_unwind(runner_unwind_receiver: mpsc::Receiver<()>) {
        runner_unwind_receiver
            .recv()
            .expect("panic-notifying runner should unwind");
    }

    impl RunnerVcpu for FakeVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            self.entered_run_sender
                .send(())
                .map_err(|_| BackendError::InvalidState("fake run entry receiver closed"))?;
            self.release_run_receiver
                .recv()
                .map_err(|_| BackendError::InvalidState("fake run release sender closed"))?
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            self.destroyed_sender
                .send(())
                .map_err(|_| BackendError::InvalidState("fake destroy receiver closed"))
        }
    }

    impl RunnerVcpu for PanicOnRunVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            panic!("fake run panic");
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for BlockingPanicOnRunVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            self.entered_run_sender
                .send(())
                .expect("fake blocking panic entry receiver should remain open");
            self.release_run_receiver
                .recv()
                .expect("fake blocking panic release sender should remain open");
            panic!("fake blocking run panic");
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for ConfigureRecordingVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            self.configured_sender
                .send(registers)
                .map_err(|_| BackendError::InvalidState("fake setup receiver closed"))
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for FailingOnceConfigureVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            self.configured_sender
                .send(registers)
                .map_err(|_| BackendError::InvalidState("fake setup receiver closed"))?;
            if self.fail_next_setup {
                self.fail_next_setup = false;
                return Err(BackendError::InvalidState("fake setup failed"));
            }

            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for PanicOnConfigureVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            panic!("fake setup panic");
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for BlockingConfigureVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            self.entered_setup_sender
                .send(())
                .map_err(|_| BackendError::InvalidState("fake setup entry receiver closed"))?;
            self.release_setup_receiver
                .recv()
                .map_err(|_| BackendError::InvalidState("fake setup release sender closed"))?
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for MpidrRecordingVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn mpidr_el1(&mut self) -> Result<u64, BackendError> {
            if self.fail_next_read {
                self.fail_next_read = false;
                Err(BackendError::InvalidState("fake MPIDR read failed"))
            } else {
                Ok(self.mpidr)
            }
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for BlockingMpidrVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn mpidr_el1(&mut self) -> Result<u64, BackendError> {
            self.entered_read_sender
                .send(())
                .map_err(|_| BackendError::InvalidState("fake MPIDR entry receiver closed"))?;
            self.release_read_receiver
                .recv()
                .map_err(|_| BackendError::InvalidState("fake MPIDR release sender closed"))?
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for GeneralRegisterCaptureRecordingVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_register(&mut self, register: HvfRegister) -> Result<u64, BackendError> {
            self.read_sender
                .send(register)
                .map_err(|_| BackendError::InvalidState("fake register-read receiver closed"))?;
            if self.fail_next_register == Some(register) {
                self.fail_next_register = None;
                Err(BackendError::InvalidState(
                    "fake general-register capture failed",
                ))
            } else {
                Ok(0x1000 + u64::from(register.raw()))
            }
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for BlockingGeneralRegisterCaptureVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_register(&mut self, register: HvfRegister) -> Result<u64, BackendError> {
            if register == HvfRegister::X0 {
                self.entered_capture_sender.send(()).map_err(|_| {
                    BackendError::InvalidState("fake capture entry receiver closed")
                })?;
                self.release_capture_receiver.recv().map_err(|_| {
                    BackendError::InvalidState("fake capture release sender closed")
                })??;
            }

            Ok(0x1000 + u64::from(register.raw()))
        }

        fn mpidr_el1(&mut self) -> Result<u64, BackendError> {
            self.barrier_sender
                .send(())
                .map_err(|_| BackendError::InvalidState("fake barrier receiver closed"))?;
            Ok(0x8000_0000)
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for PanicOnGeneralRegisterCaptureVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_register(&mut self, _register: HvfRegister) -> Result<u64, BackendError> {
            panic!("fake general-register capture panic");
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for CoreSystemRegisterCaptureRecordingVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_system_register(
            &mut self,
            register: HvfSystemRegister,
        ) -> Result<u64, BackendError> {
            self.read_sender.send(register).map_err(|_| {
                BackendError::InvalidState("fake system-register read receiver closed")
            })?;
            if self.fail_next_register == Some(register) {
                self.fail_next_register = None;
                Err(BackendError::InvalidState(
                    "fake core system-register capture failed",
                ))
            } else {
                Ok(0x2_0000 + u64::from(register.raw()))
            }
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for BlockingCoreSystemRegisterCaptureVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_system_register(
            &mut self,
            register: HvfSystemRegister,
        ) -> Result<u64, BackendError> {
            if register == HvfSystemRegister::SP_EL0 {
                self.entered_capture_sender.send(()).map_err(|_| {
                    BackendError::InvalidState("fake system capture entry receiver closed")
                })?;
                self.release_capture_receiver.recv().map_err(|_| {
                    BackendError::InvalidState("fake system capture release sender closed")
                })??;
            }

            Ok(0x2_0000 + u64::from(register.raw()))
        }

        fn mpidr_el1(&mut self) -> Result<u64, BackendError> {
            self.barrier_sender
                .send(())
                .map_err(|_| BackendError::InvalidState("fake barrier receiver closed"))?;
            Ok(0x8000_0000)
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for PanicOnCoreSystemRegisterCaptureVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_system_register(
            &mut self,
            _register: HvfSystemRegister,
        ) -> Result<u64, BackendError> {
            panic!("fake core system-register capture panic");
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for ExceptionRegisterCaptureRecordingVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_system_register(
            &mut self,
            register: HvfSystemRegister,
        ) -> Result<u64, BackendError> {
            self.read_sender.send(register).map_err(|_| {
                BackendError::InvalidState("fake exception-register read receiver closed")
            })?;
            if self.fail_next_register == Some(register) {
                self.fail_next_register = None;
                Err(BackendError::InvalidState(
                    "fake exception-register capture failed",
                ))
            } else {
                Ok(0x7_0000 + u64::from(register.raw()))
            }
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for BlockingExceptionRegisterCaptureVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_system_register(
            &mut self,
            register: HvfSystemRegister,
        ) -> Result<u64, BackendError> {
            if register == HvfSystemRegister::AFSR0_EL1 {
                self.entered_capture_sender.send(()).map_err(|_| {
                    BackendError::InvalidState("fake exception capture entry receiver closed")
                })?;
                self.release_capture_receiver.recv().map_err(|_| {
                    BackendError::InvalidState("fake exception capture release sender closed")
                })??;
            }

            Ok(0x7_0000 + u64::from(register.raw()))
        }

        fn mpidr_el1(&mut self) -> Result<u64, BackendError> {
            self.barrier_sender
                .send(())
                .map_err(|_| BackendError::InvalidState("fake barrier receiver closed"))?;
            Ok(0x8000_0000)
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for PanicOnExceptionRegisterCaptureVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_system_register(
            &mut self,
            _register: HvfSystemRegister,
        ) -> Result<u64, BackendError> {
            panic!("fake exception-register capture panic");
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for ExecutionControlRegisterCaptureRecordingVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_system_register(
            &mut self,
            register: HvfSystemRegister,
        ) -> Result<u64, BackendError> {
            self.read_sender.send(register).map_err(|_| {
                BackendError::InvalidState("fake execution-control read receiver closed")
            })?;
            if self.fail_next_register == Some(register) {
                self.fail_next_register = None;
                Err(BackendError::InvalidState(
                    "fake execution-control capture failed",
                ))
            } else {
                Ok(0x8_0000 + u64::from(register.raw()))
            }
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for BlockingExecutionControlRegisterCaptureVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_system_register(
            &mut self,
            register: HvfSystemRegister,
        ) -> Result<u64, BackendError> {
            if register == HvfSystemRegister::ACTLR_EL1 {
                self.entered_capture_sender.send(()).map_err(|_| {
                    BackendError::InvalidState(
                        "fake execution-control capture entry receiver closed",
                    )
                })?;
                self.release_capture_receiver.recv().map_err(|_| {
                    BackendError::InvalidState(
                        "fake execution-control capture release sender closed",
                    )
                })??;
            }

            Ok(0x8_0000 + u64::from(register.raw()))
        }

        fn mpidr_el1(&mut self) -> Result<u64, BackendError> {
            self.barrier_sender
                .send(())
                .map_err(|_| BackendError::InvalidState("fake barrier receiver closed"))?;
            Ok(0x8000_0000)
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for PanicOnExecutionControlRegisterCaptureVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_system_register(
            &mut self,
            _register: HvfSystemRegister,
        ) -> Result<u64, BackendError> {
            panic!("fake execution-control capture panic");
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for IdentificationRegisterCaptureRecordingVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_system_register(
            &mut self,
            register: HvfSystemRegister,
        ) -> Result<u64, BackendError> {
            self.read_sender.send(register).map_err(|_| {
                BackendError::InvalidState("fake identification read receiver closed")
            })?;
            if self.fail_next_register == Some(register) {
                self.fail_next_register = None;
                Err(BackendError::InvalidState(
                    "fake identification-register capture failed",
                ))
            } else {
                Ok(identification_test_value(register))
            }
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for BlockingIdentificationRegisterCaptureVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_system_register(
            &mut self,
            register: HvfSystemRegister,
        ) -> Result<u64, BackendError> {
            if register == HvfSystemRegister::MIDR_EL1 {
                self.entered_capture_sender.send(()).map_err(|_| {
                    BackendError::InvalidState("fake identification capture entry receiver closed")
                })?;
                self.release_capture_receiver.recv().map_err(|_| {
                    BackendError::InvalidState("fake identification capture release sender closed")
                })??;
            }

            Ok(identification_test_value(register))
        }

        fn mpidr_el1(&mut self) -> Result<u64, BackendError> {
            self.barrier_sender
                .send(())
                .map_err(|_| BackendError::InvalidState("fake barrier receiver closed"))?;
            Ok(0x8000_0000)
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for PanicOnIdentificationRegisterCaptureVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_system_register(
            &mut self,
            _register: HvfSystemRegister,
        ) -> Result<u64, BackendError> {
            panic!("fake identification-register capture panic");
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for TranslationRegisterCaptureRecordingVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_system_register(
            &mut self,
            register: HvfSystemRegister,
        ) -> Result<u64, BackendError> {
            self.read_sender.send(register).map_err(|_| {
                BackendError::InvalidState("fake translation-register read receiver closed")
            })?;
            if self.fail_next_register == Some(register) {
                self.fail_next_register = None;
                Err(BackendError::InvalidState(
                    "fake translation-register capture failed",
                ))
            } else {
                Ok(0x6_0000 + u64::from(register.raw()))
            }
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for BlockingTranslationRegisterCaptureVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_system_register(
            &mut self,
            register: HvfSystemRegister,
        ) -> Result<u64, BackendError> {
            if register == HvfSystemRegister::SCTLR_EL1 {
                self.entered_capture_sender.send(()).map_err(|_| {
                    BackendError::InvalidState("fake translation capture entry receiver closed")
                })?;
                self.release_capture_receiver.recv().map_err(|_| {
                    BackendError::InvalidState("fake translation capture release sender closed")
                })??;
            }

            Ok(0x6_0000 + u64::from(register.raw()))
        }

        fn mpidr_el1(&mut self) -> Result<u64, BackendError> {
            self.barrier_sender
                .send(())
                .map_err(|_| BackendError::InvalidState("fake barrier receiver closed"))?;
            Ok(0x8000_0000)
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for PanicOnTranslationRegisterCaptureVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_system_register(
            &mut self,
            _register: HvfSystemRegister,
        ) -> Result<u64, BackendError> {
            panic!("fake translation-register capture panic");
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for PointerAuthenticationKeyCaptureRecordingVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_system_register(
            &mut self,
            register: HvfSystemRegister,
        ) -> Result<u64, BackendError> {
            self.read_sender.send(register).map_err(|_| {
                BackendError::InvalidState("fake pointer-authentication key read receiver closed")
            })?;
            if self.fail_next_register == Some(register) {
                self.fail_next_register = None;
                Err(BackendError::InvalidState(
                    "fake pointer-authentication key capture failed",
                ))
            } else {
                Ok(pointer_authentication_test_half(register))
            }
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for BlockingPointerAuthenticationKeyCaptureVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_system_register(
            &mut self,
            register: HvfSystemRegister,
        ) -> Result<u64, BackendError> {
            if register == HvfSystemRegister::APIAKEYLO_EL1 {
                self.entered_capture_sender.send(()).map_err(|_| {
                    BackendError::InvalidState(
                        "fake pointer-authentication key capture entry receiver closed",
                    )
                })?;
                self.release_capture_receiver.recv().map_err(|_| {
                    BackendError::InvalidState(
                        "fake pointer-authentication key capture release sender closed",
                    )
                })??;
            }

            Ok(pointer_authentication_test_half(register))
        }

        fn mpidr_el1(&mut self) -> Result<u64, BackendError> {
            self.barrier_sender
                .send(())
                .map_err(|_| BackendError::InvalidState("fake barrier receiver closed"))?;
            Ok(0x8000_0000)
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for PanicOnPointerAuthenticationKeyCaptureVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_system_register(
            &mut self,
            _register: HvfSystemRegister,
        ) -> Result<u64, BackendError> {
            panic!("fake pointer-authentication key capture panic");
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for ThreadContextRegisterCaptureRecordingVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_system_register(
            &mut self,
            register: HvfSystemRegister,
        ) -> Result<u64, BackendError> {
            self.read_sender.send(register).map_err(|_| {
                BackendError::InvalidState("fake thread-context read receiver closed")
            })?;
            if self.fail_next_register == Some(register) {
                self.fail_next_register = None;
                Err(BackendError::InvalidState(
                    "fake thread-context register capture failed",
                ))
            } else {
                Ok(0x5_0000 + u64::from(register.raw()))
            }
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for BlockingThreadContextRegisterCaptureVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_system_register(
            &mut self,
            register: HvfSystemRegister,
        ) -> Result<u64, BackendError> {
            if register == HvfSystemRegister::TPIDR_EL0 {
                self.entered_capture_sender.send(()).map_err(|_| {
                    BackendError::InvalidState("fake thread-context entry receiver closed")
                })?;
                self.release_capture_receiver.recv().map_err(|_| {
                    BackendError::InvalidState("fake thread-context release sender closed")
                })??;
            }

            Ok(0x5_0000 + u64::from(register.raw()))
        }

        fn mpidr_el1(&mut self) -> Result<u64, BackendError> {
            self.barrier_sender
                .send(())
                .map_err(|_| BackendError::InvalidState("fake barrier receiver closed"))?;
            Ok(0x8000_0000)
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for PanicOnThreadContextRegisterCaptureVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_system_register(
            &mut self,
            _register: HvfSystemRegister,
        ) -> Result<u64, BackendError> {
            panic!("fake thread-context register capture panic");
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl SimdFpCaptureRecordingVcpu {
        fn record_read(&mut self, read: SimdFpCaptureRead) -> Result<(), BackendError> {
            self.read_sender
                .send(read)
                .map_err(|_| BackendError::InvalidState("fake SIMD/FP read receiver closed"))?;
            if self.fail_next_read == Some(read) {
                self.fail_next_read = None;
                Err(BackendError::InvalidState("fake SIMD/FP capture failed"))
            } else {
                Ok(())
            }
        }
    }

    impl RunnerVcpu for SimdFpCaptureRecordingVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_register(&mut self, register: HvfRegister) -> Result<u64, BackendError> {
            self.record_read(SimdFpCaptureRead::Scalar(register))?;
            Ok(0x4_0000 + u64::from(register.raw()))
        }

        fn read_simd_fp_register(
            &mut self,
            register: HvfSimdFpRegister,
        ) -> Result<[u8; 16], BackendError> {
            self.record_read(SimdFpCaptureRead::Q(register))?;
            Ok(simd_fp_capture_q_value(register))
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for BlockingSimdFpCaptureVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_register(&mut self, register: HvfRegister) -> Result<u64, BackendError> {
            Ok(0x4_0000 + u64::from(register.raw()))
        }

        fn read_simd_fp_register(
            &mut self,
            register: HvfSimdFpRegister,
        ) -> Result<[u8; 16], BackendError> {
            if register == HvfSimdFpRegister::q(0).expect("Q0 should map to a SIMD register") {
                self.entered_capture_sender.send(()).map_err(|_| {
                    BackendError::InvalidState("fake SIMD/FP capture entry receiver closed")
                })?;
                self.release_capture_receiver.recv().map_err(|_| {
                    BackendError::InvalidState("fake SIMD/FP capture release sender closed")
                })??;
            }

            Ok(simd_fp_capture_q_value(register))
        }

        fn mpidr_el1(&mut self) -> Result<u64, BackendError> {
            self.barrier_sender
                .send(())
                .map_err(|_| BackendError::InvalidState("fake barrier receiver closed"))?;
            Ok(0x8000_0000)
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for PanicOnSimdFpCaptureVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_simd_fp_register(
            &mut self,
            _register: HvfSimdFpRegister,
        ) -> Result<[u8; 16], BackendError> {
            panic!("fake SIMD/FP capture panic");
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for PhysicalTimerCaptureRecordingVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_system_register(
            &mut self,
            register: HvfSystemRegister,
        ) -> Result<u64, BackendError> {
            self.read_sender.send(register).map_err(|_| {
                BackendError::InvalidState("fake physical-timer read receiver closed")
            })?;
            if self.fail_next_register == Some(register) {
                self.fail_next_register = None;
                Err(BackendError::InvalidState(
                    "fake physical-timer capture failed",
                ))
            } else {
                Ok(0x9_0000 + u64::from(register.raw()))
            }
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for BlockingPhysicalTimerCaptureVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_system_register(
            &mut self,
            register: HvfSystemRegister,
        ) -> Result<u64, BackendError> {
            if register == HvfSystemRegister::CNTKCTL_EL1 {
                self.entered_capture_sender.send(()).map_err(|_| {
                    BackendError::InvalidState("fake physical-timer entry receiver closed")
                })?;
                self.release_capture_receiver.recv().map_err(|_| {
                    BackendError::InvalidState("fake physical-timer release sender closed")
                })??;
            }

            Ok(0x9_0000 + u64::from(register.raw()))
        }

        fn mpidr_el1(&mut self) -> Result<u64, BackendError> {
            self.barrier_sender
                .send(())
                .map_err(|_| BackendError::InvalidState("fake barrier receiver closed"))?;
            Ok(0x8000_0000)
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for PanicOnPhysicalTimerCaptureVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_system_register(
            &mut self,
            _register: HvfSystemRegister,
        ) -> Result<u64, BackendError> {
            panic!("fake physical-timer capture panic");
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for VtimerMaskRecordingVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn get_vtimer_mask(&mut self) -> Result<bool, BackendError> {
            if let Some(sender) = &self.operation_sender {
                sender.send(VtimerOperation::GetMask).map_err(|_| {
                    BackendError::InvalidState("fake vtimer operation receiver closed")
                })?;
            }
            if self.failures.get_mask {
                self.failures.get_mask = false;
                Err(BackendError::InvalidState("fake vtimer mask read failed"))
            } else {
                Ok(self.masked)
            }
        }

        fn set_vtimer_mask(&mut self, masked: bool) -> Result<(), BackendError> {
            if let Some(sender) = &self.operation_sender {
                sender.send(VtimerOperation::SetMask(masked)).map_err(|_| {
                    BackendError::InvalidState("fake vtimer operation receiver closed")
                })?;
            }
            if self.failures.set_mask {
                self.failures.set_mask = false;
                Err(BackendError::InvalidState("fake vtimer mask write failed"))
            } else {
                self.masked = masked;
                Ok(())
            }
        }

        fn get_vtimer_offset(&mut self) -> Result<u64, BackendError> {
            if let Some(sender) = &self.operation_sender {
                sender.send(VtimerOperation::GetOffset).map_err(|_| {
                    BackendError::InvalidState("fake vtimer operation receiver closed")
                })?;
            }
            if self.failures.get_offset {
                self.failures.get_offset = false;
                Err(BackendError::InvalidState("fake vtimer offset read failed"))
            } else {
                Ok(self.offset)
            }
        }

        fn set_vtimer_offset(&mut self, offset: u64) -> Result<(), BackendError> {
            if let Some(sender) = &self.operation_sender {
                sender
                    .send(VtimerOperation::SetOffset(offset))
                    .map_err(|_| {
                        BackendError::InvalidState("fake vtimer operation receiver closed")
                    })?;
            }
            if self.failures.set_offset {
                self.failures.set_offset = false;
                Err(BackendError::InvalidState(
                    "fake vtimer offset write failed",
                ))
            } else {
                self.offset = offset;
                Ok(())
            }
        }

        fn get_vtimer_control(&mut self) -> Result<u64, BackendError> {
            if let Some(sender) = &self.operation_sender {
                sender.send(VtimerOperation::GetControl).map_err(|_| {
                    BackendError::InvalidState("fake vtimer operation receiver closed")
                })?;
            }
            if self.failures.get_control {
                self.failures.get_control = false;
                Err(BackendError::InvalidState(
                    "fake vtimer control read failed",
                ))
            } else {
                Ok(self.control)
            }
        }

        fn set_vtimer_control(&mut self, control: u64) -> Result<(), BackendError> {
            if let Some(sender) = &self.operation_sender {
                sender
                    .send(VtimerOperation::SetControl(control))
                    .map_err(|_| {
                        BackendError::InvalidState("fake vtimer operation receiver closed")
                    })?;
            }
            if self.failures.set_control {
                self.failures.set_control = false;
                Err(BackendError::InvalidState(
                    "fake vtimer control write failed",
                ))
            } else {
                self.control = control;
                Ok(())
            }
        }

        fn get_vtimer_compare_value(&mut self) -> Result<u64, BackendError> {
            if let Some(sender) = &self.operation_sender {
                sender.send(VtimerOperation::GetCompareValue).map_err(|_| {
                    BackendError::InvalidState("fake vtimer operation receiver closed")
                })?;
            }
            if self.failures.get_compare_value {
                self.failures.get_compare_value = false;
                Err(BackendError::InvalidState(
                    "fake vtimer compare-value read failed",
                ))
            } else {
                Ok(self.compare_value)
            }
        }

        fn set_vtimer_compare_value(&mut self, compare_value: u64) -> Result<(), BackendError> {
            if let Some(sender) = &self.operation_sender {
                sender
                    .send(VtimerOperation::SetCompareValue(compare_value))
                    .map_err(|_| {
                        BackendError::InvalidState("fake vtimer operation receiver closed")
                    })?;
            }
            if self.failures.set_compare_value {
                self.failures.set_compare_value = false;
                Err(BackendError::InvalidState(
                    "fake vtimer compare-value write failed",
                ))
            } else {
                self.compare_value = compare_value;
                Ok(())
            }
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for BlockingVtimerMaskVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn get_vtimer_mask(&mut self) -> Result<bool, BackendError> {
            self.entered_get_sender.send(()).map_err(|_| {
                BackendError::InvalidState("fake vtimer mask entry receiver closed")
            })?;
            self.release_get_receiver
                .recv()
                .map_err(|_| BackendError::InvalidState("fake vtimer mask release sender closed"))?
        }

        fn get_vtimer_offset(&mut self) -> Result<u64, BackendError> {
            Ok(self.offset)
        }

        fn get_vtimer_control(&mut self) -> Result<u64, BackendError> {
            Ok(self.control)
        }

        fn get_vtimer_compare_value(&mut self) -> Result<u64, BackendError> {
            Ok(self.compare_value)
        }

        fn mpidr_el1(&mut self) -> Result<u64, BackendError> {
            if let Some(sender) = &self.barrier_sender {
                sender
                    .send(())
                    .map_err(|_| BackendError::InvalidState("fake barrier receiver closed"))?;
            }
            Ok(0x8000_0000)
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for PanicOnVtimerMaskVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn get_vtimer_mask(&mut self) -> Result<bool, BackendError> {
            panic!("fake vtimer mask panic");
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for PendingInterruptRecordingVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn get_pending_interrupt(
            &mut self,
            interrupt_type: HvfInterruptType,
        ) -> Result<bool, BackendError> {
            let operation = PendingInterruptOperation::Get(interrupt_type);
            self.operation_sender.send(operation).map_err(|_| {
                BackendError::InvalidState("fake pending interrupt operation receiver closed")
            })?;
            if self.fail_next_operation == Some(operation) {
                self.fail_next_operation = None;
                return Err(BackendError::InvalidState(
                    "fake pending interrupt operation failed",
                ));
            }

            Ok(match interrupt_type {
                HvfInterruptType::Irq => self.irq_pending,
                HvfInterruptType::Fiq => self.fiq_pending,
            })
        }

        fn set_pending_interrupt(
            &mut self,
            interrupt_type: HvfInterruptType,
            pending: bool,
        ) -> Result<(), BackendError> {
            let operation = PendingInterruptOperation::Set(interrupt_type, pending);
            self.operation_sender.send(operation).map_err(|_| {
                BackendError::InvalidState("fake pending interrupt operation receiver closed")
            })?;
            if self.fail_next_operation == Some(operation) {
                self.fail_next_operation = None;
                return Err(BackendError::InvalidState(
                    "fake pending interrupt operation failed",
                ));
            }

            match interrupt_type {
                HvfInterruptType::Irq => self.irq_pending = pending,
                HvfInterruptType::Fiq => self.fiq_pending = pending,
            }
            Ok(())
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for BlockingPendingInterruptVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn get_pending_interrupt(
            &mut self,
            interrupt_type: HvfInterruptType,
        ) -> Result<bool, BackendError> {
            if interrupt_type == HvfInterruptType::Irq {
                self.entered_get_sender.send(()).map_err(|_| {
                    BackendError::InvalidState("fake pending interrupt entry receiver closed")
                })?;
                self.release_get_receiver.recv().map_err(|_| {
                    BackendError::InvalidState("fake pending interrupt release sender closed")
                })??;
            }

            Ok(match interrupt_type {
                HvfInterruptType::Irq => self.irq_pending,
                HvfInterruptType::Fiq => self.fiq_pending,
            })
        }

        fn mpidr_el1(&mut self) -> Result<u64, BackendError> {
            if let Some(sender) = &self.barrier_sender {
                sender
                    .send(())
                    .map_err(|_| BackendError::InvalidState("fake barrier receiver closed"))?;
            }
            Ok(0x8000_0000)
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for PanicOnPendingInterruptVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn get_pending_interrupt(
            &mut self,
            _interrupt_type: HvfInterruptType,
        ) -> Result<bool, BackendError> {
            panic!("fake pending interrupt panic");
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for GicDeviceStateRecordingVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn capture_gic_device_state(&mut self) -> Result<HvfGicDeviceState, HvfGicError> {
            self.capture_sender
                .send(())
                .map_err(|_| HvfGicError::InvalidState("fake GIC capture receiver closed"))?;
            if self.fail_next_capture {
                self.fail_next_capture = false;
                Err(HvfGicError::InvalidState(
                    "fake GIC device-state capture failed",
                ))
            } else {
                Ok(HvfGicDeviceState::new(GIC_DEVICE_STATE_TEST_BYTES.to_vec()))
            }
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for BlockingGicDeviceStateVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn capture_gic_device_state(&mut self) -> Result<HvfGicDeviceState, HvfGicError> {
            self.entered_capture_sender
                .send(())
                .map_err(|_| HvfGicError::InvalidState("fake GIC capture entry receiver closed"))?;
            let bytes = self.release_capture_receiver.recv().map_err(|_| {
                HvfGicError::InvalidState("fake GIC capture release sender closed")
            })??;
            Ok(HvfGicDeviceState::new(bytes))
        }

        fn mpidr_el1(&mut self) -> Result<u64, BackendError> {
            if let Some(sender) = &self.barrier_sender {
                sender
                    .send(())
                    .map_err(|_| BackendError::InvalidState("fake barrier receiver closed"))?;
            }
            Ok(0x8000_0000)
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for PanicOnGicDeviceStateVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn capture_gic_device_state(&mut self) -> Result<HvfGicDeviceState, HvfGicError> {
            panic!("fake GIC device-state capture panic");
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for GicIccRegisterStateRecordingVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn capture_arm64_gic_icc_register_state(
            &mut self,
        ) -> Result<HvfArm64GicIccRegisterState, HvfGicError> {
            self.capture_sender
                .send(())
                .map_err(|_| HvfGicError::InvalidState("fake GIC ICC capture receiver closed"))?;
            if self.fail_next_capture {
                self.fail_next_capture = false;
                Err(HvfGicError::InvalidState(
                    "fake GIC ICC register-state capture failed",
                ))
            } else {
                Ok(HvfArm64GicIccRegisterState::new(
                    GIC_ICC_REGISTER_STATE_TEST_VALUES,
                ))
            }
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for BlockingGicIccRegisterStateVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn capture_arm64_gic_icc_register_state(
            &mut self,
        ) -> Result<HvfArm64GicIccRegisterState, HvfGicError> {
            self.entered_capture_sender.send(()).map_err(|_| {
                HvfGicError::InvalidState("fake GIC ICC capture entry receiver closed")
            })?;
            let values = self.release_capture_receiver.recv().map_err(|_| {
                HvfGicError::InvalidState("fake GIC ICC capture release sender closed")
            })??;
            Ok(HvfArm64GicIccRegisterState::new(values))
        }

        fn mpidr_el1(&mut self) -> Result<u64, BackendError> {
            if let Some(sender) = &self.barrier_sender {
                sender
                    .send(())
                    .map_err(|_| BackendError::InvalidState("fake barrier receiver closed"))?;
            }
            Ok(0x8000_0000)
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for PanicOnGicIccRegisterStateVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn capture_arm64_gic_icc_register_state(
            &mut self,
        ) -> Result<HvfArm64GicIccRegisterState, HvfGicError> {
            panic!("fake GIC ICC register-state capture panic");
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for GicPpiPendingRecordingVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn set_gic_ppi_pending(&mut self, intid: u32, pending: bool) -> Result<(), HvfGicError> {
            if self.fail_next_operation {
                self.fail_next_operation = false;
                Err(HvfGicError::InvalidState(
                    "fake GIC PPI pending operation failed",
                ))
            } else {
                self.operation_sender
                    .send((intid, pending))
                    .map_err(|_| HvfGicError::InvalidState("fake GIC PPI receiver closed"))
            }
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for BlockingGicPpiPendingVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn set_gic_ppi_pending(&mut self, _intid: u32, _pending: bool) -> Result<(), HvfGicError> {
            self.entered_operation_sender
                .send(())
                .map_err(|_| HvfGicError::InvalidState("fake GIC PPI entry receiver closed"))?;
            self.release_operation_receiver
                .recv()
                .map_err(|_| HvfGicError::InvalidState("fake GIC PPI release sender closed"))?
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for PanicOnGicPpiPendingVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn set_gic_ppi_pending(&mut self, _intid: u32, _pending: bool) -> Result<(), HvfGicError> {
            panic!("fake GIC PPI pending panic");
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for MmioDispatchRecordingVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            access: HvfResolvedMmioAccess,
            dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            self.dispatched_sender.send(access).map_err(|_| {
                HvfVcpuRunnerError::InvalidState("fake MMIO dispatch receiver closed")
            })?;
            dispatcher
                .insert_region(MmioRegionId::new(99), GuestAddress::new(0x3000), 0x100)
                .map_err(|_| HvfVcpuRunnerError::InvalidState("fake dispatcher mutation failed"))?;

            self.result.clone()
        }

        fn read_register(&mut self, register: HvfRegister) -> Result<u64, BackendError> {
            if register == HvfRegister::PC {
                Ok(self.pc)
            } else {
                Err(BackendError::InvalidState(
                    "fake MMIO dispatch vCPU only supports PC reads",
                ))
            }
        }

        fn write_register(
            &mut self,
            register: HvfRegister,
            value: u64,
        ) -> Result<(), BackendError> {
            if register == HvfRegister::PC {
                self.pc = value;
                Ok(())
            } else {
                Err(BackendError::InvalidState(
                    "fake MMIO dispatch vCPU only supports PC writes",
                ))
            }
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for BlockingMmioDispatchVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            Ok(HvfVcpuExit::Canceled)
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            self.entered_dispatch_sender.send(()).map_err(|_| {
                HvfVcpuRunnerError::InvalidState("fake MMIO dispatch entry receiver closed")
            })?;
            self.release_dispatch_receiver.recv().map_err(|_| {
                HvfVcpuRunnerError::InvalidState("fake MMIO dispatch release sender closed")
            })?
        }

        fn read_register(&mut self, register: HvfRegister) -> Result<u64, BackendError> {
            if register == HvfRegister::PC {
                Ok(self.pc)
            } else {
                Err(BackendError::InvalidState(
                    "fake blocking MMIO dispatch vCPU only supports PC reads",
                ))
            }
        }

        fn write_register(
            &mut self,
            register: HvfRegister,
            value: u64,
        ) -> Result<(), BackendError> {
            if register == HvfRegister::PC {
                self.pc = value;
                Ok(())
            } else {
                Err(BackendError::InvalidState(
                    "fake blocking MMIO dispatch vCPU only supports PC writes",
                ))
            }
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for RunStepRecordingVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            self.run_once_result.clone()
        }

        fn dispatch_mmio_access(
            &mut self,
            access: HvfResolvedMmioAccess,
            dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            let Some(dispatched_sender) = &self.dispatched_sender else {
                return Err(HvfVcpuRunnerError::InvalidState(
                    "fake run step vCPU does not expect MMIO dispatch",
                ));
            };
            dispatched_sender.send(access).map_err(|_| {
                HvfVcpuRunnerError::InvalidState("fake run step dispatch receiver closed")
            })?;
            dispatcher
                .insert_region(MmioRegionId::new(99), GuestAddress::new(0x3000), 0x100)
                .map_err(|_| HvfVcpuRunnerError::InvalidState("fake dispatcher mutation failed"))?;

            self.dispatch_result.clone()
        }

        fn read_register(&mut self, register: HvfRegister) -> Result<u64, BackendError> {
            if register == HvfRegister::PC {
                Ok(self.pc)
            } else {
                Err(BackendError::InvalidState(
                    "fake run step vCPU only supports PC reads",
                ))
            }
        }

        fn write_register(
            &mut self,
            register: HvfRegister,
            value: u64,
        ) -> Result<(), BackendError> {
            if register == HvfRegister::PC {
                self.pc = value;
            }
            self.register_write_sender
                .send((register, value))
                .map_err(|_| BackendError::InvalidState("fake register write receiver closed"))
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for PsciRunStepRecordingVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            self.run_once_result.clone()
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_register(&mut self, register: HvfRegister) -> Result<u64, BackendError> {
            match register {
                HvfRegister::X0 => Ok(self.x0),
                HvfRegister::X1 => self.x1.clone(),
                _ => Err(BackendError::InvalidState("unexpected fake register read")),
            }
        }

        fn write_register(
            &mut self,
            register: HvfRegister,
            value: u64,
        ) -> Result<(), BackendError> {
            if register != HvfRegister::X0 {
                return Err(BackendError::InvalidState("unexpected fake register write"));
            }

            self.x0 = value;
            self.register_write_sender
                .send((register, value))
                .map_err(|_| BackendError::InvalidState("fake register write receiver closed"))
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    impl RunnerVcpu for Sys64RunStepRecordingVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn configure_arm64_boot_registers(
            &mut self,
            _registers: HvfArm64BootRegisters,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            self.run_once_result.clone()
        }

        fn dispatch_mmio_access(
            &mut self,
            _access: HvfResolvedMmioAccess,
            _dispatcher: &mut MmioDispatcher,
        ) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
            unsupported_mmio_dispatch()
        }

        fn read_register(&mut self, register: HvfRegister) -> Result<u64, BackendError> {
            if register == HvfRegister::PC {
                Ok(self.pc)
            } else {
                Err(BackendError::InvalidState("SYS64 should not read a GPR"))
            }
        }

        fn write_register(
            &mut self,
            register: HvfRegister,
            value: u64,
        ) -> Result<(), BackendError> {
            if register == HvfRegister::PC {
                self.pc = value;
            }
            self.register_write_sender
                .send((register, value))
                .map_err(|_| BackendError::InvalidState("fake register write receiver closed"))
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    fn boot_registers() -> HvfArm64BootRegisters {
        HvfArm64BootRegisters {
            kernel_entry: GuestAddress::new(0x8028_0000),
            fdt_address: GuestAddress::new(0x8fe0_0000),
        }
    }

    fn mmio_exception_exit() -> HvfVcpuExit {
        let register = HvfMmioRegister::new(1).expect("test register should decode");
        HvfVcpuExit::Exception(HvfExceptionExit {
            syndrome: data_abort_syndrome(
                HvfMmioAccessSize::Byte,
                HvfMmioDirection::Read,
                register,
            ),
            virtual_address: 0x2000,
            physical_address: 0x1040,
        })
    }

    fn hvc_exception_exit(immediate: u16) -> HvfVcpuExit {
        HvfVcpuExit::Exception(HvfExceptionExit {
            syndrome: hvc_syndrome(immediate),
            virtual_address: 0,
            physical_address: 0,
        })
    }

    fn hvc_exit(immediate: u16) -> HvfHvcExit {
        let HvfVcpuExit::Exception(exit) = hvc_exception_exit(immediate) else {
            panic!("test HVC helper should build an exception exit");
        };

        exit.decode_hvc().expect("test HVC exit should decode")
    }

    fn resolved_mmio_access() -> HvfResolvedMmioAccess {
        let mut dispatcher = MmioDispatcher::new();
        dispatcher
            .insert_region(MmioRegionId::new(7), GuestAddress::new(0x1000), 0x100)
            .expect("test region should insert");

        let resolved = mmio_exception_exit()
            .resolve_with_mmio_bus(dispatcher.bus())
            .expect("test access should resolve");
        let HvfResolvedVcpuExit::Mmio(access) = resolved else {
            panic!("expected MMIO exit");
        };
        access
    }

    fn hvc_syndrome(immediate: u16) -> u64 {
        (ESR_EC_HVC << ESR_EC_SHIFT) | u64::from(immediate)
    }

    fn sys64_syndrome(
        direction: HvfSys64Direction,
        register: HvfSys64Register,
        target_register: u8,
    ) -> u64 {
        let direction_bit = match direction {
            HvfSys64Direction::Read => ESR_ISS_SYS64_DIRECTION,
            HvfSys64Direction::Write => 0,
        };

        (ESR_EC_SYS64 << ESR_EC_SHIFT)
            | direction_bit
            | (u64::from(target_register) << ESR_ISS_SYS64_RT_SHIFT)
            | (u64::from(register.op0()) << ESR_ISS_SYS64_OP0_SHIFT)
            | (u64::from(register.op1()) << ESR_ISS_SYS64_OP1_SHIFT)
            | (u64::from(register.crn()) << ESR_ISS_SYS64_CRN_SHIFT)
            | (u64::from(register.crm()) << ESR_ISS_SYS64_CRM_SHIFT)
            | (u64::from(register.op2()) << ESR_ISS_SYS64_OP2_SHIFT)
    }

    fn sys64_exception_exit(
        direction: HvfSys64Direction,
        register: HvfSys64Register,
        target_register: u8,
    ) -> HvfVcpuExit {
        HvfVcpuExit::Exception(HvfExceptionExit {
            syndrome: sys64_syndrome(direction, register, target_register),
            virtual_address: 0,
            physical_address: 0,
        })
    }

    fn sys64_exit(
        direction: HvfSys64Direction,
        register: HvfSys64Register,
        target_register: u8,
    ) -> HvfSys64Exit {
        let HvfVcpuExit::Exception(exit) =
            sys64_exception_exit(direction, register, target_register)
        else {
            panic!("test SYS64 helper should build an exception exit");
        };

        exit.decode_sys64().expect("test SYS64 exit should decode")
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

    fn simd_fp_capture_q_value(register: HvfSimdFpRegister) -> [u8; 16] {
        std::array::from_fn(|index| (register.raw() as u8) ^ (index as u8).wrapping_mul(29))
    }

    fn expected_simd_fp_capture_reads() -> Vec<SimdFpCaptureRead> {
        (0_u8..32)
            .map(|index| {
                SimdFpCaptureRead::Q(
                    HvfSimdFpRegister::q(index).expect("Q0-Q31 should map to SIMD registers"),
                )
            })
            .chain([
                SimdFpCaptureRead::Scalar(HvfRegister::FPCR),
                SimdFpCaptureRead::Scalar(HvfRegister::FPSR),
            ])
            .collect()
    }

    fn exception_registers() -> [HvfSystemRegister; 6] {
        [
            HvfSystemRegister::AFSR0_EL1,
            HvfSystemRegister::AFSR1_EL1,
            HvfSystemRegister::ESR_EL1,
            HvfSystemRegister::FAR_EL1,
            HvfSystemRegister::PAR_EL1,
            HvfSystemRegister::VBAR_EL1,
        ]
    }

    fn assert_exception_register_test_state(state: HvfArm64VcpuExceptionRegisterState) {
        assert_eq!(
            state.afsr0_el1(),
            0x7_0000 + u64::from(HvfSystemRegister::AFSR0_EL1.raw())
        );
        assert_eq!(
            state.afsr1_el1(),
            0x7_0000 + u64::from(HvfSystemRegister::AFSR1_EL1.raw())
        );
        assert_eq!(
            state.esr_el1(),
            0x7_0000 + u64::from(HvfSystemRegister::ESR_EL1.raw())
        );
        assert_eq!(
            state.far_el1(),
            0x7_0000 + u64::from(HvfSystemRegister::FAR_EL1.raw())
        );
        assert_eq!(
            state.par_el1(),
            0x7_0000 + u64::from(HvfSystemRegister::PAR_EL1.raw())
        );
        assert_eq!(
            state.vbar_el1(),
            0x7_0000 + u64::from(HvfSystemRegister::VBAR_EL1.raw())
        );
    }

    fn execution_control_registers() -> [HvfSystemRegister; 2] {
        [HvfSystemRegister::ACTLR_EL1, HvfSystemRegister::CPACR_EL1]
    }

    fn assert_execution_control_register_test_state(
        state: HvfArm64VcpuExecutionControlRegisterState,
    ) {
        assert_eq!(
            state.actlr_el1(),
            0x8_0000 + u64::from(HvfSystemRegister::ACTLR_EL1.raw())
        );
        assert_eq!(
            state.cpacr_el1(),
            0x8_0000 + u64::from(HvfSystemRegister::CPACR_EL1.raw())
        );
    }

    fn identification_registers() -> [HvfSystemRegister; 11] {
        [
            HvfSystemRegister::MIDR_EL1,
            HvfSystemRegister::MPIDR_EL1,
            HvfSystemRegister::ID_AA64PFR0_EL1,
            HvfSystemRegister::ID_AA64PFR1_EL1,
            HvfSystemRegister::ID_AA64DFR0_EL1,
            HvfSystemRegister::ID_AA64DFR1_EL1,
            HvfSystemRegister::ID_AA64ISAR0_EL1,
            HvfSystemRegister::ID_AA64ISAR1_EL1,
            HvfSystemRegister::ID_AA64MMFR0_EL1,
            HvfSystemRegister::ID_AA64MMFR1_EL1,
            HvfSystemRegister::ID_AA64MMFR2_EL1,
        ]
    }

    fn identification_test_value(register: HvfSystemRegister) -> u64 {
        0xa_0000 + u64::from(register.raw())
    }

    fn assert_identification_register_test_state(state: HvfArm64VcpuIdentificationRegisterState) {
        let registers = identification_registers();
        assert_eq!(state.midr_el1(), identification_test_value(registers[0]));
        assert_eq!(state.mpidr_el1(), identification_test_value(registers[1]));
        assert_eq!(
            state.id_aa64pfr0_el1(),
            identification_test_value(registers[2])
        );
        assert_eq!(
            state.id_aa64pfr1_el1(),
            identification_test_value(registers[3])
        );
        assert_eq!(
            state.id_aa64dfr0_el1(),
            identification_test_value(registers[4])
        );
        assert_eq!(
            state.id_aa64dfr1_el1(),
            identification_test_value(registers[5])
        );
        assert_eq!(
            state.id_aa64isar0_el1(),
            identification_test_value(registers[6])
        );
        assert_eq!(
            state.id_aa64isar1_el1(),
            identification_test_value(registers[7])
        );
        assert_eq!(
            state.id_aa64mmfr0_el1(),
            identification_test_value(registers[8])
        );
        assert_eq!(
            state.id_aa64mmfr1_el1(),
            identification_test_value(registers[9])
        );
        assert_eq!(
            state.id_aa64mmfr2_el1(),
            identification_test_value(registers[10])
        );
    }

    fn physical_timer_registers() -> [HvfSystemRegister; 3] {
        [
            HvfSystemRegister::CNTKCTL_EL1,
            HvfSystemRegister::CNTP_CTL_EL0,
            HvfSystemRegister::CNTP_CVAL_EL0,
        ]
    }

    fn assert_physical_timer_test_state(state: HvfArm64VcpuPhysicalTimerState) {
        assert_eq!(
            state.cntkctl_el1(),
            0x9_0000 + u64::from(HvfSystemRegister::CNTKCTL_EL1.raw())
        );
        assert_eq!(
            state.cntp_ctl_el0(),
            0x9_0000 + u64::from(HvfSystemRegister::CNTP_CTL_EL0.raw())
        );
        assert_eq!(
            state.cntp_cval_el0(),
            0x9_0000 + u64::from(HvfSystemRegister::CNTP_CVAL_EL0.raw())
        );
    }

    fn translation_registers() -> [HvfSystemRegister; 7] {
        [
            HvfSystemRegister::SCTLR_EL1,
            HvfSystemRegister::TTBR0_EL1,
            HvfSystemRegister::TTBR1_EL1,
            HvfSystemRegister::TCR_EL1,
            HvfSystemRegister::MAIR_EL1,
            HvfSystemRegister::AMAIR_EL1,
            HvfSystemRegister::CONTEXTIDR_EL1,
        ]
    }

    fn assert_translation_register_test_state(state: HvfArm64VcpuTranslationRegisterState) {
        assert_eq!(
            state.sctlr_el1(),
            0x6_0000 + u64::from(HvfSystemRegister::SCTLR_EL1.raw())
        );
        assert_eq!(
            state.ttbr0_el1(),
            0x6_0000 + u64::from(HvfSystemRegister::TTBR0_EL1.raw())
        );
        assert_eq!(
            state.ttbr1_el1(),
            0x6_0000 + u64::from(HvfSystemRegister::TTBR1_EL1.raw())
        );
        assert_eq!(
            state.tcr_el1(),
            0x6_0000 + u64::from(HvfSystemRegister::TCR_EL1.raw())
        );
        assert_eq!(
            state.mair_el1(),
            0x6_0000 + u64::from(HvfSystemRegister::MAIR_EL1.raw())
        );
        assert_eq!(
            state.amair_el1(),
            0x6_0000 + u64::from(HvfSystemRegister::AMAIR_EL1.raw())
        );
        assert_eq!(
            state.contextidr_el1(),
            0x6_0000 + u64::from(HvfSystemRegister::CONTEXTIDR_EL1.raw())
        );
    }

    fn pointer_authentication_key_registers() -> [HvfSystemRegister; 10] {
        [
            HvfSystemRegister::APIAKEYLO_EL1,
            HvfSystemRegister::APIAKEYHI_EL1,
            HvfSystemRegister::APIBKEYLO_EL1,
            HvfSystemRegister::APIBKEYHI_EL1,
            HvfSystemRegister::APDAKEYLO_EL1,
            HvfSystemRegister::APDAKEYHI_EL1,
            HvfSystemRegister::APDBKEYLO_EL1,
            HvfSystemRegister::APDBKEYHI_EL1,
            HvfSystemRegister::APGAKEYLO_EL1,
            HvfSystemRegister::APGAKEYHI_EL1,
        ]
    }

    fn pointer_authentication_test_half(register: HvfSystemRegister) -> u64 {
        0xa11c_0000_0000_0000 | u64::from(register.raw())
    }

    fn pointer_authentication_test_key(low: HvfSystemRegister, high: HvfSystemRegister) -> u128 {
        u128::from(pointer_authentication_test_half(low))
            | (u128::from(pointer_authentication_test_half(high)) << 64)
    }

    fn assert_pointer_authentication_key_test_state(
        state: &HvfArm64VcpuPointerAuthenticationKeyState,
    ) {
        let registers = pointer_authentication_key_registers();
        assert_eq!(
            state.apia_key(),
            pointer_authentication_test_key(registers[0], registers[1])
        );
        assert_eq!(
            state.apib_key(),
            pointer_authentication_test_key(registers[2], registers[3])
        );
        assert_eq!(
            state.apda_key(),
            pointer_authentication_test_key(registers[4], registers[5])
        );
        assert_eq!(
            state.apdb_key(),
            pointer_authentication_test_key(registers[6], registers[7])
        );
        assert_eq!(
            state.apga_key(),
            pointer_authentication_test_key(registers[8], registers[9])
        );
    }

    fn shared_dispatcher() -> Arc<Mutex<MmioDispatcher>> {
        Arc::new(Mutex::new(MmioDispatcher::new()))
    }

    fn assert_core_register_captures_rejected(
        runner: &HvfVcpuRunner<'_>,
        expected: HvfVcpuRunnerError,
    ) {
        assert_eq!(
            runner.capture_arm64_general_register_state(),
            Err(expected.clone())
        );
        assert_eq!(
            runner.capture_arm64_core_system_register_state(),
            Err(expected.clone())
        );
        assert_eq!(
            runner.capture_arm64_exception_register_state(),
            Err(expected.clone())
        );
        assert_eq!(
            runner.capture_arm64_execution_control_register_state(),
            Err(expected.clone())
        );
        assert_eq!(
            runner.capture_arm64_identification_register_state(),
            Err(expected.clone())
        );
        assert_eq!(
            runner.capture_arm64_translation_register_state(),
            Err(expected.clone())
        );
        assert_eq!(
            runner.capture_arm64_pointer_authentication_key_state(),
            Err(expected.clone())
        );
        assert_eq!(
            runner.capture_arm64_thread_context_register_state(),
            Err(expected.clone())
        );
        assert_eq!(runner.capture_arm64_simd_fp_state(), Err(expected));
    }

    fn assert_timer_operations_rejected(runner: &HvfVcpuRunner<'_>, expected: HvfVcpuRunnerError) {
        assert_eq!(runner.get_vtimer_mask(), Err(expected.clone()));
        assert_eq!(runner.set_vtimer_mask(false), Err(expected.clone()));
        assert_eq!(runner.get_vtimer_offset(), Err(expected.clone()));
        assert_eq!(runner.set_vtimer_offset(0), Err(expected.clone()));
        assert_eq!(runner.get_vtimer_control(), Err(expected.clone()));
        assert_eq!(runner.set_vtimer_control(0), Err(expected.clone()));
        assert_eq!(runner.get_vtimer_compare_value(), Err(expected.clone()));
        assert_eq!(runner.set_vtimer_compare_value(0), Err(expected.clone()));
        assert_eq!(
            runner.capture_arm64_physical_timer_state(),
            Err(expected.clone())
        );
        assert_eq!(runner.capture_arm64_virtual_timer_state(), Err(expected));
    }

    fn assert_interrupt_operations_rejected(
        runner: &HvfVcpuRunner<'_>,
        expected: HvfVcpuRunnerError,
    ) {
        assert_eq!(
            runner.get_pending_interrupt(HvfInterruptType::Irq),
            Err(expected.clone())
        );
        assert_eq!(
            runner.get_pending_interrupt(HvfInterruptType::Fiq),
            Err(expected.clone())
        );
        assert_eq!(
            runner.set_pending_interrupt(HvfInterruptType::Irq, true),
            Err(expected.clone())
        );
        assert_eq!(
            runner.set_pending_interrupt(HvfInterruptType::Fiq, false),
            Err(expected.clone())
        );
        assert_eq!(
            runner.capture_arm64_pending_interrupt_state(),
            Err(expected.clone())
        );
        assert_eq!(runner.capture_gic_device_state(), Err(expected.clone()));
        assert_eq!(
            runner.capture_arm64_gic_icc_register_state(),
            Err(expected.clone())
        );
        assert_eq!(runner.set_gic_ppi_pending(27), Err(expected.clone()));
        assert_eq!(runner.clear_gic_ppi_pending(27), Err(expected));
    }

    fn assert_gic_icc_register_test_state(state: HvfArm64GicIccRegisterState) {
        assert_eq!(state.pmr_el1(), GIC_ICC_REGISTER_STATE_TEST_VALUES[0]);
        assert_eq!(state.bpr0_el1(), GIC_ICC_REGISTER_STATE_TEST_VALUES[1]);
        assert_eq!(state.ap0r0_el1(), GIC_ICC_REGISTER_STATE_TEST_VALUES[2]);
        assert_eq!(state.ap1r0_el1(), GIC_ICC_REGISTER_STATE_TEST_VALUES[3]);
        assert_eq!(state.rpr_el1(), GIC_ICC_REGISTER_STATE_TEST_VALUES[4]);
        assert_eq!(state.bpr1_el1(), GIC_ICC_REGISTER_STATE_TEST_VALUES[5]);
        assert_eq!(state.ctlr_el1(), GIC_ICC_REGISTER_STATE_TEST_VALUES[6]);
        assert_eq!(state.sre_el1(), GIC_ICC_REGISTER_STATE_TEST_VALUES[7]);
        assert_eq!(state.igrpen0_el1(), GIC_ICC_REGISTER_STATE_TEST_VALUES[8]);
        assert_eq!(state.igrpen1_el1(), GIC_ICC_REGISTER_STATE_TEST_VALUES[9]);
    }

    fn shared_dispatcher_with_region() -> Arc<Mutex<MmioDispatcher>> {
        let dispatcher = shared_dispatcher();
        {
            dispatcher
                .lock()
                .expect("dispatcher lock should not be poisoned")
                .insert_region(MmioRegionId::new(7), GuestAddress::new(0x1000), 0x100)
                .expect("test region should insert");
        }
        dispatcher
    }

    fn fake_cancel_vcpu(
        release_run_sender: mpsc::Sender<Result<HvfVcpuExit, BackendError>>,
    ) -> CancelVcpu {
        Arc::new(move |_| {
            release_run_sender
                .send(Ok(HvfVcpuExit::Canceled))
                .map_err(|_| BackendError::InvalidState("fake run release receiver closed"))
        })
    }

    fn start_fake_runner() -> (
        HvfVcpuRunner<'static>,
        mpsc::Receiver<()>,
        mpsc::Receiver<()>,
    ) {
        let (entered_run_sender, entered_run_receiver) = mpsc::channel();
        let (release_run_sender, release_run_receiver) = mpsc::channel();
        let (destroyed_sender, destroyed_receiver) = mpsc::channel();
        let cancel_vcpu = fake_cancel_vcpu(release_run_sender);
        start_fake_runner_with_cancel(
            cancel_vcpu,
            entered_run_sender,
            release_run_receiver,
            destroyed_sender,
            entered_run_receiver,
            destroyed_receiver,
        )
    }

    fn start_dispatch_recording_runner(
        result: Result<MmioDispatchOutcome, HvfVcpuRunnerError>,
    ) -> (
        HvfVcpuRunner<'static>,
        mpsc::Receiver<HvfResolvedMmioAccess>,
    ) {
        let (dispatched_sender, dispatched_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(MmioDispatchRecordingVcpu {
                dispatched_sender,
                result,
                pc: 0x8020_2000,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            dispatched_receiver,
        )
    }

    fn start_run_step_recording_runner(
        run_once_result: Result<HvfVcpuExit, BackendError>,
        dispatch_result: Result<MmioDispatchOutcome, HvfVcpuRunnerError>,
    ) -> (
        HvfVcpuRunner<'static>,
        mpsc::Receiver<HvfResolvedMmioAccess>,
        mpsc::Receiver<(HvfRegister, u64)>,
    ) {
        let (dispatched_sender, dispatched_receiver) = mpsc::channel();
        let (register_write_sender, register_write_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(RunStepRecordingVcpu {
                run_once_result,
                dispatched_sender: Some(dispatched_sender),
                dispatch_result,
                pc: 0x8020_3000,
                register_write_sender,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            dispatched_receiver,
            register_write_receiver,
        )
    }

    fn start_run_step_exit_runner(
        run_once_result: Result<HvfVcpuExit, BackendError>,
    ) -> HvfVcpuRunner<'static> {
        let started = spawn_runner_thread(move || {
            let (register_write_sender, _register_write_receiver) = mpsc::channel();
            Ok(RunStepRecordingVcpu {
                run_once_result,
                dispatched_sender: None,
                dispatch_result: Ok(MmioDispatchOutcome::Write),
                pc: 0x8020_3000,
                register_write_sender,
            })
        })
        .expect("fake runner should start");

        HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
            .expect("runner should be created")
    }

    fn start_psci_run_step_recording_runner(
        function_id: u64,
        arg0: u64,
    ) -> (HvfVcpuRunner<'static>, mpsc::Receiver<(HvfRegister, u64)>) {
        start_psci_run_step_recording_runner_with_x1(function_id, Ok(arg0))
    }

    fn start_psci_run_step_recording_runner_with_x1(
        function_id: u64,
        x1: Result<u64, BackendError>,
    ) -> (HvfVcpuRunner<'static>, mpsc::Receiver<(HvfRegister, u64)>) {
        start_psci_run_step_recording_runner_with_exit(function_id, x1, 0)
    }

    fn start_psci_run_step_recording_runner_with_exit(
        function_id: u64,
        x1: Result<u64, BackendError>,
        hvc_immediate: u16,
    ) -> (HvfVcpuRunner<'static>, mpsc::Receiver<(HvfRegister, u64)>) {
        let (register_write_sender, register_write_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(PsciRunStepRecordingVcpu {
                run_once_result: Ok(hvc_exception_exit(hvc_immediate)),
                x0: function_id,
                x1,
                register_write_sender,
            })
        })
        .expect("fake PSCI runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            register_write_receiver,
        )
    }

    fn start_sys64_run_step_recording_runner(
        direction: HvfSys64Direction,
        register: HvfSys64Register,
        target_register: u8,
    ) -> (HvfVcpuRunner<'static>, mpsc::Receiver<(HvfRegister, u64)>) {
        start_sys64_run_step_recording_runner_with_pc(
            direction,
            register,
            target_register,
            0x8020_1000,
        )
    }

    fn start_sys64_run_step_recording_runner_with_pc(
        direction: HvfSys64Direction,
        register: HvfSys64Register,
        target_register: u8,
        pc: u64,
    ) -> (HvfVcpuRunner<'static>, mpsc::Receiver<(HvfRegister, u64)>) {
        let (register_write_sender, register_write_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(Sys64RunStepRecordingVcpu {
                run_once_result: Ok(sys64_exception_exit(direction, register, target_register)),
                pc,
                register_write_sender,
            })
        })
        .expect("fake SYS64 runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            register_write_receiver,
        )
    }

    fn start_mpidr_recording_runner(mpidr: u64, fail_next_read: bool) -> HvfVcpuRunner<'static> {
        let started = spawn_runner_thread(move || {
            Ok(MpidrRecordingVcpu {
                mpidr,
                fail_next_read,
            })
        })
        .expect("fake runner should start");

        HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
            .expect("runner should be created")
    }

    fn start_blocking_mpidr_runner() -> (
        HvfVcpuRunner<'static>,
        mpsc::Receiver<()>,
        mpsc::Sender<Result<u64, BackendError>>,
    ) {
        let (entered_read_sender, entered_read_receiver) = mpsc::channel();
        let (release_read_sender, release_read_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(BlockingMpidrVcpu {
                entered_read_sender,
                release_read_receiver,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            entered_read_receiver,
            release_read_sender,
        )
    }

    fn start_general_register_capture_recording_runner(
        fail_next_register: Option<HvfRegister>,
    ) -> (HvfVcpuRunner<'static>, mpsc::Receiver<HvfRegister>) {
        let (read_sender, read_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(GeneralRegisterCaptureRecordingVcpu {
                read_sender,
                fail_next_register,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            read_receiver,
        )
    }

    fn start_blocking_general_register_capture_runner() -> BlockingGeneralRegisterCaptureRunner {
        let (entered_capture_sender, entered_capture_receiver) = mpsc::channel();
        let (release_capture_sender, release_capture_receiver) = mpsc::channel();
        let (barrier_sender, barrier_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(BlockingGeneralRegisterCaptureVcpu {
                entered_capture_sender,
                release_capture_receiver,
                barrier_sender,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            entered_capture_receiver,
            release_capture_sender,
            barrier_receiver,
        )
    }

    fn start_core_system_register_capture_recording_runner(
        fail_next_register: Option<HvfSystemRegister>,
    ) -> (HvfVcpuRunner<'static>, mpsc::Receiver<HvfSystemRegister>) {
        let (read_sender, read_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(CoreSystemRegisterCaptureRecordingVcpu {
                read_sender,
                fail_next_register,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            read_receiver,
        )
    }

    fn start_blocking_core_system_register_capture_runner()
    -> BlockingCoreSystemRegisterCaptureRunner {
        let (entered_capture_sender, entered_capture_receiver) = mpsc::channel();
        let (release_capture_sender, release_capture_receiver) = mpsc::channel();
        let (barrier_sender, barrier_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(BlockingCoreSystemRegisterCaptureVcpu {
                entered_capture_sender,
                release_capture_receiver,
                barrier_sender,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            entered_capture_receiver,
            release_capture_sender,
            barrier_receiver,
        )
    }

    fn start_exception_register_capture_recording_runner(
        fail_next_register: Option<HvfSystemRegister>,
    ) -> (HvfVcpuRunner<'static>, mpsc::Receiver<HvfSystemRegister>) {
        let (read_sender, read_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(ExceptionRegisterCaptureRecordingVcpu {
                read_sender,
                fail_next_register,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            read_receiver,
        )
    }

    fn start_blocking_exception_register_capture_runner() -> BlockingExceptionRegisterCaptureRunner
    {
        let (entered_capture_sender, entered_capture_receiver) = mpsc::channel();
        let (release_capture_sender, release_capture_receiver) = mpsc::channel();
        let (barrier_sender, barrier_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(BlockingExceptionRegisterCaptureVcpu {
                entered_capture_sender,
                release_capture_receiver,
                barrier_sender,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            entered_capture_receiver,
            release_capture_sender,
            barrier_receiver,
        )
    }

    fn start_execution_control_register_capture_recording_runner(
        fail_next_register: Option<HvfSystemRegister>,
    ) -> (HvfVcpuRunner<'static>, mpsc::Receiver<HvfSystemRegister>) {
        let (read_sender, read_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(ExecutionControlRegisterCaptureRecordingVcpu {
                read_sender,
                fail_next_register,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            read_receiver,
        )
    }

    fn start_blocking_execution_control_register_capture_runner()
    -> BlockingExecutionControlRegisterCaptureRunner {
        let (entered_capture_sender, entered_capture_receiver) = mpsc::channel();
        let (release_capture_sender, release_capture_receiver) = mpsc::channel();
        let (barrier_sender, barrier_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(BlockingExecutionControlRegisterCaptureVcpu {
                entered_capture_sender,
                release_capture_receiver,
                barrier_sender,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            entered_capture_receiver,
            release_capture_sender,
            barrier_receiver,
        )
    }

    fn start_identification_register_capture_recording_runner(
        fail_next_register: Option<HvfSystemRegister>,
    ) -> (HvfVcpuRunner<'static>, mpsc::Receiver<HvfSystemRegister>) {
        let (read_sender, read_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(IdentificationRegisterCaptureRecordingVcpu {
                read_sender,
                fail_next_register,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            read_receiver,
        )
    }

    fn start_blocking_identification_register_capture_runner()
    -> BlockingIdentificationRegisterCaptureRunner {
        let (entered_capture_sender, entered_capture_receiver) = mpsc::channel();
        let (release_capture_sender, release_capture_receiver) = mpsc::channel();
        let (barrier_sender, barrier_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(BlockingIdentificationRegisterCaptureVcpu {
                entered_capture_sender,
                release_capture_receiver,
                barrier_sender,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            entered_capture_receiver,
            release_capture_sender,
            barrier_receiver,
        )
    }

    fn start_translation_register_capture_recording_runner(
        fail_next_register: Option<HvfSystemRegister>,
    ) -> (HvfVcpuRunner<'static>, mpsc::Receiver<HvfSystemRegister>) {
        let (read_sender, read_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(TranslationRegisterCaptureRecordingVcpu {
                read_sender,
                fail_next_register,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            read_receiver,
        )
    }

    fn start_blocking_translation_register_capture_runner()
    -> BlockingTranslationRegisterCaptureRunner {
        let (entered_capture_sender, entered_capture_receiver) = mpsc::channel();
        let (release_capture_sender, release_capture_receiver) = mpsc::channel();
        let (barrier_sender, barrier_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(BlockingTranslationRegisterCaptureVcpu {
                entered_capture_sender,
                release_capture_receiver,
                barrier_sender,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            entered_capture_receiver,
            release_capture_sender,
            barrier_receiver,
        )
    }

    fn start_pointer_authentication_key_capture_recording_runner(
        fail_next_register: Option<HvfSystemRegister>,
    ) -> (HvfVcpuRunner<'static>, mpsc::Receiver<HvfSystemRegister>) {
        let (read_sender, read_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(PointerAuthenticationKeyCaptureRecordingVcpu {
                read_sender,
                fail_next_register,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            read_receiver,
        )
    }

    fn start_blocking_pointer_authentication_key_capture_runner()
    -> BlockingPointerAuthenticationKeyCaptureRunner {
        let (entered_capture_sender, entered_capture_receiver) = mpsc::channel();
        let (release_capture_sender, release_capture_receiver) = mpsc::channel();
        let (barrier_sender, barrier_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(BlockingPointerAuthenticationKeyCaptureVcpu {
                entered_capture_sender,
                release_capture_receiver,
                barrier_sender,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            entered_capture_receiver,
            release_capture_sender,
            barrier_receiver,
        )
    }

    fn start_thread_context_register_capture_recording_runner(
        fail_next_register: Option<HvfSystemRegister>,
    ) -> (HvfVcpuRunner<'static>, mpsc::Receiver<HvfSystemRegister>) {
        let (read_sender, read_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(ThreadContextRegisterCaptureRecordingVcpu {
                read_sender,
                fail_next_register,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            read_receiver,
        )
    }

    fn start_blocking_thread_context_register_capture_runner()
    -> BlockingThreadContextRegisterCaptureRunner {
        let (entered_capture_sender, entered_capture_receiver) = mpsc::channel();
        let (release_capture_sender, release_capture_receiver) = mpsc::channel();
        let (barrier_sender, barrier_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(BlockingThreadContextRegisterCaptureVcpu {
                entered_capture_sender,
                release_capture_receiver,
                barrier_sender,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            entered_capture_receiver,
            release_capture_sender,
            barrier_receiver,
        )
    }

    fn start_simd_fp_capture_recording_runner(
        fail_next_read: Option<SimdFpCaptureRead>,
    ) -> (HvfVcpuRunner<'static>, mpsc::Receiver<SimdFpCaptureRead>) {
        let (read_sender, read_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(SimdFpCaptureRecordingVcpu {
                read_sender,
                fail_next_read,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            read_receiver,
        )
    }

    fn start_blocking_simd_fp_capture_runner() -> BlockingSimdFpCaptureRunner {
        let (entered_capture_sender, entered_capture_receiver) = mpsc::channel();
        let (release_capture_sender, release_capture_receiver) = mpsc::channel();
        let (barrier_sender, barrier_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(BlockingSimdFpCaptureVcpu {
                entered_capture_sender,
                release_capture_receiver,
                barrier_sender,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            entered_capture_receiver,
            release_capture_sender,
            barrier_receiver,
        )
    }

    fn start_physical_timer_capture_recording_runner(
        fail_next_register: Option<HvfSystemRegister>,
    ) -> (HvfVcpuRunner<'static>, mpsc::Receiver<HvfSystemRegister>) {
        let (read_sender, read_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(PhysicalTimerCaptureRecordingVcpu {
                read_sender,
                fail_next_register,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            read_receiver,
        )
    }

    fn start_blocking_physical_timer_capture_runner() -> BlockingPhysicalTimerCaptureRunner {
        let (entered_capture_sender, entered_capture_receiver) = mpsc::channel();
        let (release_capture_sender, release_capture_receiver) = mpsc::channel();
        let (barrier_sender, barrier_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(BlockingPhysicalTimerCaptureVcpu {
                entered_capture_sender,
                release_capture_receiver,
                barrier_sender,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            entered_capture_receiver,
            release_capture_sender,
            barrier_receiver,
        )
    }

    fn start_vtimer_mask_recording_runner(
        masked: bool,
        fail_next_get: bool,
        fail_next_set: bool,
    ) -> HvfVcpuRunner<'static> {
        let started = spawn_runner_thread(move || {
            Ok(VtimerMaskRecordingVcpu {
                masked,
                offset: 0,
                control: 0,
                compare_value: 0,
                failures: VtimerFailures {
                    get_mask: fail_next_get,
                    set_mask: fail_next_set,
                    ..VtimerFailures::default()
                },
                operation_sender: None,
            })
        })
        .expect("fake runner should start");

        HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
            .expect("runner should be created")
    }

    fn start_blocking_vtimer_mask_runner() -> (
        HvfVcpuRunner<'static>,
        mpsc::Receiver<()>,
        mpsc::Sender<Result<bool, BackendError>>,
    ) {
        let (entered_get_sender, entered_get_receiver) = mpsc::channel();
        let (release_get_sender, release_get_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(BlockingVtimerMaskVcpu {
                entered_get_sender,
                release_get_receiver,
                offset: 0,
                control: 0,
                compare_value: 0,
                barrier_sender: None,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            entered_get_receiver,
            release_get_sender,
        )
    }

    fn start_vtimer_recording_runner(
        masked: bool,
        offset: u64,
        control: u64,
        compare_value: u64,
        failures: VtimerFailures,
    ) -> (HvfVcpuRunner<'static>, mpsc::Receiver<VtimerOperation>) {
        let (operation_sender, operation_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(VtimerMaskRecordingVcpu {
                masked,
                offset,
                control,
                compare_value,
                failures,
                operation_sender: Some(operation_sender),
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            operation_receiver,
        )
    }

    fn start_blocking_virtual_timer_capture_runner() -> BlockingVirtualTimerCaptureRunner {
        let (entered_get_sender, entered_get_receiver) = mpsc::channel();
        let (release_get_sender, release_get_receiver) = mpsc::channel();
        let (barrier_sender, barrier_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(BlockingVtimerMaskVcpu {
                entered_get_sender,
                release_get_receiver,
                offset: 0x1234_5678,
                control: 0b101,
                compare_value: 0xfedc_ba98,
                barrier_sender: Some(barrier_sender),
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            entered_get_receiver,
            release_get_sender,
            barrier_receiver,
        )
    }

    fn start_pending_interrupt_recording_runner(
        irq_pending: bool,
        fiq_pending: bool,
        fail_next_operation: Option<PendingInterruptOperation>,
    ) -> (
        HvfVcpuRunner<'static>,
        mpsc::Receiver<PendingInterruptOperation>,
    ) {
        let (operation_sender, operation_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(PendingInterruptRecordingVcpu {
                irq_pending,
                fiq_pending,
                fail_next_operation,
                operation_sender,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            operation_receiver,
        )
    }

    fn start_blocking_pending_interrupt_capture_runner() -> BlockingPendingInterruptCaptureRunner {
        let (entered_get_sender, entered_get_receiver) = mpsc::channel();
        let (release_get_sender, release_get_receiver) = mpsc::channel();
        let (barrier_sender, barrier_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(BlockingPendingInterruptVcpu {
                entered_get_sender,
                release_get_receiver,
                barrier_sender: Some(barrier_sender),
                irq_pending: true,
                fiq_pending: false,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            entered_get_receiver,
            release_get_sender,
            barrier_receiver,
        )
    }

    fn start_gic_device_state_recording_runner(
        fail_next_capture: bool,
    ) -> (HvfVcpuRunner<'static>, mpsc::Receiver<()>) {
        let (capture_sender, capture_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(GicDeviceStateRecordingVcpu {
                fail_next_capture,
                capture_sender,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            capture_receiver,
        )
    }

    fn start_blocking_gic_device_state_capture_runner() -> BlockingGicDeviceStateCaptureRunner {
        let (entered_capture_sender, entered_capture_receiver) = mpsc::channel();
        let (release_capture_sender, release_capture_receiver) = mpsc::channel();
        let (barrier_sender, barrier_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(BlockingGicDeviceStateVcpu {
                entered_capture_sender,
                release_capture_receiver,
                barrier_sender: Some(barrier_sender),
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            entered_capture_receiver,
            release_capture_sender,
            barrier_receiver,
        )
    }

    fn start_gic_icc_register_state_recording_runner(
        fail_next_capture: bool,
    ) -> (HvfVcpuRunner<'static>, mpsc::Receiver<()>) {
        let (capture_sender, capture_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(GicIccRegisterStateRecordingVcpu {
                fail_next_capture,
                capture_sender,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            capture_receiver,
        )
    }

    fn start_blocking_gic_icc_register_state_capture_runner()
    -> BlockingGicIccRegisterStateCaptureRunner {
        let (entered_capture_sender, entered_capture_receiver) = mpsc::channel();
        let (release_capture_sender, release_capture_receiver) = mpsc::channel();
        let (barrier_sender, barrier_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(BlockingGicIccRegisterStateVcpu {
                entered_capture_sender,
                release_capture_receiver,
                barrier_sender: Some(barrier_sender),
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            entered_capture_receiver,
            release_capture_sender,
            barrier_receiver,
        )
    }

    fn start_gic_ppi_pending_recording_runner(
        fail_next_operation: bool,
    ) -> (HvfVcpuRunner<'static>, mpsc::Receiver<(u32, bool)>) {
        let (operation_sender, operation_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(GicPpiPendingRecordingVcpu {
                operation_sender,
                fail_next_operation,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            operation_receiver,
        )
    }

    fn start_blocking_gic_ppi_pending_runner() -> (
        HvfVcpuRunner<'static>,
        mpsc::Receiver<()>,
        mpsc::Sender<Result<(), HvfGicError>>,
    ) {
        let (entered_operation_sender, entered_operation_receiver) = mpsc::channel();
        let (release_operation_sender, release_operation_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(BlockingGicPpiPendingVcpu {
                entered_operation_sender,
                release_operation_receiver,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            entered_operation_receiver,
            release_operation_sender,
        )
    }

    fn start_fake_runner_with_cancel(
        cancel_vcpu: CancelVcpu,
        entered_run_sender: mpsc::Sender<()>,
        release_run_receiver: mpsc::Receiver<Result<HvfVcpuExit, BackendError>>,
        destroyed_sender: mpsc::Sender<()>,
        entered_run_receiver: mpsc::Receiver<()>,
        destroyed_receiver: mpsc::Receiver<()>,
    ) -> (
        HvfVcpuRunner<'static>,
        mpsc::Receiver<()>,
        mpsc::Receiver<()>,
    ) {
        let started = spawn_runner_thread(move || {
            Ok(FakeVcpu {
                entered_run_sender,
                release_run_receiver,
                destroyed_sender,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, cancel_vcpu).expect("runner should be created"),
            entered_run_receiver,
            destroyed_receiver,
        )
    }

    #[test]
    fn run_cancel_handle_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<super::HvfVcpuRunCancelHandle>();
        assert_send_sync::<HvfArm64VcpuCoreSystemRegisterState>();
        assert_send_sync::<HvfArm64VcpuExceptionRegisterState>();
        assert_send_sync::<HvfArm64VcpuExecutionControlRegisterState>();
        assert_send_sync::<HvfArm64VcpuGeneralRegisterState>();
        assert_send_sync::<HvfArm64VcpuIdentificationRegisterState>();
        assert_send_sync::<HvfArm64VcpuPendingInterruptState>();
        assert_send_sync::<HvfArm64VcpuPhysicalTimerState>();
        assert_send_sync::<HvfArm64VcpuPointerAuthenticationKeyState>();
        assert_send_sync::<HvfArm64VcpuSimdFpState>();
        assert_send_sync::<HvfArm64VcpuThreadContextRegisterState>();
        assert_send_sync::<HvfArm64VcpuTranslationRegisterState>();
        assert_send_sync::<HvfArm64VcpuVirtualTimerState>();
        assert_send_sync::<HvfGicDeviceState>();
        assert_send_sync::<HvfArm64GicIccRegisterState>();
        assert_send_sync::<HvfInterruptType>();
    }

    #[test]
    fn captures_arm64_general_register_state_on_runner_thread() {
        let (runner, read_receiver) = start_general_register_capture_recording_runner(None);

        let state = runner
            .capture_arm64_general_register_state()
            .expect("general-register capture should succeed");

        assert_eq!(state.general_purpose_registers().len(), 31);
        assert_eq!(state.general_purpose_register(0), Some(0x1000));
        assert_eq!(state.general_purpose_register(30), Some(0x101e));
        assert_eq!(state.general_purpose_register(31), None);
        assert_eq!(state.pc(), 0x1000 + u64::from(HvfRegister::PC.raw()));
        assert_eq!(state.cpsr(), 0x1000 + u64::from(HvfRegister::CPSR.raw()));
        assert_eq!(
            read_receiver.try_iter().collect::<Vec<_>>(),
            (0_u8..31)
                .map(|index| {
                    HvfRegister::general_purpose(index).expect("X0-X30 should map to registers")
                })
                .chain([HvfRegister::PC, HvfRegister::CPSR])
                .collect::<Vec<_>>()
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn failed_arm64_general_register_capture_can_be_retried_without_stale_state() {
        let (runner, read_receiver) =
            start_general_register_capture_recording_runner(Some(HvfRegister::X2));

        assert_eq!(
            runner.capture_arm64_general_register_state(),
            Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                "fake general-register capture failed"
            )))
        );
        assert_eq!(
            read_receiver.try_iter().collect::<Vec<_>>(),
            vec![HvfRegister::X0, HvfRegister::X1, HvfRegister::X2]
        );

        let state = runner
            .capture_arm64_general_register_state()
            .expect("general-register capture retry should succeed");
        assert_eq!(state.general_purpose_register(2), Some(0x1002));
        assert_eq!(read_receiver.try_iter().count(), 33);
        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn captures_arm64_core_system_register_state_on_runner_thread() {
        let (runner, read_receiver) = start_core_system_register_capture_recording_runner(None);

        let state = runner
            .capture_arm64_core_system_register_state()
            .expect("core system-register capture should succeed");

        assert_eq!(
            state.sp_el0(),
            0x2_0000 + u64::from(HvfSystemRegister::SP_EL0.raw())
        );
        assert_eq!(
            state.sp_el1(),
            0x2_0000 + u64::from(HvfSystemRegister::SP_EL1.raw())
        );
        assert_eq!(
            state.elr_el1(),
            0x2_0000 + u64::from(HvfSystemRegister::ELR_EL1.raw())
        );
        assert_eq!(
            state.spsr_el1(),
            0x2_0000 + u64::from(HvfSystemRegister::SPSR_EL1.raw())
        );
        assert_eq!(
            read_receiver.try_iter().collect::<Vec<_>>(),
            [
                HvfSystemRegister::SP_EL0,
                HvfSystemRegister::SP_EL1,
                HvfSystemRegister::ELR_EL1,
                HvfSystemRegister::SPSR_EL1,
            ]
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn failed_arm64_core_system_register_capture_can_be_retried_without_stale_state() {
        let registers = [
            HvfSystemRegister::SP_EL0,
            HvfSystemRegister::SP_EL1,
            HvfSystemRegister::ELR_EL1,
            HvfSystemRegister::SPSR_EL1,
        ];

        for (failed_index, failed_register) in registers.into_iter().enumerate() {
            let (runner, read_receiver) =
                start_core_system_register_capture_recording_runner(Some(failed_register));

            assert_eq!(
                runner.capture_arm64_core_system_register_state(),
                Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                    "fake core system-register capture failed"
                )))
            );
            assert_eq!(
                read_receiver.try_iter().collect::<Vec<_>>(),
                registers[..=failed_index]
            );

            let state = runner
                .capture_arm64_core_system_register_state()
                .expect("core system-register capture retry should succeed");
            assert_eq!(
                state.sp_el0(),
                0x2_0000 + u64::from(HvfSystemRegister::SP_EL0.raw())
            );
            assert_eq!(
                state.sp_el1(),
                0x2_0000 + u64::from(HvfSystemRegister::SP_EL1.raw())
            );
            assert_eq!(
                state.elr_el1(),
                0x2_0000 + u64::from(HvfSystemRegister::ELR_EL1.raw())
            );
            assert_eq!(
                state.spsr_el1(),
                0x2_0000 + u64::from(HvfSystemRegister::SPSR_EL1.raw())
            );
            assert_eq!(read_receiver.try_iter().collect::<Vec<_>>(), registers);
            assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

            runner.shutdown().expect("runner should shut down");
        }
    }

    #[test]
    fn captures_arm64_exception_register_state_on_runner_thread() {
        let (runner, read_receiver) = start_exception_register_capture_recording_runner(None);

        let state = runner
            .capture_arm64_exception_register_state()
            .expect("exception-register capture should succeed");

        assert_exception_register_test_state(state);
        assert_eq!(
            read_receiver.try_iter().collect::<Vec<_>>(),
            exception_registers()
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn failed_arm64_exception_register_capture_can_be_retried_without_stale_state() {
        let registers = exception_registers();

        for (failed_index, failed_register) in registers.into_iter().enumerate() {
            let (runner, read_receiver) =
                start_exception_register_capture_recording_runner(Some(failed_register));

            assert_eq!(
                runner.capture_arm64_exception_register_state(),
                Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                    "fake exception-register capture failed"
                )))
            );
            assert_eq!(
                read_receiver.try_iter().collect::<Vec<_>>(),
                registers[..=failed_index]
            );

            let state = runner
                .capture_arm64_exception_register_state()
                .expect("exception-register capture retry should succeed");
            assert_exception_register_test_state(state);
            assert_eq!(read_receiver.try_iter().collect::<Vec<_>>(), registers);
            assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

            runner.shutdown().expect("runner should shut down");
        }
    }

    #[test]
    fn captures_arm64_execution_control_register_state_on_runner_thread() {
        let (runner, read_receiver) =
            start_execution_control_register_capture_recording_runner(None);

        let state = runner
            .capture_arm64_execution_control_register_state()
            .expect("execution-control capture should succeed");

        assert_execution_control_register_test_state(state);
        assert_eq!(
            read_receiver.try_iter().collect::<Vec<_>>(),
            execution_control_registers()
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn failed_arm64_execution_control_register_capture_can_retry_without_stale_state() {
        let registers = execution_control_registers();

        for (failed_index, failed_register) in registers.into_iter().enumerate() {
            let (runner, read_receiver) =
                start_execution_control_register_capture_recording_runner(Some(failed_register));

            assert_eq!(
                runner.capture_arm64_execution_control_register_state(),
                Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                    "fake execution-control capture failed"
                )))
            );
            assert_eq!(
                read_receiver.try_iter().collect::<Vec<_>>(),
                registers[..=failed_index]
            );

            let state = runner
                .capture_arm64_execution_control_register_state()
                .expect("execution-control capture retry should succeed");
            assert_execution_control_register_test_state(state);
            assert_eq!(read_receiver.try_iter().collect::<Vec<_>>(), registers);
            assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

            runner.shutdown().expect("runner should shut down");
        }
    }

    #[test]
    fn captures_arm64_identification_register_state_on_runner_thread() {
        let (runner, read_receiver) = start_identification_register_capture_recording_runner(None);

        let state = runner
            .capture_arm64_identification_register_state()
            .expect("identification-register capture should succeed");

        assert_identification_register_test_state(state);
        assert_eq!(
            read_receiver.try_iter().collect::<Vec<_>>(),
            identification_registers()
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn failed_arm64_identification_register_capture_can_retry_without_partial_state() {
        let registers = identification_registers();

        for (failed_index, failed_register) in registers.into_iter().enumerate() {
            let (runner, read_receiver) =
                start_identification_register_capture_recording_runner(Some(failed_register));

            assert_eq!(
                runner.capture_arm64_identification_register_state(),
                Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                    "fake identification-register capture failed"
                )))
            );
            assert_eq!(
                read_receiver.try_iter().collect::<Vec<_>>(),
                registers[..=failed_index]
            );

            let state = runner
                .capture_arm64_identification_register_state()
                .expect("identification-register capture retry should succeed");
            assert_identification_register_test_state(state);
            assert_eq!(read_receiver.try_iter().collect::<Vec<_>>(), registers);
            assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

            runner.shutdown().expect("runner should shut down");
        }
    }

    #[test]
    fn captures_arm64_translation_register_state_on_runner_thread() {
        let (runner, read_receiver) = start_translation_register_capture_recording_runner(None);

        let state = runner
            .capture_arm64_translation_register_state()
            .expect("translation-register capture should succeed");

        assert_translation_register_test_state(state);
        assert_eq!(
            read_receiver.try_iter().collect::<Vec<_>>(),
            translation_registers()
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn failed_arm64_translation_register_capture_can_be_retried_without_stale_state() {
        let registers = translation_registers();

        for (failed_index, failed_register) in registers.into_iter().enumerate() {
            let (runner, read_receiver) =
                start_translation_register_capture_recording_runner(Some(failed_register));

            assert_eq!(
                runner.capture_arm64_translation_register_state(),
                Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                    "fake translation-register capture failed"
                )))
            );
            assert_eq!(
                read_receiver.try_iter().collect::<Vec<_>>(),
                registers[..=failed_index]
            );

            let state = runner
                .capture_arm64_translation_register_state()
                .expect("translation-register capture retry should succeed");
            assert_translation_register_test_state(state);
            assert_eq!(read_receiver.try_iter().collect::<Vec<_>>(), registers);
            assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

            runner.shutdown().expect("runner should shut down");
        }
    }

    #[test]
    fn captures_arm64_pointer_authentication_keys_on_runner_thread() {
        let (runner, read_receiver) =
            start_pointer_authentication_key_capture_recording_runner(None);

        let state = runner
            .capture_arm64_pointer_authentication_key_state()
            .expect("pointer-authentication key capture should succeed");

        assert_pointer_authentication_key_test_state(&state);
        assert_eq!(
            read_receiver.try_iter().collect::<Vec<_>>(),
            pointer_authentication_key_registers()
        );
        assert_eq!(
            format!("{state:?}"),
            "HvfArm64VcpuPointerAuthenticationKeyState { keys: \"<redacted>\" }"
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn failed_arm64_pointer_authentication_key_capture_can_retry_without_partial_state() {
        let registers = pointer_authentication_key_registers();

        for (failed_index, failed_register) in registers.into_iter().enumerate() {
            let (runner, read_receiver) =
                start_pointer_authentication_key_capture_recording_runner(Some(failed_register));

            assert_eq!(
                runner.capture_arm64_pointer_authentication_key_state(),
                Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                    "fake pointer-authentication key capture failed"
                )))
            );
            assert_eq!(
                read_receiver.try_iter().collect::<Vec<_>>(),
                registers[..=failed_index]
            );

            let state = runner
                .capture_arm64_pointer_authentication_key_state()
                .expect("pointer-authentication key capture retry should succeed");
            assert_pointer_authentication_key_test_state(&state);
            assert_eq!(read_receiver.try_iter().collect::<Vec<_>>(), registers);
            assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

            runner.shutdown().expect("runner should shut down");
        }
    }

    #[test]
    fn captures_arm64_thread_context_register_state_on_runner_thread() {
        let (runner, read_receiver) = start_thread_context_register_capture_recording_runner(None);

        let state = runner
            .capture_arm64_thread_context_register_state()
            .expect("thread-context register capture should succeed");

        assert_eq!(
            state.tpidr_el0(),
            0x5_0000 + u64::from(HvfSystemRegister::TPIDR_EL0.raw())
        );
        assert_eq!(
            state.tpidrro_el0(),
            0x5_0000 + u64::from(HvfSystemRegister::TPIDRRO_EL0.raw())
        );
        assert_eq!(
            state.tpidr_el1(),
            0x5_0000 + u64::from(HvfSystemRegister::TPIDR_EL1.raw())
        );
        assert_eq!(
            read_receiver.try_iter().collect::<Vec<_>>(),
            [
                HvfSystemRegister::TPIDR_EL0,
                HvfSystemRegister::TPIDRRO_EL0,
                HvfSystemRegister::TPIDR_EL1,
            ]
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn failed_arm64_thread_context_register_capture_can_be_retried_without_stale_state() {
        let registers = [
            HvfSystemRegister::TPIDR_EL0,
            HvfSystemRegister::TPIDRRO_EL0,
            HvfSystemRegister::TPIDR_EL1,
        ];

        for (failed_index, failed_register) in registers.into_iter().enumerate() {
            let (runner, read_receiver) =
                start_thread_context_register_capture_recording_runner(Some(failed_register));

            assert_eq!(
                runner.capture_arm64_thread_context_register_state(),
                Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                    "fake thread-context register capture failed"
                )))
            );
            assert_eq!(
                read_receiver.try_iter().collect::<Vec<_>>(),
                registers[..=failed_index]
            );

            let state = runner
                .capture_arm64_thread_context_register_state()
                .expect("thread-context register capture retry should succeed");
            assert_eq!(
                state.tpidr_el0(),
                0x5_0000 + u64::from(HvfSystemRegister::TPIDR_EL0.raw())
            );
            assert_eq!(
                state.tpidrro_el0(),
                0x5_0000 + u64::from(HvfSystemRegister::TPIDRRO_EL0.raw())
            );
            assert_eq!(
                state.tpidr_el1(),
                0x5_0000 + u64::from(HvfSystemRegister::TPIDR_EL1.raw())
            );
            assert_eq!(read_receiver.try_iter().collect::<Vec<_>>(), registers);
            assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

            runner.shutdown().expect("runner should shut down");
        }
    }

    #[test]
    fn captures_arm64_simd_fp_state_on_runner_thread() {
        let (runner, read_receiver) = start_simd_fp_capture_recording_runner(None);

        let state = runner
            .capture_arm64_simd_fp_state()
            .expect("SIMD/FP capture should succeed");

        let q0 = HvfSimdFpRegister::q(0).expect("Q0 should map to a SIMD register");
        let q31 = HvfSimdFpRegister::q(31).expect("Q31 should map to a SIMD register");
        assert_eq!(state.q_register(0), Some(simd_fp_capture_q_value(q0)));
        assert_eq!(state.q_register(31), Some(simd_fp_capture_q_value(q31)));
        assert_eq!(state.q_register(32), None);
        assert_eq!(state.fpcr(), 0x4_0000 + u64::from(HvfRegister::FPCR.raw()));
        assert_eq!(state.fpsr(), 0x4_0000 + u64::from(HvfRegister::FPSR.raw()));
        assert_eq!(
            read_receiver.try_iter().collect::<Vec<_>>(),
            expected_simd_fp_capture_reads()
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn failed_arm64_simd_fp_capture_can_be_retried_without_stale_state() {
        let expected_reads = expected_simd_fp_capture_reads();

        for (failed_index, failed_read) in expected_reads.iter().copied().enumerate() {
            let (runner, read_receiver) = start_simd_fp_capture_recording_runner(Some(failed_read));

            assert_eq!(
                runner.capture_arm64_simd_fp_state(),
                Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                    "fake SIMD/FP capture failed"
                )))
            );
            assert_eq!(
                read_receiver.try_iter().collect::<Vec<_>>(),
                expected_reads[..=failed_index]
            );

            let state = runner
                .capture_arm64_simd_fp_state()
                .expect("SIMD/FP capture retry should succeed");
            assert_eq!(
                state.q_register(31),
                Some(simd_fp_capture_q_value(
                    HvfSimdFpRegister::q(31).expect("Q31 should map to a SIMD register")
                ))
            );
            assert_eq!(state.fpcr(), 0x4_0000 + u64::from(HvfRegister::FPCR.raw()));
            assert_eq!(state.fpsr(), 0x4_0000 + u64::from(HvfRegister::FPSR.raw()));
            assert_eq!(read_receiver.try_iter().collect::<Vec<_>>(), expected_reads);
            assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

            runner.shutdown().expect("runner should shut down");
        }
    }

    #[test]
    fn commands_during_arm64_general_register_capture_are_rejected_without_queueing() {
        let (runner, entered_capture_receiver, release_capture_sender, _barrier_receiver) =
            start_blocking_general_register_capture_runner();

        thread::scope(|scope| {
            let capture = scope.spawn(|| runner.capture_arm64_general_register_state());
            entered_capture_receiver
                .recv()
                .expect("runner should enter fake general-register capture");

            let expected =
                HvfVcpuRunnerError::InvalidState(super::CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE);
            assert_core_register_captures_rejected(&runner, expected.clone());
            assert_eq!(runner.run_once(), Err(expected.clone()));
            assert_eq!(
                runner.run_once_and_handle_mmio(shared_dispatcher()),
                Err(expected.clone())
            );
            assert_eq!(
                runner.dispatch_mmio_access(resolved_mmio_access(), shared_dispatcher()),
                Err(expected.clone())
            );
            assert_eq!(
                runner.configure_arm64_boot_registers(boot_registers()),
                Err(expected.clone())
            );
            assert_eq!(runner.mpidr_el1(), Err(expected.clone()));
            assert_timer_operations_rejected(&runner, expected.clone());
            assert_eq!(runner.set_gic_ppi_pending(27), Err(expected.clone()));
            assert_eq!(runner.clear_gic_ppi_pending(27), Err(expected.clone()));
            assert_eq!(runner.cancel(), Err(expected.clone()));
            assert_eq!(runner.shutdown(), Err(expected));

            release_capture_sender
                .send(Ok(()))
                .expect("general-register capture release should be sent");
            let state = capture
                .join()
                .expect("general-register capture thread should join")
                .expect("general-register capture should succeed");
            assert_eq!(state.general_purpose_register(30), Some(0x101e));
        });

        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn caller_unwind_keeps_arm64_general_register_capture_admitted_until_command_finishes() {
        let (runner, entered_capture_receiver, release_capture_sender, barrier_receiver) =
            start_blocking_general_register_capture_runner();

        let unwind_result = panic::catch_unwind(AssertUnwindSafe(|| {
            let (response_sender, _response_receiver) = mpsc::channel();
            runner
                .start_arm64_general_register_capture(response_sender)
                .expect("general-register capture should be admitted");
            panic!("fake caller unwind");
        }));
        assert!(unwind_result.is_err());
        entered_capture_receiver
            .recv()
            .expect("runner should enter fake general-register capture");
        assert_eq!(
            runner.cancel(),
            Err(HvfVcpuRunnerError::InvalidState(
                super::CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE
            ))
        );

        let (barrier_response_sender, barrier_response_receiver) = mpsc::channel();
        runner
            .command_sender
            .send(super::RunnerCommand::ReadMpidrEl1 {
                response_sender: barrier_response_sender,
            })
            .expect("test barrier should queue behind capture");
        release_capture_sender
            .send(Ok(()))
            .expect("general-register capture release should be sent");
        barrier_receiver
            .recv()
            .expect("runner should enter the command queued after capture");
        assert_eq!(
            barrier_response_receiver
                .recv()
                .expect("barrier response should be sent"),
            Ok(0x8000_0000)
        );
        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn arm64_general_register_capture_send_failure_releases_admission() {
        let (runner, runner_unwind_receiver) = start_panic_notifying_runner(|| Ok(PanicOnRunVcpu));

        assert_eq!(
            runner.run_once(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert_eq!(
            runner.capture_arm64_general_register_state(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::COMMAND_CHANNEL_CLOSED_MESSAGE
            ))
        );
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn shutdown_reports_thread_panic_after_arm64_general_register_capture_panic() {
        let (runner, runner_unwind_receiver) =
            start_panic_notifying_runner(|| Ok(PanicOnGeneralRegisterCaptureVcpu));

        assert_eq!(
            runner.capture_arm64_general_register_state(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn commands_during_arm64_core_system_register_capture_are_rejected_without_queueing() {
        let (runner, entered_capture_receiver, release_capture_sender, _barrier_receiver) =
            start_blocking_core_system_register_capture_runner();

        thread::scope(|scope| {
            let capture = scope.spawn(|| runner.capture_arm64_core_system_register_state());
            entered_capture_receiver
                .recv()
                .expect("runner should enter fake core system-register capture");

            let expected =
                HvfVcpuRunnerError::InvalidState(super::CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE);
            assert_core_register_captures_rejected(&runner, expected.clone());
            assert_eq!(runner.run_once(), Err(expected.clone()));
            assert_eq!(
                runner.run_once_and_handle_mmio(shared_dispatcher()),
                Err(expected.clone())
            );
            assert_eq!(
                runner.dispatch_mmio_access(resolved_mmio_access(), shared_dispatcher()),
                Err(expected.clone())
            );
            assert_eq!(
                runner.configure_arm64_boot_registers(boot_registers()),
                Err(expected.clone())
            );
            assert_eq!(runner.mpidr_el1(), Err(expected.clone()));
            assert_timer_operations_rejected(&runner, expected.clone());
            assert_eq!(runner.set_gic_ppi_pending(27), Err(expected.clone()));
            assert_eq!(runner.clear_gic_ppi_pending(27), Err(expected.clone()));
            assert_eq!(runner.cancel(), Err(expected.clone()));
            assert_eq!(runner.shutdown(), Err(expected));

            release_capture_sender
                .send(Ok(()))
                .expect("core system-register capture release should be sent");
            let state = capture
                .join()
                .expect("core system-register capture thread should join")
                .expect("core system-register capture should succeed");
            assert_eq!(
                state.spsr_el1(),
                0x2_0000 + u64::from(HvfSystemRegister::SPSR_EL1.raw())
            );
        });

        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn caller_unwind_keeps_arm64_core_system_register_capture_admitted_until_command_finishes() {
        let (runner, entered_capture_receiver, release_capture_sender, barrier_receiver) =
            start_blocking_core_system_register_capture_runner();

        let unwind_result = panic::catch_unwind(AssertUnwindSafe(|| {
            let (response_sender, _response_receiver) = mpsc::channel();
            runner
                .start_arm64_core_system_register_capture(response_sender)
                .expect("core system-register capture should be admitted");
            panic!("fake caller unwind");
        }));
        assert!(unwind_result.is_err());
        entered_capture_receiver
            .recv()
            .expect("runner should enter fake core system-register capture");
        assert_eq!(
            runner.cancel(),
            Err(HvfVcpuRunnerError::InvalidState(
                super::CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE
            ))
        );

        let (barrier_response_sender, barrier_response_receiver) = mpsc::channel();
        runner
            .command_sender
            .send(super::RunnerCommand::ReadMpidrEl1 {
                response_sender: barrier_response_sender,
            })
            .expect("test barrier should queue behind capture");
        release_capture_sender
            .send(Ok(()))
            .expect("core system-register capture release should be sent");
        barrier_receiver
            .recv()
            .expect("runner should enter the command queued after capture");
        assert_eq!(
            barrier_response_receiver
                .recv()
                .expect("barrier response should be sent"),
            Ok(0x8000_0000)
        );
        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn arm64_core_system_register_capture_send_failure_releases_admission() {
        let (runner, runner_unwind_receiver) = start_panic_notifying_runner(|| Ok(PanicOnRunVcpu));

        assert_eq!(
            runner.run_once(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert_eq!(
            runner.capture_arm64_core_system_register_state(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::COMMAND_CHANNEL_CLOSED_MESSAGE
            ))
        );
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn queued_arm64_core_system_register_capture_destruction_releases_admission() {
        let (entered_run_sender, entered_run_receiver) = mpsc::channel();
        let (release_run_sender, release_run_receiver) = mpsc::channel();
        let (runner, runner_unwind_receiver) = start_panic_notifying_runner(move || {
            Ok(BlockingPanicOnRunVcpu {
                entered_run_sender,
                release_run_receiver,
            })
        });

        let (run_response_sender, run_response_receiver) = mpsc::channel();
        runner
            .command_sender
            .send(super::RunnerCommand::RunOnce {
                response_sender: run_response_sender,
            })
            .expect("raw panic command should be sent");
        entered_run_receiver
            .recv()
            .expect("runner should enter the blocking panic command");

        let (capture_response_sender, capture_response_receiver) = mpsc::channel();
        runner
            .start_arm64_core_system_register_capture(capture_response_sender)
            .expect("core system-register capture should queue behind the panic command");
        assert!(
            runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );

        release_run_sender
            .send(())
            .expect("blocking panic command should be released");
        assert!(run_response_receiver.recv().is_err());
        assert!(capture_response_receiver.recv().is_err());
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn shutdown_reports_thread_panic_after_arm64_core_system_register_capture_panic() {
        let (runner, runner_unwind_receiver) =
            start_panic_notifying_runner(|| Ok(PanicOnCoreSystemRegisterCaptureVcpu));

        assert_eq!(
            runner.capture_arm64_core_system_register_state(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn commands_during_arm64_exception_register_capture_are_rejected_without_queueing() {
        let (runner, entered_capture_receiver, release_capture_sender, _barrier_receiver) =
            start_blocking_exception_register_capture_runner();

        thread::scope(|scope| {
            let capture = scope.spawn(|| runner.capture_arm64_exception_register_state());
            entered_capture_receiver
                .recv()
                .expect("runner should enter fake exception-register capture");

            let expected =
                HvfVcpuRunnerError::InvalidState(super::CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE);
            assert_core_register_captures_rejected(&runner, expected.clone());
            assert_eq!(runner.run_once(), Err(expected.clone()));
            assert_eq!(
                runner.run_once_and_handle_mmio(shared_dispatcher()),
                Err(expected.clone())
            );
            assert_eq!(
                runner.dispatch_mmio_access(resolved_mmio_access(), shared_dispatcher()),
                Err(expected.clone())
            );
            assert_eq!(
                runner.configure_arm64_boot_registers(boot_registers()),
                Err(expected.clone())
            );
            assert_eq!(runner.mpidr_el1(), Err(expected.clone()));
            assert_timer_operations_rejected(&runner, expected.clone());
            assert_interrupt_operations_rejected(&runner, expected.clone());
            assert_eq!(runner.cancel(), Err(expected.clone()));
            assert_eq!(runner.shutdown(), Err(expected));

            release_capture_sender
                .send(Ok(()))
                .expect("exception-register capture release should be sent");
            let state = capture
                .join()
                .expect("exception-register capture thread should join")
                .expect("exception-register capture should succeed");
            assert_exception_register_test_state(state);
        });

        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn caller_unwind_keeps_arm64_exception_register_capture_admitted_until_command_finishes() {
        let (runner, entered_capture_receiver, release_capture_sender, barrier_receiver) =
            start_blocking_exception_register_capture_runner();

        let unwind_result = panic::catch_unwind(AssertUnwindSafe(|| {
            let (response_sender, _response_receiver) = mpsc::channel();
            runner
                .start_arm64_exception_register_capture(response_sender)
                .expect("exception-register capture should be admitted");
            panic!("fake caller unwind");
        }));
        assert!(unwind_result.is_err());
        entered_capture_receiver
            .recv()
            .expect("runner should enter fake exception-register capture");
        assert_eq!(
            runner.cancel(),
            Err(HvfVcpuRunnerError::InvalidState(
                super::CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE
            ))
        );

        let (barrier_response_sender, barrier_response_receiver) = mpsc::channel();
        runner
            .command_sender
            .send(super::RunnerCommand::ReadMpidrEl1 {
                response_sender: barrier_response_sender,
            })
            .expect("test barrier should queue behind capture");
        release_capture_sender
            .send(Ok(()))
            .expect("exception-register capture release should be sent");
        barrier_receiver
            .recv()
            .expect("runner should enter the command queued after capture");
        assert_eq!(
            barrier_response_receiver
                .recv()
                .expect("barrier response should be sent"),
            Ok(0x8000_0000)
        );
        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn arm64_exception_register_capture_send_failure_releases_admission() {
        let (runner, runner_unwind_receiver) = start_panic_notifying_runner(|| Ok(PanicOnRunVcpu));

        assert_eq!(
            runner.run_once(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert_eq!(
            runner.capture_arm64_exception_register_state(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::COMMAND_CHANNEL_CLOSED_MESSAGE
            ))
        );
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn queued_arm64_exception_register_capture_destruction_releases_admission() {
        let (entered_run_sender, entered_run_receiver) = mpsc::channel();
        let (release_run_sender, release_run_receiver) = mpsc::channel();
        let (runner, runner_unwind_receiver) = start_panic_notifying_runner(move || {
            Ok(BlockingPanicOnRunVcpu {
                entered_run_sender,
                release_run_receiver,
            })
        });

        let (run_response_sender, run_response_receiver) = mpsc::channel();
        runner
            .command_sender
            .send(super::RunnerCommand::RunOnce {
                response_sender: run_response_sender,
            })
            .expect("raw panic command should be sent");
        entered_run_receiver
            .recv()
            .expect("runner should enter the blocking panic command");

        let (capture_response_sender, capture_response_receiver) = mpsc::channel();
        runner
            .start_arm64_exception_register_capture(capture_response_sender)
            .expect("exception-register capture should queue behind the panic command");
        assert!(
            runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );

        release_run_sender
            .send(())
            .expect("blocking panic command should be released");
        assert!(run_response_receiver.recv().is_err());
        assert!(capture_response_receiver.recv().is_err());
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn shutdown_reports_thread_panic_after_arm64_exception_register_capture_panic() {
        let (runner, runner_unwind_receiver) =
            start_panic_notifying_runner(|| Ok(PanicOnExceptionRegisterCaptureVcpu));

        assert_eq!(
            runner.capture_arm64_exception_register_state(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn commands_during_arm64_execution_control_capture_are_rejected_without_queueing() {
        let (runner, entered_capture_receiver, release_capture_sender, _barrier_receiver) =
            start_blocking_execution_control_register_capture_runner();

        thread::scope(|scope| {
            let capture = scope.spawn(|| runner.capture_arm64_execution_control_register_state());
            entered_capture_receiver
                .recv()
                .expect("runner should enter fake execution-control capture");

            let expected =
                HvfVcpuRunnerError::InvalidState(super::CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE);
            assert_core_register_captures_rejected(&runner, expected.clone());
            assert_eq!(runner.run_once(), Err(expected.clone()));
            assert_eq!(
                runner.run_once_and_handle_mmio(shared_dispatcher()),
                Err(expected.clone())
            );
            assert_eq!(
                runner.dispatch_mmio_access(resolved_mmio_access(), shared_dispatcher()),
                Err(expected.clone())
            );
            assert_eq!(
                runner.configure_arm64_boot_registers(boot_registers()),
                Err(expected.clone())
            );
            assert_eq!(runner.mpidr_el1(), Err(expected.clone()));
            assert_timer_operations_rejected(&runner, expected.clone());
            assert_interrupt_operations_rejected(&runner, expected.clone());
            assert_eq!(runner.cancel(), Err(expected.clone()));
            assert_eq!(runner.shutdown(), Err(expected));

            release_capture_sender
                .send(Ok(()))
                .expect("execution-control capture release should be sent");
            let state = capture
                .join()
                .expect("execution-control capture thread should join")
                .expect("execution-control capture should succeed");
            assert_execution_control_register_test_state(state);
        });

        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn caller_unwind_keeps_execution_control_capture_admitted_until_command_finishes() {
        let (runner, entered_capture_receiver, release_capture_sender, barrier_receiver) =
            start_blocking_execution_control_register_capture_runner();

        let unwind_result = panic::catch_unwind(AssertUnwindSafe(|| {
            let (response_sender, _response_receiver) = mpsc::channel();
            runner
                .start_arm64_execution_control_register_capture(response_sender)
                .expect("execution-control capture should be admitted");
            panic!("fake caller unwind");
        }));
        assert!(unwind_result.is_err());
        entered_capture_receiver
            .recv()
            .expect("runner should enter fake execution-control capture");
        assert_eq!(
            runner.cancel(),
            Err(HvfVcpuRunnerError::InvalidState(
                super::CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE
            ))
        );

        let (barrier_response_sender, barrier_response_receiver) = mpsc::channel();
        runner
            .command_sender
            .send(super::RunnerCommand::ReadMpidrEl1 {
                response_sender: barrier_response_sender,
            })
            .expect("test barrier should queue behind capture");
        release_capture_sender
            .send(Ok(()))
            .expect("execution-control capture release should be sent");
        barrier_receiver
            .recv()
            .expect("runner should enter the command queued after capture");
        assert_eq!(
            barrier_response_receiver
                .recv()
                .expect("barrier response should be sent"),
            Ok(0x8000_0000)
        );
        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn arm64_execution_control_capture_send_failure_releases_admission() {
        let (runner, runner_unwind_receiver) = start_panic_notifying_runner(|| Ok(PanicOnRunVcpu));

        assert_eq!(
            runner.run_once(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert_eq!(
            runner.capture_arm64_execution_control_register_state(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::COMMAND_CHANNEL_CLOSED_MESSAGE
            ))
        );
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn queued_execution_control_capture_destruction_releases_admission() {
        let (entered_run_sender, entered_run_receiver) = mpsc::channel();
        let (release_run_sender, release_run_receiver) = mpsc::channel();
        let (runner, runner_unwind_receiver) = start_panic_notifying_runner(move || {
            Ok(BlockingPanicOnRunVcpu {
                entered_run_sender,
                release_run_receiver,
            })
        });

        let (run_response_sender, run_response_receiver) = mpsc::channel();
        runner
            .command_sender
            .send(super::RunnerCommand::RunOnce {
                response_sender: run_response_sender,
            })
            .expect("raw panic command should be sent");
        entered_run_receiver
            .recv()
            .expect("runner should enter the blocking panic command");

        let (capture_response_sender, capture_response_receiver) = mpsc::channel();
        runner
            .start_arm64_execution_control_register_capture(capture_response_sender)
            .expect("execution-control capture should queue behind the panic command");
        assert!(
            runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );

        release_run_sender
            .send(())
            .expect("blocking panic command should be released");
        assert!(run_response_receiver.recv().is_err());
        assert!(capture_response_receiver.recv().is_err());
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn shutdown_reports_thread_panic_after_execution_control_capture_panic() {
        let (runner, runner_unwind_receiver) =
            start_panic_notifying_runner(|| Ok(PanicOnExecutionControlRegisterCaptureVcpu));

        assert_eq!(
            runner.capture_arm64_execution_control_register_state(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn commands_during_arm64_identification_capture_are_rejected_without_queueing() {
        let (runner, entered_capture_receiver, release_capture_sender, _barrier_receiver) =
            start_blocking_identification_register_capture_runner();

        thread::scope(|scope| {
            let capture = scope.spawn(|| runner.capture_arm64_identification_register_state());
            entered_capture_receiver
                .recv()
                .expect("runner should enter fake identification-register capture");

            let expected =
                HvfVcpuRunnerError::InvalidState(super::CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE);
            assert_core_register_captures_rejected(&runner, expected.clone());
            assert_eq!(runner.run_once(), Err(expected.clone()));
            assert_eq!(
                runner.run_once_and_handle_mmio(shared_dispatcher()),
                Err(expected.clone())
            );
            assert_eq!(
                runner.dispatch_mmio_access(resolved_mmio_access(), shared_dispatcher()),
                Err(expected.clone())
            );
            assert_eq!(
                runner.configure_arm64_boot_registers(boot_registers()),
                Err(expected.clone())
            );
            assert_eq!(runner.mpidr_el1(), Err(expected.clone()));
            assert_timer_operations_rejected(&runner, expected.clone());
            assert_interrupt_operations_rejected(&runner, expected.clone());
            assert_eq!(runner.cancel(), Err(expected.clone()));
            assert_eq!(runner.shutdown(), Err(expected));

            release_capture_sender
                .send(Ok(()))
                .expect("identification-register capture release should be sent");
            let state = capture
                .join()
                .expect("identification-register capture thread should join")
                .expect("identification-register capture should succeed");
            assert_identification_register_test_state(state);
        });

        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn caller_unwind_keeps_identification_capture_admitted_until_command_finishes() {
        let (runner, entered_capture_receiver, release_capture_sender, barrier_receiver) =
            start_blocking_identification_register_capture_runner();

        let unwind_result = panic::catch_unwind(AssertUnwindSafe(|| {
            let (response_sender, _response_receiver) = mpsc::channel();
            runner
                .start_arm64_identification_register_capture(response_sender)
                .expect("identification-register capture should be admitted");
            panic!("fake caller unwind");
        }));
        assert!(unwind_result.is_err());
        entered_capture_receiver
            .recv()
            .expect("runner should enter fake identification-register capture");
        assert_eq!(
            runner.cancel(),
            Err(HvfVcpuRunnerError::InvalidState(
                super::CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE
            ))
        );

        let (barrier_response_sender, barrier_response_receiver) = mpsc::channel();
        runner
            .command_sender
            .send(super::RunnerCommand::ReadMpidrEl1 {
                response_sender: barrier_response_sender,
            })
            .expect("test barrier should queue behind capture");
        release_capture_sender
            .send(Ok(()))
            .expect("identification-register capture release should be sent");
        barrier_receiver
            .recv()
            .expect("runner should enter the command queued after capture");
        assert_eq!(
            barrier_response_receiver
                .recv()
                .expect("barrier response should be sent"),
            Ok(0x8000_0000)
        );
        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn arm64_identification_capture_send_failure_releases_admission() {
        let (runner, runner_unwind_receiver) = start_panic_notifying_runner(|| Ok(PanicOnRunVcpu));

        assert_eq!(
            runner.run_once(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert_eq!(
            runner.capture_arm64_identification_register_state(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::COMMAND_CHANNEL_CLOSED_MESSAGE
            ))
        );
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn queued_identification_capture_destruction_releases_admission() {
        let (entered_run_sender, entered_run_receiver) = mpsc::channel();
        let (release_run_sender, release_run_receiver) = mpsc::channel();
        let (runner, runner_unwind_receiver) = start_panic_notifying_runner(move || {
            Ok(BlockingPanicOnRunVcpu {
                entered_run_sender,
                release_run_receiver,
            })
        });

        let (run_response_sender, run_response_receiver) = mpsc::channel();
        runner
            .command_sender
            .send(super::RunnerCommand::RunOnce {
                response_sender: run_response_sender,
            })
            .expect("raw panic command should be sent");
        entered_run_receiver
            .recv()
            .expect("runner should enter the blocking panic command");

        let (capture_response_sender, capture_response_receiver) = mpsc::channel();
        runner
            .start_arm64_identification_register_capture(capture_response_sender)
            .expect("identification-register capture should queue behind the panic command");
        assert!(
            runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );

        release_run_sender
            .send(())
            .expect("blocking panic command should be released");
        assert!(run_response_receiver.recv().is_err());
        assert!(capture_response_receiver.recv().is_err());
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn shutdown_reports_thread_panic_after_identification_capture_panic() {
        let (runner, runner_unwind_receiver) =
            start_panic_notifying_runner(|| Ok(PanicOnIdentificationRegisterCaptureVcpu));

        assert_eq!(
            runner.capture_arm64_identification_register_state(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn commands_during_arm64_translation_register_capture_are_rejected_without_queueing() {
        let (runner, entered_capture_receiver, release_capture_sender, _barrier_receiver) =
            start_blocking_translation_register_capture_runner();

        thread::scope(|scope| {
            let capture = scope.spawn(|| runner.capture_arm64_translation_register_state());
            entered_capture_receiver
                .recv()
                .expect("runner should enter fake translation-register capture");

            let expected =
                HvfVcpuRunnerError::InvalidState(super::CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE);
            assert_core_register_captures_rejected(&runner, expected.clone());
            assert_eq!(runner.run_once(), Err(expected.clone()));
            assert_eq!(
                runner.run_once_and_handle_mmio(shared_dispatcher()),
                Err(expected.clone())
            );
            assert_eq!(
                runner.dispatch_mmio_access(resolved_mmio_access(), shared_dispatcher()),
                Err(expected.clone())
            );
            assert_eq!(
                runner.configure_arm64_boot_registers(boot_registers()),
                Err(expected.clone())
            );
            assert_eq!(runner.mpidr_el1(), Err(expected.clone()));
            assert_timer_operations_rejected(&runner, expected.clone());
            assert_interrupt_operations_rejected(&runner, expected.clone());
            assert_eq!(runner.cancel(), Err(expected.clone()));
            assert_eq!(runner.shutdown(), Err(expected));

            release_capture_sender
                .send(Ok(()))
                .expect("translation-register capture release should be sent");
            let state = capture
                .join()
                .expect("translation-register capture thread should join")
                .expect("translation-register capture should succeed");
            assert_translation_register_test_state(state);
        });

        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn caller_unwind_keeps_arm64_translation_register_capture_admitted_until_command_finishes() {
        let (runner, entered_capture_receiver, release_capture_sender, barrier_receiver) =
            start_blocking_translation_register_capture_runner();

        let unwind_result = panic::catch_unwind(AssertUnwindSafe(|| {
            let (response_sender, _response_receiver) = mpsc::channel();
            runner
                .start_arm64_translation_register_capture(response_sender)
                .expect("translation-register capture should be admitted");
            panic!("fake caller unwind");
        }));
        assert!(unwind_result.is_err());
        entered_capture_receiver
            .recv()
            .expect("runner should enter fake translation-register capture");
        assert_eq!(
            runner.cancel(),
            Err(HvfVcpuRunnerError::InvalidState(
                super::CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE
            ))
        );

        let (barrier_response_sender, barrier_response_receiver) = mpsc::channel();
        runner
            .command_sender
            .send(super::RunnerCommand::ReadMpidrEl1 {
                response_sender: barrier_response_sender,
            })
            .expect("test barrier should queue behind capture");
        release_capture_sender
            .send(Ok(()))
            .expect("translation-register capture release should be sent");
        barrier_receiver
            .recv()
            .expect("runner should enter the command queued after capture");
        assert_eq!(
            barrier_response_receiver
                .recv()
                .expect("barrier response should be sent"),
            Ok(0x8000_0000)
        );
        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn arm64_translation_register_capture_send_failure_releases_admission() {
        let (runner, runner_unwind_receiver) = start_panic_notifying_runner(|| Ok(PanicOnRunVcpu));

        assert_eq!(
            runner.run_once(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert_eq!(
            runner.capture_arm64_translation_register_state(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::COMMAND_CHANNEL_CLOSED_MESSAGE
            ))
        );
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn queued_arm64_translation_register_capture_destruction_releases_admission() {
        let (entered_run_sender, entered_run_receiver) = mpsc::channel();
        let (release_run_sender, release_run_receiver) = mpsc::channel();
        let (runner, runner_unwind_receiver) = start_panic_notifying_runner(move || {
            Ok(BlockingPanicOnRunVcpu {
                entered_run_sender,
                release_run_receiver,
            })
        });

        let (run_response_sender, run_response_receiver) = mpsc::channel();
        runner
            .command_sender
            .send(super::RunnerCommand::RunOnce {
                response_sender: run_response_sender,
            })
            .expect("raw panic command should be sent");
        entered_run_receiver
            .recv()
            .expect("runner should enter the blocking panic command");

        let (capture_response_sender, capture_response_receiver) = mpsc::channel();
        runner
            .start_arm64_translation_register_capture(capture_response_sender)
            .expect("translation-register capture should queue behind the panic command");
        assert!(
            runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );

        release_run_sender
            .send(())
            .expect("blocking panic command should be released");
        assert!(run_response_receiver.recv().is_err());
        assert!(capture_response_receiver.recv().is_err());
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn shutdown_reports_thread_panic_after_arm64_translation_register_capture_panic() {
        let (runner, runner_unwind_receiver) =
            start_panic_notifying_runner(|| Ok(PanicOnTranslationRegisterCaptureVcpu));

        assert_eq!(
            runner.capture_arm64_translation_register_state(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn commands_during_arm64_pointer_authentication_key_capture_are_rejected_without_queueing() {
        let (runner, entered_capture_receiver, release_capture_sender, _barrier_receiver) =
            start_blocking_pointer_authentication_key_capture_runner();

        thread::scope(|scope| {
            let capture = scope.spawn(|| runner.capture_arm64_pointer_authentication_key_state());
            entered_capture_receiver
                .recv()
                .expect("runner should enter fake pointer-authentication key capture");

            let expected =
                HvfVcpuRunnerError::InvalidState(super::CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE);
            assert_core_register_captures_rejected(&runner, expected.clone());
            assert_eq!(runner.run_once(), Err(expected.clone()));
            assert_eq!(
                runner.run_once_and_handle_mmio(shared_dispatcher()),
                Err(expected.clone())
            );
            assert_eq!(
                runner.dispatch_mmio_access(resolved_mmio_access(), shared_dispatcher()),
                Err(expected.clone())
            );
            assert_eq!(
                runner.configure_arm64_boot_registers(boot_registers()),
                Err(expected.clone())
            );
            assert_eq!(runner.mpidr_el1(), Err(expected.clone()));
            assert_timer_operations_rejected(&runner, expected.clone());
            assert_interrupt_operations_rejected(&runner, expected.clone());
            assert_eq!(runner.cancel(), Err(expected.clone()));
            assert_eq!(runner.shutdown(), Err(expected));

            release_capture_sender
                .send(Ok(()))
                .expect("pointer-authentication key capture release should be sent");
            let state = capture
                .join()
                .expect("pointer-authentication key capture thread should join")
                .expect("pointer-authentication key capture should succeed");
            assert_pointer_authentication_key_test_state(&state);
        });

        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn caller_unwind_keeps_arm64_pointer_authentication_key_capture_admitted_until_finish() {
        let (runner, entered_capture_receiver, release_capture_sender, barrier_receiver) =
            start_blocking_pointer_authentication_key_capture_runner();

        let unwind_result = panic::catch_unwind(AssertUnwindSafe(|| {
            let (response_sender, _response_receiver) = mpsc::channel();
            runner
                .start_arm64_pointer_authentication_key_capture(response_sender)
                .expect("pointer-authentication key capture should be admitted");
            panic!("fake caller unwind");
        }));
        assert!(unwind_result.is_err());
        entered_capture_receiver
            .recv()
            .expect("runner should enter fake pointer-authentication key capture");
        assert_eq!(
            runner.cancel(),
            Err(HvfVcpuRunnerError::InvalidState(
                super::CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE
            ))
        );

        let (barrier_response_sender, barrier_response_receiver) = mpsc::channel();
        runner
            .command_sender
            .send(super::RunnerCommand::ReadMpidrEl1 {
                response_sender: barrier_response_sender,
            })
            .expect("test barrier should queue behind capture");
        release_capture_sender
            .send(Ok(()))
            .expect("pointer-authentication key capture release should be sent");
        barrier_receiver
            .recv()
            .expect("runner should enter the command queued after capture");
        assert_eq!(
            barrier_response_receiver
                .recv()
                .expect("barrier response should be sent"),
            Ok(0x8000_0000)
        );
        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn arm64_pointer_authentication_key_capture_send_failure_releases_admission() {
        let (runner, runner_unwind_receiver) = start_panic_notifying_runner(|| Ok(PanicOnRunVcpu));

        assert_eq!(
            runner.run_once(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert_eq!(
            runner.capture_arm64_pointer_authentication_key_state(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::COMMAND_CHANNEL_CLOSED_MESSAGE
            ))
        );
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn queued_arm64_pointer_authentication_key_capture_destruction_releases_admission() {
        let (entered_run_sender, entered_run_receiver) = mpsc::channel();
        let (release_run_sender, release_run_receiver) = mpsc::channel();
        let (runner, runner_unwind_receiver) = start_panic_notifying_runner(move || {
            Ok(BlockingPanicOnRunVcpu {
                entered_run_sender,
                release_run_receiver,
            })
        });

        let (run_response_sender, run_response_receiver) = mpsc::channel();
        runner
            .command_sender
            .send(super::RunnerCommand::RunOnce {
                response_sender: run_response_sender,
            })
            .expect("raw panic command should be sent");
        entered_run_receiver
            .recv()
            .expect("runner should enter the blocking panic command");

        let (capture_response_sender, capture_response_receiver) = mpsc::channel();
        runner
            .start_arm64_pointer_authentication_key_capture(capture_response_sender)
            .expect("pointer-authentication key capture should queue behind panic");
        assert!(
            runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );

        release_run_sender
            .send(())
            .expect("blocking panic command should be released");
        assert!(run_response_receiver.recv().is_err());
        assert!(capture_response_receiver.recv().is_err());
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn shutdown_reports_thread_panic_after_arm64_pointer_authentication_key_capture_panic() {
        let (runner, runner_unwind_receiver) =
            start_panic_notifying_runner(|| Ok(PanicOnPointerAuthenticationKeyCaptureVcpu));

        assert_eq!(
            runner.capture_arm64_pointer_authentication_key_state(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn commands_during_arm64_thread_context_register_capture_are_rejected_without_queueing() {
        let (runner, entered_capture_receiver, release_capture_sender, _barrier_receiver) =
            start_blocking_thread_context_register_capture_runner();

        thread::scope(|scope| {
            let capture = scope.spawn(|| runner.capture_arm64_thread_context_register_state());
            entered_capture_receiver
                .recv()
                .expect("runner should enter fake thread-context register capture");

            let expected =
                HvfVcpuRunnerError::InvalidState(super::CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE);
            assert_core_register_captures_rejected(&runner, expected.clone());
            assert_eq!(runner.run_once(), Err(expected.clone()));
            assert_eq!(
                runner.run_once_and_handle_mmio(shared_dispatcher()),
                Err(expected.clone())
            );
            assert_eq!(
                runner.dispatch_mmio_access(resolved_mmio_access(), shared_dispatcher()),
                Err(expected.clone())
            );
            assert_eq!(
                runner.configure_arm64_boot_registers(boot_registers()),
                Err(expected.clone())
            );
            assert_eq!(runner.mpidr_el1(), Err(expected.clone()));
            assert_timer_operations_rejected(&runner, expected.clone());
            assert_interrupt_operations_rejected(&runner, expected.clone());
            assert_eq!(runner.cancel(), Err(expected.clone()));
            assert_eq!(runner.shutdown(), Err(expected));

            release_capture_sender
                .send(Ok(()))
                .expect("thread-context register capture release should be sent");
            let state = capture
                .join()
                .expect("thread-context register capture thread should join")
                .expect("thread-context register capture should succeed");
            assert_eq!(
                state.tpidr_el1(),
                0x5_0000 + u64::from(HvfSystemRegister::TPIDR_EL1.raw())
            );
        });

        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn caller_unwind_keeps_arm64_thread_context_capture_admitted_until_command_finishes() {
        let (runner, entered_capture_receiver, release_capture_sender, barrier_receiver) =
            start_blocking_thread_context_register_capture_runner();

        let unwind_result = panic::catch_unwind(AssertUnwindSafe(|| {
            let (response_sender, _response_receiver) = mpsc::channel();
            runner
                .start_arm64_thread_context_register_capture(response_sender)
                .expect("thread-context register capture should be admitted");
            panic!("fake caller unwind");
        }));
        assert!(unwind_result.is_err());
        entered_capture_receiver
            .recv()
            .expect("runner should enter fake thread-context register capture");
        assert_eq!(
            runner.cancel(),
            Err(HvfVcpuRunnerError::InvalidState(
                super::CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE
            ))
        );

        let (barrier_response_sender, barrier_response_receiver) = mpsc::channel();
        runner
            .command_sender
            .send(super::RunnerCommand::ReadMpidrEl1 {
                response_sender: barrier_response_sender,
            })
            .expect("test barrier should queue behind capture");
        release_capture_sender
            .send(Ok(()))
            .expect("thread-context register capture release should be sent");
        barrier_receiver
            .recv()
            .expect("runner should enter the command queued after capture");
        assert_eq!(
            barrier_response_receiver
                .recv()
                .expect("barrier response should be sent"),
            Ok(0x8000_0000)
        );
        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn arm64_thread_context_register_capture_send_failure_releases_admission() {
        let (runner, runner_unwind_receiver) = start_panic_notifying_runner(|| Ok(PanicOnRunVcpu));

        assert_eq!(
            runner.run_once(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert_eq!(
            runner.capture_arm64_thread_context_register_state(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::COMMAND_CHANNEL_CLOSED_MESSAGE
            ))
        );
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn queued_arm64_thread_context_capture_destruction_releases_admission() {
        let (entered_run_sender, entered_run_receiver) = mpsc::channel();
        let (release_run_sender, release_run_receiver) = mpsc::channel();
        let (runner, runner_unwind_receiver) = start_panic_notifying_runner(move || {
            Ok(BlockingPanicOnRunVcpu {
                entered_run_sender,
                release_run_receiver,
            })
        });

        let (run_response_sender, run_response_receiver) = mpsc::channel();
        runner
            .command_sender
            .send(super::RunnerCommand::RunOnce {
                response_sender: run_response_sender,
            })
            .expect("raw panic command should be sent");
        entered_run_receiver
            .recv()
            .expect("runner should enter the blocking panic command");

        let (capture_response_sender, capture_response_receiver) = mpsc::channel();
        runner
            .start_arm64_thread_context_register_capture(capture_response_sender)
            .expect("thread-context capture should queue behind the panic command");
        assert!(
            runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );

        release_run_sender
            .send(())
            .expect("blocking panic command should be released");
        assert!(run_response_receiver.recv().is_err());
        assert!(capture_response_receiver.recv().is_err());
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn thread_context_capture_panic_releases_admission() {
        let (runner, runner_unwind_receiver) =
            start_panic_notifying_runner(|| Ok(PanicOnThreadContextRegisterCaptureVcpu));

        assert_eq!(
            runner.capture_arm64_thread_context_register_state(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn commands_during_arm64_simd_fp_capture_are_rejected_without_queueing() {
        let (runner, entered_capture_receiver, release_capture_sender, _barrier_receiver) =
            start_blocking_simd_fp_capture_runner();

        thread::scope(|scope| {
            let capture = scope.spawn(|| runner.capture_arm64_simd_fp_state());
            entered_capture_receiver
                .recv()
                .expect("runner should enter fake SIMD/FP capture");

            let expected =
                HvfVcpuRunnerError::InvalidState(super::CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE);
            assert_core_register_captures_rejected(&runner, expected.clone());
            assert_eq!(runner.run_once(), Err(expected.clone()));
            assert_eq!(
                runner.run_once_and_handle_mmio(shared_dispatcher()),
                Err(expected.clone())
            );
            assert_eq!(
                runner.dispatch_mmio_access(resolved_mmio_access(), shared_dispatcher()),
                Err(expected.clone())
            );
            assert_eq!(
                runner.configure_arm64_boot_registers(boot_registers()),
                Err(expected.clone())
            );
            assert_eq!(runner.mpidr_el1(), Err(expected.clone()));
            assert_timer_operations_rejected(&runner, expected.clone());
            assert_eq!(runner.set_gic_ppi_pending(27), Err(expected.clone()));
            assert_eq!(runner.clear_gic_ppi_pending(27), Err(expected.clone()));
            assert_eq!(runner.cancel(), Err(expected.clone()));
            assert_eq!(runner.shutdown(), Err(expected));

            release_capture_sender
                .send(Ok(()))
                .expect("SIMD/FP capture release should be sent");
            let state = capture
                .join()
                .expect("SIMD/FP capture thread should join")
                .expect("SIMD/FP capture should succeed");
            assert_eq!(
                state.q_register(31),
                Some(simd_fp_capture_q_value(
                    HvfSimdFpRegister::q(31).expect("Q31 should map to a SIMD register")
                ))
            );
        });

        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn caller_unwind_keeps_arm64_simd_fp_capture_admitted_until_command_finishes() {
        let (runner, entered_capture_receiver, release_capture_sender, barrier_receiver) =
            start_blocking_simd_fp_capture_runner();

        let unwind_result = panic::catch_unwind(AssertUnwindSafe(|| {
            let (response_sender, _response_receiver) = mpsc::channel();
            runner
                .start_arm64_simd_fp_capture(response_sender)
                .expect("SIMD/FP capture should be admitted");
            panic!("fake caller unwind");
        }));
        assert!(unwind_result.is_err());
        entered_capture_receiver
            .recv()
            .expect("runner should enter fake SIMD/FP capture");
        assert_eq!(
            runner.cancel(),
            Err(HvfVcpuRunnerError::InvalidState(
                super::CORE_REGISTER_CAPTURE_IN_FLIGHT_MESSAGE
            ))
        );

        let (barrier_response_sender, barrier_response_receiver) = mpsc::channel();
        runner
            .command_sender
            .send(super::RunnerCommand::ReadMpidrEl1 {
                response_sender: barrier_response_sender,
            })
            .expect("test barrier should queue behind capture");
        release_capture_sender
            .send(Ok(()))
            .expect("SIMD/FP capture release should be sent");
        barrier_receiver
            .recv()
            .expect("runner should enter the command queued after capture");
        assert_eq!(
            barrier_response_receiver
                .recv()
                .expect("barrier response should be sent"),
            Ok(0x8000_0000)
        );
        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn arm64_simd_fp_capture_send_failure_releases_admission() {
        let (runner, runner_unwind_receiver) = start_panic_notifying_runner(|| Ok(PanicOnRunVcpu));

        assert_eq!(
            runner.run_once(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert_eq!(
            runner.capture_arm64_simd_fp_state(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::COMMAND_CHANNEL_CLOSED_MESSAGE
            ))
        );
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn queued_arm64_simd_fp_capture_destruction_releases_admission() {
        let (entered_run_sender, entered_run_receiver) = mpsc::channel();
        let (release_run_sender, release_run_receiver) = mpsc::channel();
        let (runner, runner_unwind_receiver) = start_panic_notifying_runner(move || {
            Ok(BlockingPanicOnRunVcpu {
                entered_run_sender,
                release_run_receiver,
            })
        });

        let (run_response_sender, run_response_receiver) = mpsc::channel();
        runner
            .command_sender
            .send(super::RunnerCommand::RunOnce {
                response_sender: run_response_sender,
            })
            .expect("raw panic command should be sent");
        entered_run_receiver
            .recv()
            .expect("runner should enter the blocking panic command");

        let (capture_response_sender, capture_response_receiver) = mpsc::channel();
        runner
            .start_arm64_simd_fp_capture(capture_response_sender)
            .expect("SIMD/FP capture should queue behind the panic command");
        assert!(
            runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );

        release_run_sender
            .send(())
            .expect("blocking panic command should be released");
        assert!(run_response_receiver.recv().is_err());
        assert!(capture_response_receiver.recv().is_err());
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn shutdown_reports_thread_panic_after_arm64_simd_fp_capture_panic() {
        let (runner, runner_unwind_receiver) =
            start_panic_notifying_runner(|| Ok(PanicOnSimdFpCaptureVcpu));

        assert_eq!(
            runner.capture_arm64_simd_fp_state(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .core_register_capture_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn core_register_captures_after_shutdown_are_rejected() {
        let (runner, _read_receiver) = start_general_register_capture_recording_runner(None);

        runner.shutdown().expect("runner should shut down");

        assert_core_register_captures_rejected(
            &runner,
            HvfVcpuRunnerError::InvalidState(super::RUNNER_SHUT_DOWN_MESSAGE),
        );
    }

    #[test]
    fn core_register_captures_during_shutdown_are_rejected() {
        let (runner, _, destroyed_receiver) = start_fake_runner();
        let (command_sender, should_cancel) = runner
            .prepare_shutdown()
            .expect("first shutdown should be prepared");

        assert!(!should_cancel);
        assert_core_register_captures_rejected(
            &runner,
            HvfVcpuRunnerError::InvalidState(super::RUNNER_SHUTTING_DOWN_MESSAGE),
        );

        let thread = runner
            .take_thread()
            .expect("runner state should be lockable");
        let (response_sender, response_receiver) = mpsc::channel();
        command_sender
            .send(super::RunnerCommand::Shutdown { response_sender })
            .expect("shutdown command should be sent");
        assert_eq!(
            response_receiver
                .recv()
                .expect("shutdown response should be sent"),
            Ok(())
        );
        super::join_runner_thread(thread).expect("runner thread should join");
        runner.finish_shutdown();
        destroyed_receiver
            .recv()
            .expect("fake vCPU should be destroyed");
    }

    #[test]
    fn reads_mpidr_on_runner_thread() {
        let runner = start_mpidr_recording_runner(0x8000_0000, false);

        assert_eq!(runner.mpidr_el1(), Ok(0x8000_0000));
        assert_eq!(runner.mpidr_el1(), Ok(0x8000_0000));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn failed_mpidr_read_can_be_retried_without_stale_state() {
        let runner = start_mpidr_recording_runner(0x8000_0001, true);

        assert_eq!(
            runner.mpidr_el1(),
            Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                "fake MPIDR read failed"
            )))
        );
        assert_eq!(runner.mpidr_el1(), Ok(0x8000_0001));
        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn captures_arm64_physical_timer_state_on_runner_thread() {
        let (runner, read_receiver) = start_physical_timer_capture_recording_runner(None);

        let state = runner
            .capture_arm64_physical_timer_state()
            .expect("physical-timer capture should succeed");

        assert_physical_timer_test_state(state);
        assert_eq!(
            read_receiver.try_iter().collect::<Vec<_>>(),
            physical_timer_registers()
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn failed_arm64_physical_timer_capture_can_be_retried_without_partial_state() {
        let registers = physical_timer_registers();

        for (failed_index, failed_register) in registers.into_iter().enumerate() {
            let (runner, read_receiver) =
                start_physical_timer_capture_recording_runner(Some(failed_register));

            assert_eq!(
                runner.capture_arm64_physical_timer_state(),
                Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                    "fake physical-timer capture failed"
                )))
            );
            assert_eq!(
                read_receiver.try_iter().collect::<Vec<_>>(),
                registers[..=failed_index]
            );

            let state = runner
                .capture_arm64_physical_timer_state()
                .expect("physical-timer capture retry should succeed");
            assert_physical_timer_test_state(state);
            assert_eq!(read_receiver.try_iter().collect::<Vec<_>>(), registers);
            assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

            runner.shutdown().expect("runner should shut down");
        }
    }

    #[test]
    fn reads_and_sets_vtimer_mask_on_runner_thread() {
        let runner = start_vtimer_mask_recording_runner(false, false, false);

        assert_eq!(runner.get_vtimer_mask(), Ok(false));
        assert_eq!(runner.set_vtimer_mask(true), Ok(()));
        assert_eq!(runner.get_vtimer_mask(), Ok(true));
        assert_eq!(runner.set_vtimer_mask(false), Ok(()));
        assert_eq!(runner.get_vtimer_mask(), Ok(false));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn reads_and_sets_vtimer_offset_on_runner_thread() {
        let (runner, operation_receiver) =
            start_vtimer_recording_runner(false, 0x1234, 0, 0, VtimerFailures::default());

        assert_eq!(runner.get_vtimer_offset(), Ok(0x1234));
        assert_eq!(runner.set_vtimer_offset(0x5678), Ok(()));
        assert_eq!(runner.get_vtimer_offset(), Ok(0x5678));
        assert_eq!(
            operation_receiver.try_iter().collect::<Vec<_>>(),
            vec![
                VtimerOperation::GetOffset,
                VtimerOperation::SetOffset(0x5678),
                VtimerOperation::GetOffset,
            ]
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn reads_and_sets_vtimer_control_and_compare_value_on_runner_thread() {
        let (runner, operation_receiver) =
            start_vtimer_recording_runner(false, 0, 0b101, 0x1234, VtimerFailures::default());

        assert_eq!(runner.get_vtimer_control(), Ok(0b101));
        assert_eq!(runner.set_vtimer_control(0b010), Ok(()));
        assert_eq!(runner.get_vtimer_control(), Ok(0b010));
        assert_eq!(runner.get_vtimer_compare_value(), Ok(0x1234));
        assert_eq!(runner.set_vtimer_compare_value(0x5678), Ok(()));
        assert_eq!(runner.get_vtimer_compare_value(), Ok(0x5678));
        assert_eq!(
            operation_receiver.try_iter().collect::<Vec<_>>(),
            vec![
                VtimerOperation::GetControl,
                VtimerOperation::SetControl(0b010),
                VtimerOperation::GetControl,
                VtimerOperation::GetCompareValue,
                VtimerOperation::SetCompareValue(0x5678),
                VtimerOperation::GetCompareValue,
            ]
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn captures_arm64_virtual_timer_state_on_runner_thread() {
        let (runner, operation_receiver) = start_vtimer_recording_runner(
            true,
            0x1234_5678,
            0b101,
            0xfedc_ba98,
            VtimerFailures::default(),
        );

        let state = runner
            .capture_arm64_virtual_timer_state()
            .expect("virtual-timer capture should succeed");

        assert!(state.masked());
        assert_eq!(state.offset(), 0x1234_5678);
        assert_eq!(state.control(), 0b101);
        assert_eq!(state.compare_value(), 0xfedc_ba98);
        assert_eq!(
            operation_receiver.try_iter().collect::<Vec<_>>(),
            vec![
                VtimerOperation::GetMask,
                VtimerOperation::GetOffset,
                VtimerOperation::GetControl,
                VtimerOperation::GetCompareValue,
            ]
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn failed_vtimer_mask_commands_can_be_retried_without_stale_state() {
        let get_runner = start_vtimer_mask_recording_runner(true, true, false);
        let set_runner = start_vtimer_mask_recording_runner(false, false, true);

        assert_eq!(
            get_runner.get_vtimer_mask(),
            Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                "fake vtimer mask read failed"
            )))
        );
        assert_eq!(get_runner.get_vtimer_mask(), Ok(true));
        assert_eq!(get_runner.run_once(), Ok(HvfVcpuExit::Canceled));

        assert_eq!(
            set_runner.set_vtimer_mask(true),
            Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                "fake vtimer mask write failed"
            )))
        );
        assert_eq!(set_runner.set_vtimer_mask(true), Ok(()));
        assert_eq!(set_runner.get_vtimer_mask(), Ok(true));
        assert_eq!(set_runner.run_once(), Ok(HvfVcpuExit::Canceled));

        get_runner.shutdown().expect("runner should shut down");
        set_runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn failed_vtimer_offset_commands_can_be_retried_without_stale_state() {
        let (get_runner, _get_operations) = start_vtimer_recording_runner(
            false,
            0x1234,
            0,
            0,
            VtimerFailures {
                get_offset: true,
                ..VtimerFailures::default()
            },
        );
        let (set_runner, _set_operations) = start_vtimer_recording_runner(
            false,
            0x1234,
            0,
            0,
            VtimerFailures {
                set_offset: true,
                ..VtimerFailures::default()
            },
        );

        assert_eq!(
            get_runner.get_vtimer_offset(),
            Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                "fake vtimer offset read failed"
            )))
        );
        assert_eq!(get_runner.get_vtimer_offset(), Ok(0x1234));
        assert_eq!(get_runner.run_once(), Ok(HvfVcpuExit::Canceled));

        assert_eq!(
            set_runner.set_vtimer_offset(0x5678),
            Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                "fake vtimer offset write failed"
            )))
        );
        assert_eq!(set_runner.set_vtimer_offset(0x5678), Ok(()));
        assert_eq!(set_runner.get_vtimer_offset(), Ok(0x5678));
        assert_eq!(set_runner.run_once(), Ok(HvfVcpuExit::Canceled));

        get_runner.shutdown().expect("runner should shut down");
        set_runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn failed_vtimer_control_commands_can_be_retried_without_stale_state() {
        let (get_runner, _get_operations) = start_vtimer_recording_runner(
            false,
            0,
            0b101,
            0,
            VtimerFailures {
                get_control: true,
                ..VtimerFailures::default()
            },
        );
        let (set_runner, _set_operations) = start_vtimer_recording_runner(
            false,
            0,
            0b101,
            0,
            VtimerFailures {
                set_control: true,
                ..VtimerFailures::default()
            },
        );

        assert_eq!(
            get_runner.get_vtimer_control(),
            Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                "fake vtimer control read failed"
            )))
        );
        assert_eq!(get_runner.get_vtimer_control(), Ok(0b101));
        assert_eq!(get_runner.run_once(), Ok(HvfVcpuExit::Canceled));

        assert_eq!(
            set_runner.set_vtimer_control(0b010),
            Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                "fake vtimer control write failed"
            )))
        );
        assert_eq!(set_runner.set_vtimer_control(0b010), Ok(()));
        assert_eq!(set_runner.get_vtimer_control(), Ok(0b010));
        assert_eq!(set_runner.run_once(), Ok(HvfVcpuExit::Canceled));

        get_runner.shutdown().expect("runner should shut down");
        set_runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn failed_vtimer_compare_commands_can_be_retried_without_stale_state() {
        let (get_runner, _get_operations) = start_vtimer_recording_runner(
            false,
            0,
            0,
            0x1234,
            VtimerFailures {
                get_compare_value: true,
                ..VtimerFailures::default()
            },
        );
        let (set_runner, _set_operations) = start_vtimer_recording_runner(
            false,
            0,
            0,
            0x1234,
            VtimerFailures {
                set_compare_value: true,
                ..VtimerFailures::default()
            },
        );

        assert_eq!(
            get_runner.get_vtimer_compare_value(),
            Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                "fake vtimer compare-value read failed"
            )))
        );
        assert_eq!(get_runner.get_vtimer_compare_value(), Ok(0x1234));
        assert_eq!(get_runner.run_once(), Ok(HvfVcpuExit::Canceled));

        assert_eq!(
            set_runner.set_vtimer_compare_value(0x5678),
            Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                "fake vtimer compare-value write failed"
            )))
        );
        assert_eq!(set_runner.set_vtimer_compare_value(0x5678), Ok(()));
        assert_eq!(set_runner.get_vtimer_compare_value(), Ok(0x5678));
        assert_eq!(set_runner.run_once(), Ok(HvfVcpuExit::Canceled));

        get_runner.shutdown().expect("runner should shut down");
        set_runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn failed_arm64_virtual_timer_capture_can_be_retried_without_partial_state() {
        let cases = [
            (
                VtimerFailures {
                    get_mask: true,
                    ..VtimerFailures::default()
                },
                "fake vtimer mask read failed",
                vec![VtimerOperation::GetMask],
            ),
            (
                VtimerFailures {
                    get_offset: true,
                    ..VtimerFailures::default()
                },
                "fake vtimer offset read failed",
                vec![VtimerOperation::GetMask, VtimerOperation::GetOffset],
            ),
            (
                VtimerFailures {
                    get_control: true,
                    ..VtimerFailures::default()
                },
                "fake vtimer control read failed",
                vec![
                    VtimerOperation::GetMask,
                    VtimerOperation::GetOffset,
                    VtimerOperation::GetControl,
                ],
            ),
            (
                VtimerFailures {
                    get_compare_value: true,
                    ..VtimerFailures::default()
                },
                "fake vtimer compare-value read failed",
                vec![
                    VtimerOperation::GetMask,
                    VtimerOperation::GetOffset,
                    VtimerOperation::GetControl,
                    VtimerOperation::GetCompareValue,
                ],
            ),
        ];

        for (failures, message, first_attempt) in cases {
            let (runner, operations) =
                start_vtimer_recording_runner(true, 0x1234, 0b101, 0x5678, failures);

            assert_eq!(
                runner.capture_arm64_virtual_timer_state(),
                Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                    message
                )))
            );
            assert_eq!(
                runner
                    .capture_arm64_virtual_timer_state()
                    .expect("read failure should leave timer capture retryable"),
                HvfArm64VcpuVirtualTimerState::new(true, 0x1234, 0b101, 0x5678)
            );

            let expected_operations = first_attempt
                .into_iter()
                .chain([
                    VtimerOperation::GetMask,
                    VtimerOperation::GetOffset,
                    VtimerOperation::GetControl,
                    VtimerOperation::GetCompareValue,
                ])
                .collect::<Vec<_>>();
            assert_eq!(
                operations.try_iter().collect::<Vec<_>>(),
                expected_operations
            );
            assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
            runner.shutdown().expect("runner should shut down");
        }
    }

    #[test]
    fn commands_during_vtimer_mask_operation_are_rejected_without_queueing() {
        let (runner, entered_get_receiver, release_get_sender) =
            start_blocking_vtimer_mask_runner();

        thread::scope(|scope| {
            let read = scope.spawn(|| runner.get_vtimer_mask());
            entered_get_receiver
                .recv()
                .expect("runner should enter fake vtimer mask read");

            assert_core_register_captures_rejected(
                &runner,
                HvfVcpuRunnerError::InvalidState(super::TIMER_OPERATION_IN_FLIGHT_MESSAGE),
            );
            assert_timer_operations_rejected(
                &runner,
                HvfVcpuRunnerError::InvalidState(super::TIMER_OPERATION_IN_FLIGHT_MESSAGE),
            );
            assert_eq!(
                runner.run_once(),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::TIMER_OPERATION_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.run_once_and_handle_mmio(shared_dispatcher()),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::TIMER_OPERATION_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.dispatch_mmio_access(resolved_mmio_access(), shared_dispatcher()),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::TIMER_OPERATION_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.configure_arm64_boot_registers(boot_registers()),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::TIMER_OPERATION_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.mpidr_el1(),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::TIMER_OPERATION_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.set_gic_ppi_pending(27),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::TIMER_OPERATION_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.clear_gic_ppi_pending(27),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::TIMER_OPERATION_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.cancel(),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::TIMER_OPERATION_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.shutdown(),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::TIMER_OPERATION_IN_FLIGHT_MESSAGE
                ))
            );

            release_get_sender
                .send(Ok(true))
                .expect("vtimer mask release should be sent");
            assert_eq!(
                read.join().expect("vtimer mask read thread should join"),
                Ok(true)
            );
        });

        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn commands_during_arm64_physical_timer_capture_are_rejected_without_queueing() {
        let (runner, entered_capture_receiver, release_capture_sender, _barrier_receiver) =
            start_blocking_physical_timer_capture_runner();

        thread::scope(|scope| {
            let capture = scope.spawn(|| runner.capture_arm64_physical_timer_state());
            entered_capture_receiver
                .recv()
                .expect("runner should enter fake physical-timer capture");

            let expected =
                HvfVcpuRunnerError::InvalidState(super::TIMER_OPERATION_IN_FLIGHT_MESSAGE);
            assert_core_register_captures_rejected(&runner, expected.clone());
            assert_timer_operations_rejected(&runner, expected.clone());
            assert_eq!(runner.run_once(), Err(expected.clone()));
            assert_eq!(
                runner.run_once_and_handle_mmio(shared_dispatcher()),
                Err(expected.clone())
            );
            assert_eq!(
                runner.dispatch_mmio_access(resolved_mmio_access(), shared_dispatcher()),
                Err(expected.clone())
            );
            assert_eq!(
                runner.configure_arm64_boot_registers(boot_registers()),
                Err(expected.clone())
            );
            assert_eq!(runner.mpidr_el1(), Err(expected.clone()));
            assert_eq!(runner.capture_gic_device_state(), Err(expected.clone()));
            assert_eq!(runner.set_gic_ppi_pending(27), Err(expected.clone()));
            assert_eq!(runner.clear_gic_ppi_pending(27), Err(expected.clone()));
            assert_eq!(runner.cancel(), Err(expected.clone()));
            assert_eq!(runner.shutdown(), Err(expected));

            release_capture_sender
                .send(Ok(()))
                .expect("physical-timer capture release should be sent");
            let state = capture
                .join()
                .expect("physical-timer capture thread should join")
                .expect("physical-timer capture should succeed");
            assert_physical_timer_test_state(state);
        });

        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn caller_unwind_keeps_arm64_physical_timer_capture_admitted_until_command_finishes() {
        let (runner, entered_capture_receiver, release_capture_sender, barrier_receiver) =
            start_blocking_physical_timer_capture_runner();

        let unwind_result = panic::catch_unwind(AssertUnwindSafe(|| {
            let (response_sender, _response_receiver) = mpsc::channel();
            runner
                .start_arm64_physical_timer_capture(response_sender)
                .expect("physical-timer capture should be admitted");
            panic!("fake caller unwind");
        }));
        assert!(unwind_result.is_err());
        entered_capture_receiver
            .recv()
            .expect("runner should enter fake physical-timer capture");
        assert_eq!(
            runner.cancel(),
            Err(HvfVcpuRunnerError::InvalidState(
                super::TIMER_OPERATION_IN_FLIGHT_MESSAGE
            ))
        );

        let (barrier_response_sender, barrier_response_receiver) = mpsc::channel();
        runner
            .command_sender
            .send(super::RunnerCommand::ReadMpidrEl1 {
                response_sender: barrier_response_sender,
            })
            .expect("test barrier should queue behind capture");
        release_capture_sender
            .send(Ok(()))
            .expect("physical-timer capture release should be sent");
        barrier_receiver
            .recv()
            .expect("runner should enter the command queued after capture");
        assert_eq!(
            barrier_response_receiver
                .recv()
                .expect("barrier response should be sent"),
            Ok(0x8000_0000)
        );
        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn arm64_physical_timer_capture_send_failure_releases_admission() {
        let (runner, runner_unwind_receiver) = start_panic_notifying_runner(|| Ok(PanicOnRunVcpu));

        assert_eq!(
            runner.run_once(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert_eq!(
            runner.capture_arm64_physical_timer_state(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::COMMAND_CHANNEL_CLOSED_MESSAGE
            ))
        );
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .timer_operation_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn queued_arm64_physical_timer_capture_destruction_releases_admission() {
        let (entered_run_sender, entered_run_receiver) = mpsc::channel();
        let (release_run_sender, release_run_receiver) = mpsc::channel();
        let (runner, runner_unwind_receiver) = start_panic_notifying_runner(move || {
            Ok(BlockingPanicOnRunVcpu {
                entered_run_sender,
                release_run_receiver,
            })
        });

        let (run_response_sender, run_response_receiver) = mpsc::channel();
        runner
            .command_sender
            .send(super::RunnerCommand::RunOnce {
                response_sender: run_response_sender,
            })
            .expect("raw panic command should be sent");
        entered_run_receiver
            .recv()
            .expect("runner should enter the blocking panic command");

        let (capture_response_sender, capture_response_receiver) = mpsc::channel();
        runner
            .start_arm64_physical_timer_capture(capture_response_sender)
            .expect("physical-timer capture should queue behind the panic command");
        assert!(
            runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .timer_operation_in_flight
        );

        release_run_sender
            .send(())
            .expect("blocking panic command should be released");
        assert!(run_response_receiver.recv().is_err());
        assert!(capture_response_receiver.recv().is_err());
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .timer_operation_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn shutdown_reports_thread_panic_after_arm64_physical_timer_capture_panic() {
        let (runner, runner_unwind_receiver) =
            start_panic_notifying_runner(|| Ok(PanicOnPhysicalTimerCaptureVcpu));

        assert_eq!(
            runner.capture_arm64_physical_timer_state(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .timer_operation_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn commands_during_arm64_virtual_timer_capture_are_rejected_without_queueing() {
        let (runner, entered_get_receiver, release_get_sender, _barrier_receiver) =
            start_blocking_virtual_timer_capture_runner();

        thread::scope(|scope| {
            let capture = scope.spawn(|| runner.capture_arm64_virtual_timer_state());
            entered_get_receiver
                .recv()
                .expect("runner should enter fake virtual-timer capture");

            let expected =
                HvfVcpuRunnerError::InvalidState(super::TIMER_OPERATION_IN_FLIGHT_MESSAGE);
            assert_core_register_captures_rejected(&runner, expected.clone());
            assert_timer_operations_rejected(&runner, expected.clone());
            assert_eq!(runner.run_once(), Err(expected.clone()));
            assert_eq!(
                runner.run_once_and_handle_mmio(shared_dispatcher()),
                Err(expected.clone())
            );
            assert_eq!(
                runner.dispatch_mmio_access(resolved_mmio_access(), shared_dispatcher()),
                Err(expected.clone())
            );
            assert_eq!(
                runner.configure_arm64_boot_registers(boot_registers()),
                Err(expected.clone())
            );
            assert_eq!(runner.mpidr_el1(), Err(expected.clone()));
            assert_eq!(runner.capture_gic_device_state(), Err(expected.clone()));
            assert_eq!(runner.set_gic_ppi_pending(27), Err(expected.clone()));
            assert_eq!(runner.clear_gic_ppi_pending(27), Err(expected.clone()));
            assert_eq!(runner.cancel(), Err(expected.clone()));
            assert_eq!(runner.shutdown(), Err(expected));

            release_get_sender
                .send(Ok(true))
                .expect("virtual-timer capture release should be sent");
            let state = capture
                .join()
                .expect("virtual-timer capture thread should join")
                .expect("virtual-timer capture should succeed");
            assert!(state.masked());
            assert_eq!(state.offset(), 0x1234_5678);
            assert_eq!(state.control(), 0b101);
            assert_eq!(state.compare_value(), 0xfedc_ba98);
        });

        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn caller_unwind_keeps_arm64_virtual_timer_capture_admitted_until_command_finishes() {
        let (runner, entered_get_receiver, release_get_sender, barrier_receiver) =
            start_blocking_virtual_timer_capture_runner();

        let unwind_result = panic::catch_unwind(AssertUnwindSafe(|| {
            let (response_sender, _response_receiver) = mpsc::channel();
            runner
                .start_arm64_virtual_timer_capture(response_sender)
                .expect("virtual-timer capture should be admitted");
            panic!("fake caller unwind");
        }));
        assert!(unwind_result.is_err());
        entered_get_receiver
            .recv()
            .expect("runner should enter fake virtual-timer capture");
        assert_eq!(
            runner.cancel(),
            Err(HvfVcpuRunnerError::InvalidState(
                super::TIMER_OPERATION_IN_FLIGHT_MESSAGE
            ))
        );

        let (barrier_response_sender, barrier_response_receiver) = mpsc::channel();
        runner
            .command_sender
            .send(super::RunnerCommand::ReadMpidrEl1 {
                response_sender: barrier_response_sender,
            })
            .expect("test barrier should queue behind capture");
        release_get_sender
            .send(Ok(true))
            .expect("virtual-timer capture release should be sent");
        barrier_receiver
            .recv()
            .expect("runner should enter the command queued after capture");
        assert_eq!(
            barrier_response_receiver
                .recv()
                .expect("barrier response should be sent"),
            Ok(0x8000_0000)
        );
        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn arm64_virtual_timer_capture_send_failure_releases_admission() {
        let (runner, runner_unwind_receiver) = start_panic_notifying_runner(|| Ok(PanicOnRunVcpu));

        assert_eq!(
            runner.run_once(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert_eq!(
            runner.capture_arm64_virtual_timer_state(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::COMMAND_CHANNEL_CLOSED_MESSAGE
            ))
        );
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .timer_operation_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn queued_arm64_virtual_timer_capture_destruction_releases_admission() {
        let (entered_run_sender, entered_run_receiver) = mpsc::channel();
        let (release_run_sender, release_run_receiver) = mpsc::channel();
        let (runner, runner_unwind_receiver) = start_panic_notifying_runner(move || {
            Ok(BlockingPanicOnRunVcpu {
                entered_run_sender,
                release_run_receiver,
            })
        });

        let (run_response_sender, run_response_receiver) = mpsc::channel();
        runner
            .command_sender
            .send(super::RunnerCommand::RunOnce {
                response_sender: run_response_sender,
            })
            .expect("raw panic command should be sent");
        entered_run_receiver
            .recv()
            .expect("runner should enter the blocking panic command");

        let (capture_response_sender, capture_response_receiver) = mpsc::channel();
        runner
            .start_arm64_virtual_timer_capture(capture_response_sender)
            .expect("virtual-timer capture should queue behind the panic command");
        assert!(
            runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .timer_operation_in_flight
        );

        release_run_sender
            .send(())
            .expect("blocking panic command should be released");
        assert!(run_response_receiver.recv().is_err());
        assert!(capture_response_receiver.recv().is_err());
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .timer_operation_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn shutdown_reports_thread_panic_after_vtimer_mask_panic() {
        let (runner, runner_unwind_receiver) =
            start_panic_notifying_runner(|| Ok(PanicOnVtimerMaskVcpu));

        assert_eq!(
            runner.get_vtimer_mask(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn shutdown_reports_thread_panic_after_arm64_virtual_timer_capture_panic() {
        let (runner, runner_unwind_receiver) =
            start_panic_notifying_runner(|| Ok(PanicOnVtimerMaskVcpu));

        assert_eq!(
            runner.capture_arm64_virtual_timer_state(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .timer_operation_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn gets_sets_and_captures_pending_interrupts_on_runner_thread() {
        let (runner, operation_receiver) =
            start_pending_interrupt_recording_runner(false, true, None);

        assert_eq!(
            runner.get_pending_interrupt(HvfInterruptType::Irq),
            Ok(false)
        );
        assert_eq!(
            runner.get_pending_interrupt(HvfInterruptType::Fiq),
            Ok(true)
        );
        assert_eq!(
            runner.set_pending_interrupt(HvfInterruptType::Irq, true),
            Ok(())
        );
        assert_eq!(
            runner.set_pending_interrupt(HvfInterruptType::Fiq, false),
            Ok(())
        );
        let state = runner
            .capture_arm64_pending_interrupt_state()
            .expect("pending-interrupt capture should succeed");
        assert!(state.irq_pending());
        assert!(!state.fiq_pending());
        assert_eq!(
            operation_receiver.try_iter().collect::<Vec<_>>(),
            [
                PendingInterruptOperation::Get(HvfInterruptType::Irq),
                PendingInterruptOperation::Get(HvfInterruptType::Fiq),
                PendingInterruptOperation::Set(HvfInterruptType::Irq, true),
                PendingInterruptOperation::Set(HvfInterruptType::Fiq, false),
                PendingInterruptOperation::Get(HvfInterruptType::Irq),
                PendingInterruptOperation::Get(HvfInterruptType::Fiq),
            ]
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn failed_pending_interrupt_get_and_set_can_be_retried() {
        for operation in [
            PendingInterruptOperation::Get(HvfInterruptType::Irq),
            PendingInterruptOperation::Get(HvfInterruptType::Fiq),
            PendingInterruptOperation::Set(HvfInterruptType::Irq, true),
            PendingInterruptOperation::Set(HvfInterruptType::Fiq, false),
        ] {
            let (runner, operation_receiver) =
                start_pending_interrupt_recording_runner(false, true, Some(operation));
            let execute = || match operation {
                PendingInterruptOperation::Get(interrupt_type) => {
                    runner.get_pending_interrupt(interrupt_type).map(|_| ())
                }
                PendingInterruptOperation::Set(interrupt_type, pending) => {
                    runner.set_pending_interrupt(interrupt_type, pending)
                }
            };

            assert_eq!(
                execute(),
                Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                    "fake pending interrupt operation failed"
                )))
            );
            assert_eq!(execute(), Ok(()));
            assert_eq!(
                operation_receiver.try_iter().collect::<Vec<_>>(),
                [operation, operation]
            );
            assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
            runner.shutdown().expect("runner should shut down");
        }
    }

    #[test]
    fn failed_pending_interrupt_capture_stops_in_order_and_can_be_retried() {
        for failed_type in [HvfInterruptType::Irq, HvfInterruptType::Fiq] {
            let failed_operation = PendingInterruptOperation::Get(failed_type);
            let (runner, operation_receiver) =
                start_pending_interrupt_recording_runner(false, true, Some(failed_operation));

            assert_eq!(
                runner.capture_arm64_pending_interrupt_state(),
                Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                    "fake pending interrupt operation failed"
                )))
            );
            let failed_reads = operation_receiver.try_iter().collect::<Vec<_>>();
            let expected_failed_reads = if failed_type == HvfInterruptType::Irq {
                vec![PendingInterruptOperation::Get(HvfInterruptType::Irq)]
            } else {
                vec![
                    PendingInterruptOperation::Get(HvfInterruptType::Irq),
                    PendingInterruptOperation::Get(HvfInterruptType::Fiq),
                ]
            };
            assert_eq!(failed_reads, expected_failed_reads);

            let state = runner
                .capture_arm64_pending_interrupt_state()
                .expect("pending-interrupt capture retry should succeed");
            assert!(!state.irq_pending());
            assert!(state.fiq_pending());
            assert_eq!(
                operation_receiver.try_iter().collect::<Vec<_>>(),
                [
                    PendingInterruptOperation::Get(HvfInterruptType::Irq),
                    PendingInterruptOperation::Get(HvfInterruptType::Fiq),
                ]
            );
            assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
            runner.shutdown().expect("runner should shut down");
        }
    }

    #[test]
    fn commands_during_pending_interrupt_capture_are_rejected_without_queueing() {
        let (runner, entered_get_receiver, release_get_sender, _barrier_receiver) =
            start_blocking_pending_interrupt_capture_runner();

        thread::scope(|scope| {
            let capture = scope.spawn(|| runner.capture_arm64_pending_interrupt_state());
            entered_get_receiver
                .recv()
                .expect("runner should enter fake pending-interrupt capture");

            let expected =
                HvfVcpuRunnerError::InvalidState(super::INTERRUPT_OPERATION_IN_FLIGHT_MESSAGE);
            assert_interrupt_operations_rejected(&runner, expected.clone());
            assert_core_register_captures_rejected(&runner, expected.clone());
            assert_timer_operations_rejected(&runner, expected.clone());
            assert_eq!(runner.run_once(), Err(expected.clone()));
            assert_eq!(
                runner.run_once_and_handle_mmio(shared_dispatcher()),
                Err(expected.clone())
            );
            assert_eq!(
                runner.dispatch_mmio_access(resolved_mmio_access(), shared_dispatcher()),
                Err(expected.clone())
            );
            assert_eq!(
                runner.configure_arm64_boot_registers(boot_registers()),
                Err(expected.clone())
            );
            assert_eq!(runner.mpidr_el1(), Err(expected.clone()));
            assert_eq!(runner.cancel(), Err(expected.clone()));
            assert_eq!(runner.shutdown(), Err(expected));

            release_get_sender
                .send(Ok(()))
                .expect("pending-interrupt capture release should be sent");
            let state = capture
                .join()
                .expect("pending-interrupt capture thread should join")
                .expect("pending-interrupt capture should succeed");
            assert!(state.irq_pending());
            assert!(!state.fiq_pending());
        });

        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn caller_unwind_keeps_pending_interrupt_capture_admitted_until_command_finishes() {
        let (runner, entered_get_receiver, release_get_sender, barrier_receiver) =
            start_blocking_pending_interrupt_capture_runner();

        let unwind_result = panic::catch_unwind(AssertUnwindSafe(|| {
            let (response_sender, _response_receiver) = mpsc::channel();
            runner
                .start_arm64_pending_interrupt_capture(response_sender)
                .expect("pending-interrupt capture should be admitted");
            panic!("fake caller unwind");
        }));
        assert!(unwind_result.is_err());
        entered_get_receiver
            .recv()
            .expect("runner should enter fake pending-interrupt capture");
        assert_eq!(
            runner.cancel(),
            Err(HvfVcpuRunnerError::InvalidState(
                super::INTERRUPT_OPERATION_IN_FLIGHT_MESSAGE
            ))
        );

        let (barrier_response_sender, barrier_response_receiver) = mpsc::channel();
        runner
            .command_sender
            .send(super::RunnerCommand::ReadMpidrEl1 {
                response_sender: barrier_response_sender,
            })
            .expect("test barrier should queue behind capture");
        release_get_sender
            .send(Ok(()))
            .expect("pending-interrupt capture release should be sent");
        barrier_receiver
            .recv()
            .expect("runner should enter the command queued after capture");
        assert_eq!(
            barrier_response_receiver
                .recv()
                .expect("barrier response should be sent"),
            Ok(0x8000_0000)
        );
        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn pending_interrupt_capture_send_failure_releases_admission() {
        let (runner, runner_unwind_receiver) = start_panic_notifying_runner(|| Ok(PanicOnRunVcpu));

        assert_eq!(
            runner.run_once(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert_eq!(
            runner.capture_arm64_pending_interrupt_state(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::COMMAND_CHANNEL_CLOSED_MESSAGE
            ))
        );
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .interrupt_operation_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn queued_pending_interrupt_capture_destruction_releases_admission() {
        let (entered_run_sender, entered_run_receiver) = mpsc::channel();
        let (release_run_sender, release_run_receiver) = mpsc::channel();
        let (runner, runner_unwind_receiver) = start_panic_notifying_runner(move || {
            Ok(BlockingPanicOnRunVcpu {
                entered_run_sender,
                release_run_receiver,
            })
        });

        let (run_response_sender, run_response_receiver) = mpsc::channel();
        runner
            .command_sender
            .send(super::RunnerCommand::RunOnce {
                response_sender: run_response_sender,
            })
            .expect("raw panic command should be sent");
        entered_run_receiver
            .recv()
            .expect("runner should enter the blocking panic command");

        let (capture_response_sender, capture_response_receiver) = mpsc::channel();
        runner
            .start_arm64_pending_interrupt_capture(capture_response_sender)
            .expect("pending-interrupt capture should queue behind the panic command");
        assert!(
            runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .interrupt_operation_in_flight
        );

        release_run_sender
            .send(())
            .expect("blocking panic command should be released");
        assert!(run_response_receiver.recv().is_err());
        assert!(capture_response_receiver.recv().is_err());
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .interrupt_operation_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn pending_interrupt_capture_panic_releases_admission() {
        let (runner, runner_unwind_receiver) =
            start_panic_notifying_runner(|| Ok(PanicOnPendingInterruptVcpu));

        assert_eq!(
            runner.capture_arm64_pending_interrupt_state(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .interrupt_operation_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn captures_opaque_gic_device_state_on_runner_thread() {
        let (runner, capture_receiver) = start_gic_device_state_recording_runner(false);

        let state = runner
            .capture_gic_device_state()
            .expect("GIC device-state capture should succeed");

        assert_eq!(state.as_bytes(), GIC_DEVICE_STATE_TEST_BYTES);
        assert_eq!(capture_receiver.try_iter().count(), 1);
        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn failed_gic_device_state_capture_can_be_retried_without_stale_admission() {
        let (runner, capture_receiver) = start_gic_device_state_recording_runner(true);

        assert_eq!(
            runner.capture_gic_device_state(),
            Err(HvfVcpuRunnerError::Gic(HvfGicError::InvalidState(
                "fake GIC device-state capture failed"
            )))
        );
        assert_eq!(capture_receiver.try_iter().count(), 1);

        let state = runner
            .capture_gic_device_state()
            .expect("GIC device-state capture retry should succeed");
        assert_eq!(state.as_bytes(), GIC_DEVICE_STATE_TEST_BYTES);
        assert_eq!(capture_receiver.try_iter().count(), 1);
        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn commands_during_gic_device_state_capture_are_rejected_without_queueing() {
        let (runner, entered_capture_receiver, release_capture_sender, _barrier_receiver) =
            start_blocking_gic_device_state_capture_runner();

        thread::scope(|scope| {
            let capture = scope.spawn(|| runner.capture_gic_device_state());
            entered_capture_receiver
                .recv()
                .expect("runner should enter fake GIC device-state capture");

            let expected =
                HvfVcpuRunnerError::InvalidState(super::INTERRUPT_OPERATION_IN_FLIGHT_MESSAGE);
            assert_interrupt_operations_rejected(&runner, expected.clone());
            assert_core_register_captures_rejected(&runner, expected.clone());
            assert_timer_operations_rejected(&runner, expected.clone());
            assert_eq!(runner.run_once(), Err(expected.clone()));
            assert_eq!(
                runner.run_once_and_handle_mmio(shared_dispatcher()),
                Err(expected.clone())
            );
            assert_eq!(
                runner.dispatch_mmio_access(resolved_mmio_access(), shared_dispatcher()),
                Err(expected.clone())
            );
            assert_eq!(
                runner.configure_arm64_boot_registers(boot_registers()),
                Err(expected.clone())
            );
            assert_eq!(runner.mpidr_el1(), Err(expected.clone()));
            assert_eq!(runner.cancel(), Err(expected.clone()));
            assert_eq!(runner.shutdown(), Err(expected));

            release_capture_sender
                .send(Ok(GIC_DEVICE_STATE_TEST_BYTES.to_vec()))
                .expect("GIC device-state capture release should be sent");
            let state = capture
                .join()
                .expect("GIC device-state capture thread should join")
                .expect("GIC device-state capture should succeed");
            assert_eq!(state.as_bytes(), GIC_DEVICE_STATE_TEST_BYTES);
        });

        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn caller_unwind_keeps_gic_device_state_capture_admitted_until_command_finishes() {
        let (runner, entered_capture_receiver, release_capture_sender, barrier_receiver) =
            start_blocking_gic_device_state_capture_runner();

        let unwind_result = panic::catch_unwind(AssertUnwindSafe(|| {
            let (response_sender, _response_receiver) = mpsc::channel();
            runner
                .start_gic_device_state_capture(response_sender)
                .expect("GIC device-state capture should be admitted");
            panic!("fake caller unwind");
        }));
        assert!(unwind_result.is_err());
        entered_capture_receiver
            .recv()
            .expect("runner should enter fake GIC device-state capture");
        assert_eq!(
            runner.cancel(),
            Err(HvfVcpuRunnerError::InvalidState(
                super::INTERRUPT_OPERATION_IN_FLIGHT_MESSAGE
            ))
        );

        let (barrier_response_sender, barrier_response_receiver) = mpsc::channel();
        runner
            .command_sender
            .send(super::RunnerCommand::ReadMpidrEl1 {
                response_sender: barrier_response_sender,
            })
            .expect("test barrier should queue behind capture");
        release_capture_sender
            .send(Ok(GIC_DEVICE_STATE_TEST_BYTES.to_vec()))
            .expect("GIC device-state capture release should be sent");
        barrier_receiver
            .recv()
            .expect("runner should enter the command queued after capture");
        assert_eq!(
            barrier_response_receiver
                .recv()
                .expect("barrier response should be sent"),
            Ok(0x8000_0000)
        );
        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn gic_device_state_capture_send_failure_releases_admission() {
        let (runner, runner_unwind_receiver) = start_panic_notifying_runner(|| Ok(PanicOnRunVcpu));

        assert_eq!(
            runner.run_once(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert_eq!(
            runner.capture_gic_device_state(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::COMMAND_CHANNEL_CLOSED_MESSAGE
            ))
        );
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .interrupt_operation_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn queued_gic_device_state_capture_destruction_releases_admission() {
        let (entered_run_sender, entered_run_receiver) = mpsc::channel();
        let (release_run_sender, release_run_receiver) = mpsc::channel();
        let (runner, runner_unwind_receiver) = start_panic_notifying_runner(move || {
            Ok(BlockingPanicOnRunVcpu {
                entered_run_sender,
                release_run_receiver,
            })
        });

        let (run_response_sender, run_response_receiver) = mpsc::channel();
        runner
            .command_sender
            .send(super::RunnerCommand::RunOnce {
                response_sender: run_response_sender,
            })
            .expect("raw panic command should be sent");
        entered_run_receiver
            .recv()
            .expect("runner should enter the blocking panic command");

        let (capture_response_sender, capture_response_receiver) = mpsc::channel();
        runner
            .start_gic_device_state_capture(capture_response_sender)
            .expect("GIC device-state capture should queue behind the panic command");
        assert!(
            runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .interrupt_operation_in_flight
        );

        release_run_sender
            .send(())
            .expect("blocking panic command should be released");
        assert!(run_response_receiver.recv().is_err());
        assert!(capture_response_receiver.recv().is_err());
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .interrupt_operation_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn gic_device_state_capture_panic_releases_admission() {
        let (runner, runner_unwind_receiver) =
            start_panic_notifying_runner(|| Ok(PanicOnGicDeviceStateVcpu));

        assert_eq!(
            runner.capture_gic_device_state(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .interrupt_operation_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn captures_arm64_gic_icc_register_state_on_runner_thread() {
        let (runner, capture_receiver) = start_gic_icc_register_state_recording_runner(false);

        let state = runner
            .capture_arm64_gic_icc_register_state()
            .expect("GIC ICC register-state capture should succeed");

        assert_gic_icc_register_test_state(state);
        assert_eq!(capture_receiver.try_iter().count(), 1);
        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn failed_gic_icc_register_state_capture_can_be_retried_without_stale_admission() {
        let (runner, capture_receiver) = start_gic_icc_register_state_recording_runner(true);

        assert_eq!(
            runner.capture_arm64_gic_icc_register_state(),
            Err(HvfVcpuRunnerError::Gic(HvfGicError::InvalidState(
                "fake GIC ICC register-state capture failed"
            )))
        );
        assert_eq!(capture_receiver.try_iter().count(), 1);

        let state = runner
            .capture_arm64_gic_icc_register_state()
            .expect("GIC ICC register-state capture retry should succeed");
        assert_gic_icc_register_test_state(state);
        assert_eq!(capture_receiver.try_iter().count(), 1);
        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn commands_during_gic_icc_register_state_capture_are_rejected_without_queueing() {
        let (runner, entered_capture_receiver, release_capture_sender, _barrier_receiver) =
            start_blocking_gic_icc_register_state_capture_runner();

        thread::scope(|scope| {
            let capture = scope.spawn(|| runner.capture_arm64_gic_icc_register_state());
            entered_capture_receiver
                .recv()
                .expect("runner should enter fake GIC ICC register-state capture");

            let expected =
                HvfVcpuRunnerError::InvalidState(super::INTERRUPT_OPERATION_IN_FLIGHT_MESSAGE);
            assert_interrupt_operations_rejected(&runner, expected.clone());
            assert_core_register_captures_rejected(&runner, expected.clone());
            assert_timer_operations_rejected(&runner, expected.clone());
            assert_eq!(runner.run_once(), Err(expected.clone()));
            assert_eq!(
                runner.run_once_and_handle_mmio(shared_dispatcher()),
                Err(expected.clone())
            );
            assert_eq!(
                runner.dispatch_mmio_access(resolved_mmio_access(), shared_dispatcher()),
                Err(expected.clone())
            );
            assert_eq!(
                runner.configure_arm64_boot_registers(boot_registers()),
                Err(expected.clone())
            );
            assert_eq!(runner.mpidr_el1(), Err(expected.clone()));
            assert_eq!(runner.cancel(), Err(expected.clone()));
            assert_eq!(runner.shutdown(), Err(expected));

            release_capture_sender
                .send(Ok(GIC_ICC_REGISTER_STATE_TEST_VALUES))
                .expect("GIC ICC register-state capture release should be sent");
            let state = capture
                .join()
                .expect("GIC ICC register-state capture thread should join")
                .expect("GIC ICC register-state capture should succeed");
            assert_gic_icc_register_test_state(state);
        });

        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn caller_unwind_keeps_gic_icc_register_state_capture_admitted_until_command_finishes() {
        let (runner, entered_capture_receiver, release_capture_sender, barrier_receiver) =
            start_blocking_gic_icc_register_state_capture_runner();

        let unwind_result = panic::catch_unwind(AssertUnwindSafe(|| {
            let (response_sender, _response_receiver) = mpsc::channel();
            runner
                .start_arm64_gic_icc_register_state_capture(response_sender)
                .expect("GIC ICC register-state capture should be admitted");
            panic!("fake caller unwind");
        }));
        assert!(unwind_result.is_err());
        entered_capture_receiver
            .recv()
            .expect("runner should enter fake GIC ICC register-state capture");
        assert_eq!(
            runner.cancel(),
            Err(HvfVcpuRunnerError::InvalidState(
                super::INTERRUPT_OPERATION_IN_FLIGHT_MESSAGE
            ))
        );

        let (barrier_response_sender, barrier_response_receiver) = mpsc::channel();
        runner
            .command_sender
            .send(super::RunnerCommand::ReadMpidrEl1 {
                response_sender: barrier_response_sender,
            })
            .expect("test barrier should queue behind capture");
        release_capture_sender
            .send(Ok(GIC_ICC_REGISTER_STATE_TEST_VALUES))
            .expect("GIC ICC register-state capture release should be sent");
        barrier_receiver
            .recv()
            .expect("runner should enter the command queued after capture");
        assert_eq!(
            barrier_response_receiver
                .recv()
                .expect("barrier response should be sent"),
            Ok(0x8000_0000)
        );
        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn gic_icc_register_state_capture_send_failure_releases_admission() {
        let (runner, runner_unwind_receiver) = start_panic_notifying_runner(|| Ok(PanicOnRunVcpu));

        assert_eq!(
            runner.run_once(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert_eq!(
            runner.capture_arm64_gic_icc_register_state(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::COMMAND_CHANNEL_CLOSED_MESSAGE
            ))
        );
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .interrupt_operation_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn queued_gic_icc_register_state_capture_destruction_releases_admission() {
        let (entered_run_sender, entered_run_receiver) = mpsc::channel();
        let (release_run_sender, release_run_receiver) = mpsc::channel();
        let (runner, runner_unwind_receiver) = start_panic_notifying_runner(move || {
            Ok(BlockingPanicOnRunVcpu {
                entered_run_sender,
                release_run_receiver,
            })
        });

        let (run_response_sender, run_response_receiver) = mpsc::channel();
        runner
            .command_sender
            .send(super::RunnerCommand::RunOnce {
                response_sender: run_response_sender,
            })
            .expect("raw panic command should be sent");
        entered_run_receiver
            .recv()
            .expect("runner should enter the blocking panic command");

        let (capture_response_sender, capture_response_receiver) = mpsc::channel();
        runner
            .start_arm64_gic_icc_register_state_capture(capture_response_sender)
            .expect("GIC ICC register-state capture should queue behind the panic command");
        assert!(
            runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .interrupt_operation_in_flight
        );

        release_run_sender
            .send(())
            .expect("blocking panic command should be released");
        assert!(run_response_receiver.recv().is_err());
        assert!(capture_response_receiver.recv().is_err());
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .interrupt_operation_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn gic_icc_register_state_capture_panic_releases_admission() {
        let (runner, runner_unwind_receiver) =
            start_panic_notifying_runner(|| Ok(PanicOnGicIccRegisterStateVcpu));

        assert_eq!(
            runner.capture_arm64_gic_icc_register_state(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert!(
            !runner
                .state
                .lock()
                .expect("runner state should be lockable")
                .interrupt_operation_in_flight
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn sets_and_clears_gic_ppi_pending_on_runner_thread() {
        let (runner, operation_receiver) = start_gic_ppi_pending_recording_runner(false);

        assert_eq!(runner.set_gic_ppi_pending(27), Ok(()));
        assert_eq!(runner.clear_gic_ppi_pending(27), Ok(()));

        assert_eq!(
            operation_receiver
                .recv()
                .expect("fake vCPU should receive set operation"),
            (27, true)
        );
        assert_eq!(
            operation_receiver
                .recv()
                .expect("fake vCPU should receive clear operation"),
            (27, false)
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn failed_gic_ppi_pending_command_can_be_retried_without_stale_state() {
        let (runner, operation_receiver) = start_gic_ppi_pending_recording_runner(true);

        assert_eq!(
            runner.set_gic_ppi_pending(27),
            Err(HvfVcpuRunnerError::Gic(HvfGicError::InvalidState(
                "fake GIC PPI pending operation failed"
            )))
        );
        assert_eq!(runner.set_gic_ppi_pending(27), Ok(()));
        assert_eq!(
            operation_receiver
                .recv()
                .expect("fake vCPU should receive retried operation"),
            (27, true)
        );
        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn invalid_gic_ppi_pending_intid_is_rejected_without_queueing() {
        let (runner, operation_receiver) = start_gic_ppi_pending_recording_runner(false);

        assert_eq!(
            runner.set_gic_ppi_pending(15),
            Err(HvfVcpuRunnerError::Gic(HvfGicError::InvalidParameter {
                name: "ppi_intid",
                value: 15,
            }))
        );
        assert_eq!(
            runner.clear_gic_ppi_pending(32),
            Err(HvfVcpuRunnerError::Gic(HvfGicError::InvalidParameter {
                name: "ppi_intid",
                value: 32,
            }))
        );
        assert_eq!(
            operation_receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        );
        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn commands_during_gic_ppi_pending_operation_are_rejected_without_queueing() {
        let (runner, entered_operation_receiver, release_operation_sender) =
            start_blocking_gic_ppi_pending_runner();

        thread::scope(|scope| {
            let operation = scope.spawn(|| runner.set_gic_ppi_pending(27));
            entered_operation_receiver
                .recv()
                .expect("runner should enter fake GIC PPI pending operation");

            assert_core_register_captures_rejected(
                &runner,
                HvfVcpuRunnerError::InvalidState(super::INTERRUPT_OPERATION_IN_FLIGHT_MESSAGE),
            );
            assert_interrupt_operations_rejected(
                &runner,
                HvfVcpuRunnerError::InvalidState(super::INTERRUPT_OPERATION_IN_FLIGHT_MESSAGE),
            );
            assert_eq!(
                runner.run_once(),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::INTERRUPT_OPERATION_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.run_once_and_handle_mmio(shared_dispatcher()),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::INTERRUPT_OPERATION_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.dispatch_mmio_access(resolved_mmio_access(), shared_dispatcher()),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::INTERRUPT_OPERATION_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.configure_arm64_boot_registers(boot_registers()),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::INTERRUPT_OPERATION_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.mpidr_el1(),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::INTERRUPT_OPERATION_IN_FLIGHT_MESSAGE
                ))
            );
            assert_timer_operations_rejected(
                &runner,
                HvfVcpuRunnerError::InvalidState(super::INTERRUPT_OPERATION_IN_FLIGHT_MESSAGE),
            );
            assert_eq!(
                runner.cancel(),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::INTERRUPT_OPERATION_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.shutdown(),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::INTERRUPT_OPERATION_IN_FLIGHT_MESSAGE
                ))
            );

            release_operation_sender
                .send(Ok(()))
                .expect("GIC PPI pending release should be sent");
            assert_eq!(
                operation
                    .join()
                    .expect("GIC PPI pending thread should join"),
                Ok(())
            );
        });

        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn shutdown_reports_thread_panic_after_gic_ppi_pending_panic() {
        let (runner, runner_unwind_receiver) =
            start_panic_notifying_runner(|| Ok(PanicOnGicPpiPendingVcpu));

        assert_eq!(
            runner.set_gic_ppi_pending(27),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn commands_during_mpidr_read_are_rejected_without_queueing() {
        let (runner, entered_read_receiver, release_read_sender) = start_blocking_mpidr_runner();

        thread::scope(|scope| {
            let read = scope.spawn(|| runner.mpidr_el1());
            entered_read_receiver
                .recv()
                .expect("runner should enter fake MPIDR read");

            assert_core_register_captures_rejected(
                &runner,
                HvfVcpuRunnerError::InvalidState(super::METADATA_READ_IN_FLIGHT_MESSAGE),
            );
            assert_eq!(
                runner.mpidr_el1(),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::METADATA_READ_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.run_once(),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::METADATA_READ_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.run_once_and_handle_mmio(shared_dispatcher()),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::METADATA_READ_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.dispatch_mmio_access(resolved_mmio_access(), shared_dispatcher()),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::METADATA_READ_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.configure_arm64_boot_registers(boot_registers()),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::METADATA_READ_IN_FLIGHT_MESSAGE
                ))
            );
            assert_timer_operations_rejected(
                &runner,
                HvfVcpuRunnerError::InvalidState(super::METADATA_READ_IN_FLIGHT_MESSAGE),
            );
            assert_eq!(
                runner.capture_gic_device_state(),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::METADATA_READ_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.set_gic_ppi_pending(27),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::METADATA_READ_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.clear_gic_ppi_pending(27),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::METADATA_READ_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.cancel(),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::METADATA_READ_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.shutdown(),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::METADATA_READ_IN_FLIGHT_MESSAGE
                ))
            );

            release_read_sender
                .send(Ok(0x8000_0002))
                .expect("MPIDR read release should be sent");
            assert_eq!(
                read.join().expect("MPIDR read thread should join"),
                Ok(0x8000_0002)
            );
        });

        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn mpidr_read_after_shutdown_is_rejected() {
        let runner = start_mpidr_recording_runner(0x8000_0000, false);

        runner.shutdown().expect("runner should shut down");

        assert_eq!(
            runner.mpidr_el1(),
            Err(HvfVcpuRunnerError::InvalidState(
                super::RUNNER_SHUT_DOWN_MESSAGE
            ))
        );
    }

    #[test]
    fn vtimer_operations_after_shutdown_are_rejected() {
        let runner = start_vtimer_mask_recording_runner(false, false, false);

        runner.shutdown().expect("runner should shut down");

        assert_timer_operations_rejected(
            &runner,
            HvfVcpuRunnerError::InvalidState(super::RUNNER_SHUT_DOWN_MESSAGE),
        );
    }

    #[test]
    fn interrupt_operations_after_shutdown_are_rejected() {
        let (runner, _operation_receiver) = start_gic_ppi_pending_recording_runner(false);

        runner.shutdown().expect("runner should shut down");

        assert_interrupt_operations_rejected(
            &runner,
            HvfVcpuRunnerError::InvalidState(super::RUNNER_SHUT_DOWN_MESSAGE),
        );
    }

    #[test]
    fn mpidr_read_during_shutdown_is_rejected() {
        let (runner, _, destroyed_receiver) = start_fake_runner();
        let (command_sender, should_cancel) = runner
            .prepare_shutdown()
            .expect("first shutdown should be prepared");

        assert!(!should_cancel);
        assert_eq!(
            runner.mpidr_el1(),
            Err(HvfVcpuRunnerError::InvalidState(
                super::RUNNER_SHUTTING_DOWN_MESSAGE
            ))
        );

        let thread = runner
            .take_thread()
            .expect("runner state should be lockable");
        let (response_sender, response_receiver) = mpsc::channel();
        command_sender
            .send(super::RunnerCommand::Shutdown { response_sender })
            .expect("shutdown command should be sent");
        assert_eq!(
            response_receiver
                .recv()
                .expect("shutdown response should be sent"),
            Ok(())
        );
        super::join_runner_thread(thread).expect("runner thread should join");
        runner.finish_shutdown();
        destroyed_receiver
            .recv()
            .expect("fake vCPU should be destroyed");
    }

    #[test]
    fn vtimer_operations_during_shutdown_are_rejected() {
        let (runner, _, destroyed_receiver) = start_fake_runner();
        let (command_sender, should_cancel) = runner
            .prepare_shutdown()
            .expect("first shutdown should be prepared");

        assert!(!should_cancel);
        assert_timer_operations_rejected(
            &runner,
            HvfVcpuRunnerError::InvalidState(super::RUNNER_SHUTTING_DOWN_MESSAGE),
        );

        let thread = runner
            .take_thread()
            .expect("runner state should be lockable");
        let (response_sender, response_receiver) = mpsc::channel();
        command_sender
            .send(super::RunnerCommand::Shutdown { response_sender })
            .expect("shutdown command should be sent");
        assert_eq!(
            response_receiver
                .recv()
                .expect("shutdown response should be sent"),
            Ok(())
        );
        super::join_runner_thread(thread).expect("runner thread should join");
        runner.finish_shutdown();
        destroyed_receiver
            .recv()
            .expect("fake vCPU should be destroyed");
    }

    #[test]
    fn interrupt_operations_during_shutdown_are_rejected() {
        let (runner, _, destroyed_receiver) = start_fake_runner();
        let (command_sender, should_cancel) = runner
            .prepare_shutdown()
            .expect("first shutdown should be prepared");

        assert!(!should_cancel);
        assert_interrupt_operations_rejected(
            &runner,
            HvfVcpuRunnerError::InvalidState(super::RUNNER_SHUTTING_DOWN_MESSAGE),
        );

        let thread = runner
            .take_thread()
            .expect("runner state should be lockable");
        let (response_sender, response_receiver) = mpsc::channel();
        command_sender
            .send(super::RunnerCommand::Shutdown { response_sender })
            .expect("shutdown command should be sent");
        assert_eq!(
            response_receiver
                .recv()
                .expect("shutdown response should be sent"),
            Ok(())
        );
        super::join_runner_thread(thread).expect("runner thread should join");
        runner.finish_shutdown();
        destroyed_receiver
            .recv()
            .expect("fake vCPU should be destroyed");
    }

    #[test]
    fn configures_arm64_boot_registers_before_first_run() {
        let registers = boot_registers();
        let (configured_sender, configured_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || Ok(ConfigureRecordingVcpu { configured_sender }))
            .expect("fake runner should start");
        let runner = HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
            .expect("runner should be created");

        assert_eq!(runner.configure_arm64_boot_registers(registers), Ok(()));
        assert_eq!(
            configured_receiver
                .recv()
                .expect("fake vCPU should receive boot registers"),
            registers
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn duplicate_arm64_boot_register_setup_is_rejected() {
        let (runner, _, destroyed_receiver) = start_fake_runner();

        assert_eq!(
            runner.configure_arm64_boot_registers(boot_registers()),
            Ok(())
        );
        assert_eq!(
            runner.configure_arm64_boot_registers(boot_registers()),
            Err(HvfVcpuRunnerError::InvalidState(
                super::BOOT_REGISTERS_ALREADY_CONFIGURED_MESSAGE
            ))
        );

        runner.shutdown().expect("runner should shut down");
        destroyed_receiver
            .recv()
            .expect("fake vCPU should be destroyed");
    }

    #[test]
    fn failed_arm64_boot_register_setup_can_be_retried() {
        let registers = boot_registers();
        let (configured_sender, configured_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(FailingOnceConfigureVcpu {
                configured_sender,
                fail_next_setup: true,
            })
        })
        .expect("fake runner should start");
        let runner = HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
            .expect("runner should be created");

        assert_eq!(
            runner.configure_arm64_boot_registers(registers),
            Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                "fake setup failed"
            )))
        );
        assert_eq!(
            configured_receiver
                .recv()
                .expect("fake vCPU should receive failed boot registers"),
            registers
        );
        assert_eq!(
            runner.run_once(),
            Err(HvfVcpuRunnerError::InvalidState(
                super::BOOT_REGISTER_SETUP_FAILED_MESSAGE
            ))
        );

        assert_eq!(runner.configure_arm64_boot_registers(registers), Ok(()));
        assert_eq!(
            configured_receiver
                .recv()
                .expect("fake vCPU should receive retried boot registers"),
            registers
        );
        assert_eq!(
            runner.configure_arm64_boot_registers(registers),
            Err(HvfVcpuRunnerError::InvalidState(
                super::BOOT_REGISTERS_ALREADY_CONFIGURED_MESSAGE
            ))
        );
        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn shutdown_reports_thread_panic_after_arm64_boot_register_setup_panic() {
        let (runner, runner_unwind_receiver) =
            start_panic_notifying_runner(|| Ok(PanicOnConfigureVcpu));

        assert_eq!(
            runner.configure_arm64_boot_registers(boot_registers()),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }

    #[test]
    fn arm64_boot_register_setup_after_shutdown_is_rejected() {
        let (runner, _, destroyed_receiver) = start_fake_runner();

        runner.shutdown().expect("runner should shut down");
        destroyed_receiver
            .recv()
            .expect("fake vCPU should be destroyed");

        assert_eq!(
            runner.configure_arm64_boot_registers(boot_registers()),
            Err(HvfVcpuRunnerError::InvalidState(
                super::RUNNER_SHUT_DOWN_MESSAGE
            ))
        );
    }

    #[test]
    fn arm64_boot_register_setup_during_shutdown_is_rejected() {
        let (runner, _, destroyed_receiver) = start_fake_runner();
        let (command_sender, should_cancel) = runner
            .prepare_shutdown()
            .expect("first shutdown should be prepared");

        assert!(!should_cancel);
        assert_eq!(
            runner.configure_arm64_boot_registers(boot_registers()),
            Err(HvfVcpuRunnerError::InvalidState(
                super::RUNNER_SHUTTING_DOWN_MESSAGE
            ))
        );

        let thread = runner
            .take_thread()
            .expect("runner state should be lockable");
        let (response_sender, response_receiver) = mpsc::channel();
        command_sender
            .send(super::RunnerCommand::Shutdown { response_sender })
            .expect("shutdown command should be sent");
        assert_eq!(
            response_receiver
                .recv()
                .expect("shutdown response should be sent"),
            Ok(())
        );
        super::join_runner_thread(thread).expect("runner thread should join");
        runner.finish_shutdown();
        destroyed_receiver
            .recv()
            .expect("fake vCPU should be destroyed");
    }

    #[test]
    fn arm64_boot_register_setup_during_run_is_rejected() {
        let (runner, entered_run_receiver, destroyed_receiver) = start_fake_runner();

        thread::scope(|scope| {
            let run = scope.spawn(|| runner.run_once());
            entered_run_receiver
                .recv()
                .expect("runner should enter fake run");

            assert_eq!(
                runner.configure_arm64_boot_registers(boot_registers()),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::RUN_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.mpidr_el1(),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::RUN_IN_FLIGHT_MESSAGE
                ))
            );
            assert_timer_operations_rejected(
                &runner,
                HvfVcpuRunnerError::InvalidState(super::RUN_IN_FLIGHT_MESSAGE),
            );
            assert_eq!(
                runner.capture_gic_device_state(),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::RUN_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.set_gic_ppi_pending(27),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::RUN_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.clear_gic_ppi_pending(27),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::RUN_IN_FLIGHT_MESSAGE
                ))
            );

            runner.cancel().expect("cancel should release fake run");
            assert_eq!(
                run.join().expect("run thread should join"),
                Ok(HvfVcpuExit::Canceled)
            );
        });

        runner.shutdown().expect("runner should shut down");
        destroyed_receiver
            .recv()
            .expect("fake vCPU should be destroyed");
    }

    #[test]
    fn arm64_boot_register_setup_after_run_started_is_rejected() {
        let (runner, entered_run_receiver, destroyed_receiver) = start_fake_runner();

        thread::scope(|scope| {
            let run = scope.spawn(|| runner.run_once());
            entered_run_receiver
                .recv()
                .expect("runner should enter fake run");

            runner.cancel().expect("cancel should release fake run");
            assert_eq!(
                run.join().expect("run thread should join"),
                Ok(HvfVcpuExit::Canceled)
            );
        });

        assert_eq!(
            runner.configure_arm64_boot_registers(boot_registers()),
            Err(HvfVcpuRunnerError::InvalidState(
                super::RUN_ALREADY_STARTED_MESSAGE
            ))
        );

        runner.shutdown().expect("runner should shut down");
        destroyed_receiver
            .recv()
            .expect("fake vCPU should be destroyed");
    }

    #[test]
    fn run_during_arm64_boot_register_setup_is_rejected() {
        let (entered_setup_sender, entered_setup_receiver) = mpsc::channel();
        let (release_setup_sender, release_setup_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(BlockingConfigureVcpu {
                entered_setup_sender,
                release_setup_receiver,
            })
        })
        .expect("fake runner should start");
        let runner = HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
            .expect("runner should be created");

        thread::scope(|scope| {
            let setup = scope.spawn(|| runner.configure_arm64_boot_registers(boot_registers()));
            entered_setup_receiver
                .recv()
                .expect("runner should enter fake setup");

            assert_eq!(
                runner.run_once(),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::BOOT_REGISTER_SETUP_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.capture_gic_device_state(),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::BOOT_REGISTER_SETUP_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.set_gic_ppi_pending(27),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::BOOT_REGISTER_SETUP_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.clear_gic_ppi_pending(27),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::BOOT_REGISTER_SETUP_IN_FLIGHT_MESSAGE
                ))
            );

            release_setup_sender
                .send(Ok(()))
                .expect("setup release should be sent");
            assert_eq!(setup.join().expect("setup thread should join"), Ok(()));
        });

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn cancel_holds_runner_state_until_hvf_exit_request_returns() {
        let (entered_run_sender, entered_run_receiver) = mpsc::channel();
        let (_release_run_sender, release_run_receiver) = mpsc::channel();
        let (destroyed_sender, destroyed_receiver) = mpsc::channel();
        let (entered_cancel_sender, entered_cancel_receiver) = mpsc::channel();
        let release_cancel = Arc::new((Mutex::new(false), Condvar::new()));
        let cancel_release_for_runner = Arc::clone(&release_cancel);
        let cancel_vcpu = Arc::new(move |_| {
            entered_cancel_sender
                .send(())
                .map_err(|_| BackendError::InvalidState("fake cancel entry receiver closed"))?;
            let (released, released_changed) = &*cancel_release_for_runner;
            let mut released = released
                .lock()
                .map_err(|_| BackendError::InvalidState("fake cancel release lock poisoned"))?;
            while !*released {
                released = released_changed
                    .wait(released)
                    .map_err(|_| BackendError::InvalidState("fake cancel release lock poisoned"))?;
            }
            Ok(())
        });
        let (runner, _, destroyed_receiver) = start_fake_runner_with_cancel(
            cancel_vcpu,
            entered_run_sender,
            release_run_receiver,
            destroyed_sender,
            entered_run_receiver,
            destroyed_receiver,
        );

        thread::scope(|scope| {
            let cancel_handle = runner.run_cancel_handle();
            let cancel = scope.spawn(move || cancel_handle.cancel());
            entered_cancel_receiver
                .recv()
                .expect("cancel should enter fake HVF exit request");

            assert!(runner.state.try_lock().is_err());

            let (released, released_changed) = &*release_cancel;
            *released
                .lock()
                .expect("fake cancel release lock should be lockable") = true;
            released_changed.notify_one();
            assert_eq!(cancel.join().expect("cancel thread should join"), Ok(()));
        });

        runner.shutdown().expect("runner should shut down");
        destroyed_receiver
            .recv()
            .expect("fake vCPU should be destroyed");
    }

    #[test]
    fn cancel_unblocks_in_flight_run() {
        let (runner, entered_run_receiver, destroyed_receiver) = start_fake_runner();

        thread::scope(|scope| {
            let run = scope.spawn(|| runner.run_once());
            entered_run_receiver
                .recv()
                .expect("runner should enter fake run");

            runner.cancel().expect("cancel should release fake run");

            assert_eq!(
                run.join().expect("run thread should join"),
                Ok(HvfVcpuExit::Canceled)
            );
        });

        runner.shutdown().expect("runner should shut down");
        destroyed_receiver
            .recv()
            .expect("fake vCPU should be destroyed");
    }

    #[test]
    fn cloned_run_cancel_handle_unblocks_in_flight_run() {
        let (runner, entered_run_receiver, destroyed_receiver) = start_fake_runner();
        let cancel_handle = runner.run_cancel_handle();
        let cloned_cancel_handle = cancel_handle.clone();

        thread::scope(|scope| {
            let run = scope.spawn(|| runner.run_once());
            entered_run_receiver
                .recv()
                .expect("runner should enter fake run");

            cloned_cancel_handle
                .cancel()
                .expect("cloned cancel handle should release fake run");

            assert_eq!(
                run.join().expect("run thread should join"),
                Ok(HvfVcpuExit::Canceled)
            );
        });

        runner.shutdown().expect("runner should shut down");
        destroyed_receiver
            .recv()
            .expect("fake vCPU should be destroyed");
    }

    #[test]
    fn run_cancel_handle_after_shutdown_is_rejected_without_calling_hvf_cancel() {
        let (entered_run_sender, entered_run_receiver) = mpsc::channel();
        let (release_run_sender, release_run_receiver) = mpsc::channel();
        let (destroyed_sender, destroyed_receiver) = mpsc::channel();
        let cancel_calls = Arc::new(Mutex::new(0usize));
        let cancel_calls_for_runner = Arc::clone(&cancel_calls);
        let cancel_vcpu = Arc::new(move |_| {
            *cancel_calls_for_runner
                .lock()
                .map_err(|_| BackendError::InvalidState("fake cancel call lock poisoned"))? += 1;
            release_run_sender
                .send(Ok(HvfVcpuExit::Canceled))
                .map_err(|_| BackendError::InvalidState("fake run release receiver closed"))
        });
        let (runner, _, destroyed_receiver) = start_fake_runner_with_cancel(
            cancel_vcpu,
            entered_run_sender,
            release_run_receiver,
            destroyed_sender,
            entered_run_receiver,
            destroyed_receiver,
        );
        let cancel_handle = runner.run_cancel_handle();

        runner.shutdown().expect("runner should shut down");

        assert_eq!(
            cancel_handle.cancel(),
            Err(HvfVcpuRunnerError::InvalidState(
                super::RUNNER_SHUT_DOWN_MESSAGE
            ))
        );
        assert_eq!(
            *cancel_calls
                .lock()
                .expect("fake cancel call count should lock"),
            0
        );
        destroyed_receiver
            .recv()
            .expect("fake vCPU should be destroyed");
    }

    #[test]
    fn run_cancel_handle_after_runner_drop_is_rejected_without_calling_hvf_cancel() {
        let (entered_run_sender, entered_run_receiver) = mpsc::channel();
        let (release_run_sender, release_run_receiver) = mpsc::channel();
        let (destroyed_sender, destroyed_receiver) = mpsc::channel();
        let cancel_calls = Arc::new(Mutex::new(0usize));
        let cancel_calls_for_runner = Arc::clone(&cancel_calls);
        let cancel_vcpu = Arc::new(move |_| {
            *cancel_calls_for_runner
                .lock()
                .map_err(|_| BackendError::InvalidState("fake cancel call lock poisoned"))? += 1;
            release_run_sender
                .send(Ok(HvfVcpuExit::Canceled))
                .map_err(|_| BackendError::InvalidState("fake run release receiver closed"))
        });
        let (runner, _, destroyed_receiver) = start_fake_runner_with_cancel(
            cancel_vcpu,
            entered_run_sender,
            release_run_receiver,
            destroyed_sender,
            entered_run_receiver,
            destroyed_receiver,
        );
        let cancel_handle = runner.run_cancel_handle();

        drop(runner);

        assert_eq!(
            cancel_handle.cancel(),
            Err(HvfVcpuRunnerError::InvalidState(
                super::RUNNER_SHUT_DOWN_MESSAGE
            ))
        );
        assert_eq!(
            *cancel_calls
                .lock()
                .expect("fake cancel call count should lock"),
            0
        );
        destroyed_receiver
            .recv()
            .expect("fake vCPU should be destroyed");
    }

    #[test]
    fn shutdown_cancels_in_flight_run_and_joins_thread() {
        let (runner, entered_run_receiver, destroyed_receiver) = start_fake_runner();

        thread::scope(|scope| {
            let run = scope.spawn(|| runner.run_once());
            entered_run_receiver
                .recv()
                .expect("runner should enter fake run");

            runner.shutdown().expect("shutdown should cancel fake run");

            assert_eq!(
                run.join().expect("run thread should join"),
                Ok(HvfVcpuExit::Canceled)
            );
        });

        destroyed_receiver
            .recv()
            .expect("fake vCPU should be destroyed");
        runner
            .shutdown()
            .expect("repeated shutdown should be idempotent");
    }

    #[test]
    fn shutdown_cancel_error_keeps_in_flight_run_and_allows_retry() {
        let (entered_run_sender, entered_run_receiver) = mpsc::channel();
        let (release_run_sender, release_run_receiver) = mpsc::channel();
        let (destroyed_sender, destroyed_receiver) = mpsc::channel();
        let fail_next_cancel = Arc::new(Mutex::new(true));
        let fail_next_cancel_for_runner = Arc::clone(&fail_next_cancel);
        let cancel_vcpu = Arc::new(move |_| {
            let mut fail_next = fail_next_cancel_for_runner
                .lock()
                .map_err(|_| BackendError::InvalidState("fake cancel state lock poisoned"))?;
            if *fail_next {
                *fail_next = false;
                return Err(BackendError::InvalidState("fake cancel failed"));
            }

            release_run_sender
                .send(Ok(HvfVcpuExit::Canceled))
                .map_err(|_| BackendError::InvalidState("fake run release receiver closed"))
        });
        let (runner, entered_run_receiver, destroyed_receiver) = start_fake_runner_with_cancel(
            cancel_vcpu,
            entered_run_sender,
            release_run_receiver,
            destroyed_sender,
            entered_run_receiver,
            destroyed_receiver,
        );

        thread::scope(|scope| {
            let run = scope.spawn(|| runner.run_once());
            entered_run_receiver
                .recv()
                .expect("runner should enter fake run");

            assert_eq!(
                runner.shutdown(),
                Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                    "fake cancel failed"
                )))
            );
            assert_eq!(
                runner.run_once(),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::RUN_IN_FLIGHT_MESSAGE
                ))
            );

            runner
                .shutdown()
                .expect("shutdown retry should cancel and join fake run");
            assert_eq!(
                run.join().expect("run thread should join"),
                Ok(HvfVcpuExit::Canceled)
            );
        });

        destroyed_receiver
            .recv()
            .expect("fake vCPU should be destroyed");
    }

    #[test]
    fn shutdown_in_progress_rejects_second_shutdown_command_and_cancel() {
        let (runner, _, destroyed_receiver) = start_fake_runner();
        let (command_sender, should_cancel) = runner
            .prepare_shutdown()
            .expect("first shutdown should be prepared");

        assert!(!should_cancel);

        let Err(err) = runner.prepare_shutdown() else {
            panic!("second shutdown should not be prepared");
        };
        assert_eq!(
            err,
            HvfVcpuRunnerError::InvalidState(super::RUNNER_SHUTTING_DOWN_MESSAGE)
        );
        assert_eq!(
            runner.shutdown(),
            Err(HvfVcpuRunnerError::InvalidState(
                super::RUNNER_SHUTTING_DOWN_MESSAGE
            ))
        );
        assert_eq!(
            runner.cancel(),
            Err(HvfVcpuRunnerError::InvalidState(
                super::RUNNER_SHUTTING_DOWN_MESSAGE
            ))
        );
        assert_eq!(
            runner.set_gic_ppi_pending(27),
            Err(HvfVcpuRunnerError::InvalidState(
                super::RUNNER_SHUTTING_DOWN_MESSAGE
            ))
        );
        assert_eq!(
            runner.clear_gic_ppi_pending(27),
            Err(HvfVcpuRunnerError::InvalidState(
                super::RUNNER_SHUTTING_DOWN_MESSAGE
            ))
        );
        let cancel_handle = runner.run_cancel_handle();
        assert_eq!(
            cancel_handle.cancel(),
            Err(HvfVcpuRunnerError::InvalidState(
                super::RUNNER_SHUTTING_DOWN_MESSAGE
            ))
        );
        let thread = runner
            .take_thread()
            .expect("runner state should be lockable");
        assert_eq!(
            runner.shutdown(),
            Err(HvfVcpuRunnerError::InvalidState(
                super::RUNNER_SHUTTING_DOWN_MESSAGE
            ))
        );

        let (response_sender, response_receiver) = mpsc::channel();
        command_sender
            .send(super::RunnerCommand::Shutdown { response_sender })
            .expect("shutdown command should be sent");
        let response = response_receiver
            .recv()
            .expect("shutdown response should be sent");

        assert_eq!(response, Ok(()));
        super::join_runner_thread(thread).expect("runner thread should join");
        runner.finish_shutdown();
        runner
            .shutdown()
            .expect("completed shutdown should be idempotent");
        destroyed_receiver
            .recv()
            .expect("fake vCPU should be destroyed");
    }

    #[test]
    fn concurrent_run_once_is_rejected() {
        let (runner, entered_run_receiver, destroyed_receiver) = start_fake_runner();

        thread::scope(|scope| {
            let run = scope.spawn(|| runner.run_once());
            entered_run_receiver
                .recv()
                .expect("runner should enter fake run");

            assert_eq!(
                runner.run_once(),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::RUN_IN_FLIGHT_MESSAGE
                ))
            );

            runner.cancel().expect("cancel should release fake run");
            assert_eq!(
                run.join().expect("run thread should join"),
                Ok(HvfVcpuExit::Canceled)
            );
        });

        runner.shutdown().expect("runner should shut down");
        destroyed_receiver
            .recv()
            .expect("fake vCPU should be destroyed");
    }

    #[test]
    fn run_once_and_handle_mmio_dispatches_mmio_exit_on_runner_thread() {
        let access = resolved_mmio_access();
        let (runner, dispatched_receiver, register_write_receiver) =
            start_run_step_recording_runner(
                Ok(mmio_exception_exit()),
                Ok(MmioDispatchOutcome::Write),
            );
        let dispatcher = shared_dispatcher_with_region();

        assert_eq!(
            runner.run_once_and_handle_mmio(Arc::clone(&dispatcher)),
            Ok(HvfVcpuRunStepOutcome::Mmio {
                access,
                outcome: MmioDispatchOutcome::Write,
            })
        );
        assert_eq!(
            dispatched_receiver
                .recv()
                .expect("fake vCPU should receive dispatch"),
            access
        );
        assert_eq!(
            register_write_receiver
                .recv()
                .expect("fake vCPU should advance PC"),
            (HvfRegister::PC, 0x8020_3004)
        );
        let region_ids = dispatcher
            .lock()
            .expect("dispatcher lock should not be poisoned")
            .regions()
            .iter()
            .map(|region| region.id())
            .collect::<Vec<_>>();
        assert_eq!(
            region_ids,
            vec![MmioRegionId::new(7), MmioRegionId::new(99)]
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn run_once_and_handle_mmio_returns_non_mmio_exits_without_dispatcher_lock() {
        for (exit, outcome) in [
            (HvfVcpuExit::Canceled, HvfVcpuRunStepOutcome::Canceled),
            (
                HvfVcpuExit::VtimerActivated,
                HvfVcpuRunStepOutcome::VtimerActivated,
            ),
            (
                HvfVcpuExit::Unknown { reason: 99 },
                HvfVcpuRunStepOutcome::Unknown { reason: 99 },
            ),
        ] {
            let runner = start_run_step_exit_runner(Ok(exit));
            let dispatcher = shared_dispatcher();
            let dispatcher_guard = dispatcher
                .lock()
                .expect("dispatcher lock should not be poisoned");

            assert_eq!(
                runner.run_once_and_handle_mmio(Arc::clone(&dispatcher)),
                Ok(outcome)
            );
            drop(dispatcher_guard);

            runner.shutdown().expect("runner should shut down");
        }
    }

    #[test]
    fn run_once_and_handle_mmio_handles_psci_hvc_without_dispatcher_lock() {
        let (runner, register_write_receiver) =
            start_psci_run_step_recording_runner(PSCI_VERSION, 0);
        let dispatcher = shared_dispatcher();
        let dispatcher_guard = dispatcher
            .lock()
            .expect("dispatcher lock should not be poisoned");

        assert_eq!(
            runner.run_once_and_handle_mmio(Arc::clone(&dispatcher)),
            Ok(HvfVcpuRunStepOutcome::Hvc {
                exit: hvc_exit(0),
                function_id: PSCI_VERSION,
                return_value: PSCI_VERSION_0_2,
            })
        );
        drop(dispatcher_guard);
        assert_eq!(
            register_write_receiver
                .recv()
                .expect("PSCI HVC should write X0"),
            (HvfRegister::X0, PSCI_VERSION_0_2)
        );
        assert_eq!(
            register_write_receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn run_once_and_handle_mmio_reads_psci_features_argument() {
        let (runner, register_write_receiver) =
            start_psci_run_step_recording_runner(PSCI_FEATURES, PSCI_VERSION);

        assert_eq!(
            runner.run_once_and_handle_mmio(shared_dispatcher()),
            Ok(HvfVcpuRunStepOutcome::Hvc {
                exit: hvc_exit(0),
                function_id: PSCI_FEATURES,
                return_value: PSCI_RET_SUCCESS,
            })
        );
        assert_eq!(
            register_write_receiver
                .recv()
                .expect("PSCI_FEATURES should write X0"),
            (HvfRegister::X0, PSCI_RET_SUCCESS)
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn run_once_and_handle_mmio_returns_guest_shutdown_for_psci_system_off() {
        let (runner, register_write_receiver) = start_psci_run_step_recording_runner_with_x1(
            PSCI_SYSTEM_OFF,
            Err(BackendError::InvalidState("X1 should not be read")),
        );

        assert_eq!(
            runner.run_once_and_handle_mmio(shared_dispatcher()),
            Ok(HvfVcpuRunStepOutcome::GuestShutdown {
                exit: hvc_exit(0),
                function_id: PSCI_SYSTEM_OFF,
                return_value: PSCI_RET_SUCCESS,
            })
        );
        assert_eq!(
            register_write_receiver
                .recv()
                .expect("PSCI_SYSTEM_OFF should write X0"),
            (HvfRegister::X0, PSCI_RET_SUCCESS)
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn run_once_and_handle_mmio_returns_guest_reset_for_psci_system_reset() {
        let (runner, register_write_receiver) = start_psci_run_step_recording_runner_with_x1(
            PSCI_SYSTEM_RESET,
            Err(BackendError::InvalidState("X1 should not be read")),
        );

        assert_eq!(
            runner.run_once_and_handle_mmio(shared_dispatcher()),
            Ok(HvfVcpuRunStepOutcome::GuestReset {
                exit: hvc_exit(0),
                function_id: PSCI_SYSTEM_RESET,
                return_value: PSCI_RET_SUCCESS,
            })
        );
        assert_eq!(
            register_write_receiver
                .recv()
                .expect("PSCI_SYSTEM_RESET should write X0"),
            (HvfRegister::X0, PSCI_RET_SUCCESS)
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn run_once_and_handle_mmio_rejects_nonzero_hvc_immediate_without_reading_arg0() {
        let (runner, register_write_receiver) = start_psci_run_step_recording_runner_with_exit(
            PSCI_VERSION,
            Err(BackendError::InvalidState("X1 should not be read")),
            1,
        );

        assert_eq!(
            runner.run_once_and_handle_mmio(shared_dispatcher()),
            Ok(HvfVcpuRunStepOutcome::Hvc {
                exit: hvc_exit(1),
                function_id: PSCI_VERSION,
                return_value: PSCI_RET_NOT_SUPPORTED,
            })
        );
        assert_eq!(
            register_write_receiver
                .recv()
                .expect("nonzero HVC immediate should still write X0"),
            (HvfRegister::X0, PSCI_RET_NOT_SUPPORTED)
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn run_once_and_handle_mmio_rejects_nonzero_hvc_immediate_without_guest_shutdown() {
        let (runner, register_write_receiver) = start_psci_run_step_recording_runner_with_exit(
            PSCI_SYSTEM_OFF,
            Err(BackendError::InvalidState("X1 should not be read")),
            1,
        );

        assert_eq!(
            runner.run_once_and_handle_mmio(shared_dispatcher()),
            Ok(HvfVcpuRunStepOutcome::Hvc {
                exit: hvc_exit(1),
                function_id: PSCI_SYSTEM_OFF,
                return_value: PSCI_RET_NOT_SUPPORTED,
            })
        );
        assert_eq!(
            register_write_receiver
                .recv()
                .expect("nonzero HVC immediate should still write X0"),
            (HvfRegister::X0, PSCI_RET_NOT_SUPPORTED)
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn run_once_and_handle_mmio_returns_not_supported_for_unsupported_psci_hvc() {
        let (runner, register_write_receiver) = start_psci_run_step_recording_runner_with_x1(
            PSCI_CPU_ON,
            Err(BackendError::InvalidState("X1 should not be read")),
        );

        assert_eq!(
            runner.run_once_and_handle_mmio(shared_dispatcher()),
            Ok(HvfVcpuRunStepOutcome::Hvc {
                exit: hvc_exit(0),
                function_id: PSCI_CPU_ON,
                return_value: PSCI_RET_NOT_SUPPORTED,
            })
        );
        assert_eq!(
            register_write_receiver
                .recv()
                .expect("unsupported PSCI HVC should still write X0"),
            (HvfRegister::X0, PSCI_RET_NOT_SUPPORTED)
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn run_once_and_handle_mmio_advances_pc_after_osdlr_sys64_write_without_dispatcher_lock() {
        let (runner, register_write_receiver) = start_sys64_run_step_recording_runner(
            HvfSys64Direction::Write,
            HvfSys64Register::OSDLR_EL1,
            2,
        );
        let dispatcher = shared_dispatcher();
        let dispatcher_guard = dispatcher
            .lock()
            .expect("dispatcher lock should not be poisoned");

        assert_eq!(
            runner.run_once_and_handle_mmio(Arc::clone(&dispatcher)),
            Ok(HvfVcpuRunStepOutcome::Sys64 {
                exit: sys64_exit(HvfSys64Direction::Write, HvfSys64Register::OSDLR_EL1, 2),
            })
        );
        drop(dispatcher_guard);
        assert_eq!(
            register_write_receiver.try_recv(),
            Ok((HvfRegister::PC, 0x8020_1004))
        );
        assert_eq!(
            register_write_receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn run_once_and_handle_mmio_advances_pc_after_oslar_sys64_write_without_dispatcher_lock() {
        let (runner, register_write_receiver) = start_sys64_run_step_recording_runner(
            HvfSys64Direction::Write,
            HvfSys64Register::OSLAR_EL1,
            31,
        );
        let dispatcher = shared_dispatcher();
        let dispatcher_guard = dispatcher
            .lock()
            .expect("dispatcher lock should not be poisoned");

        assert_eq!(
            runner.run_once_and_handle_mmio(Arc::clone(&dispatcher)),
            Ok(HvfVcpuRunStepOutcome::Sys64 {
                exit: sys64_exit(HvfSys64Direction::Write, HvfSys64Register::OSLAR_EL1, 31),
            })
        );
        drop(dispatcher_guard);
        assert_eq!(
            register_write_receiver.try_recv(),
            Ok((HvfRegister::PC, 0x8020_1004))
        );
        assert_eq!(
            register_write_receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn run_once_and_handle_mmio_rejects_pc_overflow_after_supported_sys64_write() {
        let (runner, register_write_receiver) = start_sys64_run_step_recording_runner_with_pc(
            HvfSys64Direction::Write,
            HvfSys64Register::OSDLR_EL1,
            31,
            u64::MAX,
        );

        assert_eq!(
            runner.run_once_and_handle_mmio(shared_dispatcher()),
            Err(HvfVcpuRunnerError::InvalidState(
                "arm64 PC overflow while advancing handled synchronous exit"
            ))
        );
        assert_eq!(
            register_write_receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn run_once_and_handle_mmio_writes_zero_for_osdlr_sys64_read() {
        let (runner, register_write_receiver) = start_sys64_run_step_recording_runner(
            HvfSys64Direction::Read,
            HvfSys64Register::OSDLR_EL1,
            2,
        );

        assert_eq!(
            runner.run_once_and_handle_mmio(shared_dispatcher()),
            Ok(HvfVcpuRunStepOutcome::Sys64 {
                exit: sys64_exit(HvfSys64Direction::Read, HvfSys64Register::OSDLR_EL1, 2),
            })
        );
        assert_eq!(
            register_write_receiver.try_recv(),
            Ok((HvfRegister::general_purpose(2).expect("X2 should map"), 0))
        );
        assert_eq!(
            register_write_receiver.try_recv(),
            Ok((HvfRegister::PC, 0x8020_1004))
        );
        assert_eq!(
            register_write_receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn run_once_and_handle_mmio_ignores_osdlr_sys64_read_to_xzr() {
        let (runner, register_write_receiver) = start_sys64_run_step_recording_runner(
            HvfSys64Direction::Read,
            HvfSys64Register::OSDLR_EL1,
            31,
        );

        assert_eq!(
            runner.run_once_and_handle_mmio(shared_dispatcher()),
            Ok(HvfVcpuRunStepOutcome::Sys64 {
                exit: sys64_exit(HvfSys64Direction::Read, HvfSys64Register::OSDLR_EL1, 31,),
            })
        );
        assert_eq!(
            register_write_receiver.try_recv(),
            Ok((HvfRegister::PC, 0x8020_1004))
        );
        assert_eq!(
            register_write_receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn run_once_and_handle_mmio_rejects_unsupported_sys64_without_dispatcher_lock() {
        let unsupported_register =
            HvfSys64Register::new(3, 0, 0, 0, 0).expect("SYS64 register should be valid");
        let (runner, register_write_receiver) =
            start_sys64_run_step_recording_runner(HvfSys64Direction::Read, unsupported_register, 0);
        let dispatcher = shared_dispatcher();
        let dispatcher_guard = dispatcher
            .lock()
            .expect("dispatcher lock should not be poisoned");

        assert_eq!(
            runner.run_once_and_handle_mmio(Arc::clone(&dispatcher)),
            Err(HvfVcpuRunnerError::UnsupportedSys64 {
                exit: sys64_exit(HvfSys64Direction::Read, unsupported_register, 0),
            })
        );
        drop(dispatcher_guard);
        assert_eq!(
            register_write_receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn run_once_and_handle_mmio_preserves_decode_errors() {
        let exit = HvfVcpuExit::Exception(HvfExceptionExit {
            syndrome: 0,
            virtual_address: 0x2000,
            physical_address: 0x1040,
        });
        let runner = start_run_step_exit_runner(Ok(exit));
        let dispatcher = shared_dispatcher();
        let dispatcher_guard = dispatcher
            .lock()
            .expect("dispatcher lock should not be poisoned");

        assert_eq!(
            runner.run_once_and_handle_mmio(Arc::clone(&dispatcher)),
            Err(HvfVcpuRunnerError::VcpuExitResolve(
                HvfVcpuExitResolveError::MmioDecode {
                    exit: HvfExceptionExit {
                        syndrome: 0,
                        virtual_address: 0x2000,
                        physical_address: 0x1040,
                    },
                    source: crate::exit::HvfMmioDecodeError::UnsupportedExceptionClass {
                        exception_class: 0,
                    },
                }
            ))
        );
        drop(dispatcher_guard);
        assert_eq!(runner.run_once(), Ok(exit));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn run_once_and_handle_mmio_preserves_run_errors() {
        let runner = start_run_step_exit_runner(Err(BackendError::InvalidState("fake run failed")));

        assert_eq!(
            runner.run_once_and_handle_mmio(shared_dispatcher()),
            Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                "fake run failed"
            )))
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn run_once_and_handle_mmio_preserves_resolve_errors() {
        let runner = start_run_step_exit_runner(Ok(mmio_exception_exit()));

        let Err(HvfVcpuRunnerError::VcpuExitResolve(HvfVcpuExitResolveError::MmioResolve {
            ..
        })) = runner.run_once_and_handle_mmio(shared_dispatcher())
        else {
            panic!("unregistered MMIO exit should fail resolution");
        };
        assert_eq!(runner.run_once(), Ok(mmio_exception_exit()));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn run_once_and_handle_mmio_preserves_dispatch_errors() {
        let source = HvfMmioDispatchError::Operation {
            source: HvfMmioCompletionError::UnsupportedRegister {
                register: HvfMmioRegister::new(31).expect("test register should decode"),
            },
        };
        let (runner, dispatched_receiver, register_write_receiver) =
            start_run_step_recording_runner(
                Ok(mmio_exception_exit()),
                Err(HvfVcpuRunnerError::MmioDispatch(source.clone())),
            );
        let dispatcher = shared_dispatcher_with_region();

        assert_eq!(
            runner.run_once_and_handle_mmio(dispatcher),
            Err(HvfVcpuRunnerError::MmioDispatch(source))
        );
        assert_eq!(
            dispatched_receiver
                .recv()
                .expect("fake vCPU should receive dispatch"),
            resolved_mmio_access()
        );
        assert_eq!(
            register_write_receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        );
        assert_eq!(runner.run_once(), Ok(mmio_exception_exit()));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn run_once_and_handle_mmio_reports_poisoned_dispatcher_lock() {
        let runner = start_run_step_exit_runner(Ok(mmio_exception_exit()));
        let dispatcher = shared_dispatcher_with_region();

        let _ = std::panic::catch_unwind({
            let dispatcher = Arc::clone(&dispatcher);
            move || {
                let _guard = dispatcher
                    .lock()
                    .expect("dispatcher lock should not be poisoned yet");
                panic!("poison test dispatcher");
            }
        });

        assert_eq!(
            runner.run_once_and_handle_mmio(dispatcher),
            Err(HvfVcpuRunnerError::InvalidState(
                super::MMIO_DISPATCHER_POISONED_MESSAGE
            ))
        );
        assert_eq!(runner.run_once(), Ok(mmio_exception_exit()));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn run_once_and_handle_mmio_rejects_busy_dispatcher_lock_without_blocking() {
        let runner = start_run_step_exit_runner(Ok(mmio_exception_exit()));
        let dispatcher = shared_dispatcher_with_region();
        let dispatcher_guard = dispatcher
            .lock()
            .expect("dispatcher lock should not be poisoned");

        assert_eq!(
            runner.run_once_and_handle_mmio(Arc::clone(&dispatcher)),
            Err(HvfVcpuRunnerError::InvalidState(
                super::MMIO_DISPATCHER_BUSY_MESSAGE
            ))
        );
        drop(dispatcher_guard);
        assert_eq!(runner.run_once(), Ok(mmio_exception_exit()));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn concurrent_run_once_and_handle_mmio_is_rejected_without_queueing() {
        let (runner, entered_run_receiver, destroyed_receiver) = start_fake_runner();

        thread::scope(|scope| {
            let run_step =
                scope.spawn(|| runner.run_once_and_handle_mmio(shared_dispatcher_with_region()));
            entered_run_receiver
                .recv()
                .expect("runner should enter fake run");

            assert_eq!(
                runner.run_once_and_handle_mmio(shared_dispatcher_with_region()),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::RUN_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.run_once(),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::RUN_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.dispatch_mmio_access(resolved_mmio_access(), shared_dispatcher()),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::RUN_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.configure_arm64_boot_registers(boot_registers()),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::RUN_IN_FLIGHT_MESSAGE
                ))
            );
            assert_timer_operations_rejected(
                &runner,
                HvfVcpuRunnerError::InvalidState(super::RUN_IN_FLIGHT_MESSAGE),
            );
            assert_eq!(
                runner.set_gic_ppi_pending(27),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::RUN_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.clear_gic_ppi_pending(27),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::RUN_IN_FLIGHT_MESSAGE
                ))
            );

            runner.cancel().expect("cancel should release fake run");
            assert_eq!(
                run_step.join().expect("run step thread should join"),
                Ok(HvfVcpuRunStepOutcome::Canceled)
            );
        });

        runner.shutdown().expect("runner should shut down");
        destroyed_receiver
            .recv()
            .expect("fake vCPU should be destroyed");
    }

    #[test]
    fn run_once_and_handle_mmio_after_shutdown_is_rejected() {
        let runner = start_run_step_exit_runner(Ok(HvfVcpuExit::Canceled));

        runner.shutdown().expect("runner should shut down");
        assert_eq!(
            runner.run_once_and_handle_mmio(shared_dispatcher()),
            Err(HvfVcpuRunnerError::InvalidState(
                super::RUNNER_SHUT_DOWN_MESSAGE
            ))
        );
    }

    #[test]
    fn run_once_and_handle_mmio_during_shutdown_is_rejected() {
        let (runner, _, destroyed_receiver) = start_fake_runner();
        let (command_sender, should_cancel) = runner
            .prepare_shutdown()
            .expect("first shutdown should be prepared");

        assert!(!should_cancel);
        assert_eq!(
            runner.run_once_and_handle_mmio(shared_dispatcher()),
            Err(HvfVcpuRunnerError::InvalidState(
                super::RUNNER_SHUT_DOWN_MESSAGE
            ))
        );

        let thread = runner
            .take_thread()
            .expect("runner state should be lockable");
        let (response_sender, response_receiver) = mpsc::channel();
        command_sender
            .send(super::RunnerCommand::Shutdown { response_sender })
            .expect("shutdown command should be sent");
        assert_eq!(
            response_receiver
                .recv()
                .expect("shutdown response should be sent"),
            Ok(())
        );
        super::join_runner_thread(thread).expect("runner thread should join");
        runner.finish_shutdown();
        destroyed_receiver
            .recv()
            .expect("fake vCPU should be destroyed");
    }

    #[test]
    fn run_once_and_handle_mmio_after_failed_boot_register_setup_is_rejected() {
        let registers = boot_registers();
        let (configured_sender, configured_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(FailingOnceConfigureVcpu {
                configured_sender,
                fail_next_setup: true,
            })
        })
        .expect("fake runner should start");
        let runner = HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
            .expect("runner should be created");

        assert_eq!(
            runner.configure_arm64_boot_registers(registers),
            Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                "fake setup failed"
            )))
        );
        assert_eq!(
            configured_receiver
                .recv()
                .expect("fake vCPU should receive failed boot registers"),
            registers
        );
        assert_eq!(
            runner.run_once_and_handle_mmio(shared_dispatcher()),
            Err(HvfVcpuRunnerError::InvalidState(
                super::BOOT_REGISTER_SETUP_FAILED_MESSAGE
            ))
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn dispatch_mmio_access_runs_on_runner_thread_and_preserves_dispatcher_state() {
        let access = resolved_mmio_access();
        let (runner, dispatched_receiver) =
            start_dispatch_recording_runner(Ok(MmioDispatchOutcome::Write));
        let dispatcher = shared_dispatcher();

        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
        assert_eq!(
            runner.dispatch_mmio_access(access, Arc::clone(&dispatcher)),
            Ok(MmioDispatchOutcome::Write)
        );
        assert_eq!(
            dispatched_receiver
                .recv()
                .expect("fake vCPU should receive dispatch"),
            access
        );
        assert_eq!(
            dispatcher
                .lock()
                .expect("dispatcher lock should not be poisoned")
                .regions()[0]
                .id(),
            MmioRegionId::new(99)
        );

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn dispatch_mmio_access_preserves_mmio_dispatch_errors() {
        let access = resolved_mmio_access();
        let source = HvfMmioDispatchError::Operation {
            source: HvfMmioCompletionError::UnsupportedRegister {
                register: HvfMmioRegister::new(31).expect("test register should decode"),
            },
        };
        let (runner, dispatched_receiver) =
            start_dispatch_recording_runner(Err(HvfVcpuRunnerError::MmioDispatch(source.clone())));

        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
        assert_eq!(
            runner.dispatch_mmio_access(access, shared_dispatcher()),
            Err(HvfVcpuRunnerError::MmioDispatch(source))
        );
        assert_eq!(
            dispatched_receiver
                .recv()
                .expect("fake vCPU should receive dispatch"),
            access
        );
        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn dispatch_mmio_access_before_first_run_is_rejected() {
        let (runner, dispatched_receiver) =
            start_dispatch_recording_runner(Ok(MmioDispatchOutcome::Write));

        assert_eq!(
            runner.dispatch_mmio_access(resolved_mmio_access(), shared_dispatcher()),
            Err(HvfVcpuRunnerError::InvalidState(
                super::RUN_NOT_STARTED_MESSAGE
            ))
        );
        assert!(dispatched_receiver.try_recv().is_err());

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn dispatch_mmio_access_during_run_is_rejected() {
        let (runner, entered_run_receiver, destroyed_receiver) = start_fake_runner();

        thread::scope(|scope| {
            let run = scope.spawn(|| runner.run_once());
            entered_run_receiver
                .recv()
                .expect("runner should enter fake run");

            assert_core_register_captures_rejected(
                &runner,
                HvfVcpuRunnerError::InvalidState(super::RUN_IN_FLIGHT_MESSAGE),
            );
            assert_eq!(
                runner.dispatch_mmio_access(resolved_mmio_access(), shared_dispatcher()),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::RUN_IN_FLIGHT_MESSAGE
                ))
            );

            runner.cancel().expect("cancel should release fake run");
            assert_eq!(
                run.join().expect("run thread should join"),
                Ok(HvfVcpuExit::Canceled)
            );
        });

        runner.shutdown().expect("runner should shut down");
        destroyed_receiver
            .recv()
            .expect("fake vCPU should be destroyed");
    }

    #[test]
    fn dispatch_mmio_access_during_arm64_boot_register_setup_is_rejected() {
        let (entered_setup_sender, entered_setup_receiver) = mpsc::channel();
        let (release_setup_sender, release_setup_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(BlockingConfigureVcpu {
                entered_setup_sender,
                release_setup_receiver,
            })
        })
        .expect("fake runner should start");
        let runner = HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
            .expect("runner should be created");

        thread::scope(|scope| {
            let setup = scope.spawn(|| runner.configure_arm64_boot_registers(boot_registers()));
            entered_setup_receiver
                .recv()
                .expect("runner should enter fake setup");

            assert_core_register_captures_rejected(
                &runner,
                HvfVcpuRunnerError::InvalidState(super::BOOT_REGISTER_SETUP_IN_FLIGHT_MESSAGE),
            );
            assert_eq!(
                runner.dispatch_mmio_access(resolved_mmio_access(), shared_dispatcher()),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::BOOT_REGISTER_SETUP_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.mpidr_el1(),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::BOOT_REGISTER_SETUP_IN_FLIGHT_MESSAGE
                ))
            );
            assert_timer_operations_rejected(
                &runner,
                HvfVcpuRunnerError::InvalidState(super::BOOT_REGISTER_SETUP_IN_FLIGHT_MESSAGE),
            );
            assert_eq!(
                runner.set_gic_ppi_pending(27),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::BOOT_REGISTER_SETUP_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.clear_gic_ppi_pending(27),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::BOOT_REGISTER_SETUP_IN_FLIGHT_MESSAGE
                ))
            );

            release_setup_sender
                .send(Ok(()))
                .expect("setup release should be sent");
            assert_eq!(setup.join().expect("setup thread should join"), Ok(()));
        });

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn concurrent_mmio_dispatch_is_rejected_without_queueing() {
        let access = resolved_mmio_access();
        let (entered_dispatch_sender, entered_dispatch_receiver) = mpsc::channel();
        let (release_dispatch_sender, release_dispatch_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(BlockingMmioDispatchVcpu {
                entered_dispatch_sender,
                release_dispatch_receiver,
                pc: 0x8020_4000,
            })
        })
        .expect("fake runner should start");
        let runner = HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
            .expect("runner should be created");

        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
        thread::scope(|scope| {
            let first_dispatch =
                scope.spawn(|| runner.dispatch_mmio_access(access, shared_dispatcher()));
            entered_dispatch_receiver
                .recv()
                .expect("runner should enter fake MMIO dispatch");

            assert_core_register_captures_rejected(
                &runner,
                HvfVcpuRunnerError::InvalidState(super::MMIO_DISPATCH_IN_FLIGHT_MESSAGE),
            );
            assert_eq!(
                runner.dispatch_mmio_access(access, shared_dispatcher()),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::MMIO_DISPATCH_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.run_once(),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::MMIO_DISPATCH_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.cancel(),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::MMIO_DISPATCH_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.mpidr_el1(),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::MMIO_DISPATCH_IN_FLIGHT_MESSAGE
                ))
            );
            assert_timer_operations_rejected(
                &runner,
                HvfVcpuRunnerError::InvalidState(super::MMIO_DISPATCH_IN_FLIGHT_MESSAGE),
            );
            assert_eq!(
                runner.capture_gic_device_state(),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::MMIO_DISPATCH_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.set_gic_ppi_pending(27),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::MMIO_DISPATCH_IN_FLIGHT_MESSAGE
                ))
            );
            assert_eq!(
                runner.clear_gic_ppi_pending(27),
                Err(HvfVcpuRunnerError::InvalidState(
                    super::MMIO_DISPATCH_IN_FLIGHT_MESSAGE
                ))
            );

            release_dispatch_sender
                .send(Ok(MmioDispatchOutcome::Write))
                .expect("dispatch release should be sent");
            assert_eq!(
                first_dispatch.join().expect("dispatch thread should join"),
                Ok(MmioDispatchOutcome::Write)
            );
        });

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn dispatch_mmio_access_reports_poisoned_dispatcher_lock() {
        let (runner, _) = start_dispatch_recording_runner(Ok(MmioDispatchOutcome::Write));
        let dispatcher = shared_dispatcher();

        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
        let _ = std::panic::catch_unwind({
            let dispatcher = Arc::clone(&dispatcher);
            move || {
                let _guard = dispatcher
                    .lock()
                    .expect("dispatcher lock should not be poisoned yet");
                panic!("poison test dispatcher");
            }
        });

        assert_eq!(
            runner.dispatch_mmio_access(resolved_mmio_access(), dispatcher),
            Err(HvfVcpuRunnerError::InvalidState(
                super::MMIO_DISPATCHER_POISONED_MESSAGE
            ))
        );
        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn dispatch_mmio_access_rejects_busy_dispatcher_lock_without_blocking() {
        let (runner, _) = start_dispatch_recording_runner(Ok(MmioDispatchOutcome::Write));
        let dispatcher = shared_dispatcher();

        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));
        let dispatcher_guard = dispatcher
            .lock()
            .expect("dispatcher lock should not be poisoned");
        assert_eq!(
            runner.dispatch_mmio_access(resolved_mmio_access(), Arc::clone(&dispatcher)),
            Err(HvfVcpuRunnerError::InvalidState(
                super::MMIO_DISPATCHER_BUSY_MESSAGE
            ))
        );
        drop(dispatcher_guard);
        assert_eq!(runner.run_once(), Ok(HvfVcpuExit::Canceled));

        runner.shutdown().expect("runner should shut down");
    }

    #[test]
    fn dispatch_mmio_access_after_shutdown_is_rejected() {
        let (runner, _) = start_dispatch_recording_runner(Ok(MmioDispatchOutcome::Write));

        runner.shutdown().expect("runner should shut down");
        assert_eq!(
            runner.dispatch_mmio_access(resolved_mmio_access(), shared_dispatcher()),
            Err(HvfVcpuRunnerError::InvalidState(
                super::RUNNER_SHUT_DOWN_MESSAGE
            ))
        );
    }

    #[test]
    fn dispatch_mmio_access_during_shutdown_is_rejected() {
        let (runner, _, destroyed_receiver) = start_fake_runner();
        let (command_sender, should_cancel) = runner
            .prepare_shutdown()
            .expect("first shutdown should be prepared");

        assert!(!should_cancel);
        assert_eq!(
            runner.dispatch_mmio_access(resolved_mmio_access(), shared_dispatcher()),
            Err(HvfVcpuRunnerError::InvalidState(
                super::RUNNER_SHUTTING_DOWN_MESSAGE
            ))
        );

        let thread = runner
            .take_thread()
            .expect("runner state should be lockable");
        let (response_sender, response_receiver) = mpsc::channel();
        command_sender
            .send(super::RunnerCommand::Shutdown { response_sender })
            .expect("shutdown command should be sent");
        assert_eq!(
            response_receiver
                .recv()
                .expect("shutdown response should be sent"),
            Ok(())
        );
        super::join_runner_thread(thread).expect("runner thread should join");
        runner.finish_shutdown();
        destroyed_receiver
            .recv()
            .expect("fake vCPU should be destroyed");
    }

    #[test]
    fn run_after_shutdown_reports_invalid_state() {
        let (runner, _, destroyed_receiver) = start_fake_runner();

        runner.shutdown().expect("runner should shut down");
        destroyed_receiver
            .recv()
            .expect("fake vCPU should be destroyed");

        assert_eq!(
            runner.run_once(),
            Err(HvfVcpuRunnerError::InvalidState(
                super::RUNNER_SHUT_DOWN_MESSAGE
            ))
        );
    }

    #[test]
    fn startup_error_is_returned_to_caller() {
        let result = spawn_runner_thread(|| {
            Err::<FakeVcpu, BackendError>(BackendError::InvalidState("fake startup failed"))
        });
        let Err(err) = result else {
            panic!("startup error should be returned");
        };

        assert_eq!(
            err,
            HvfVcpuRunnerError::Backend(BackendError::InvalidState("fake startup failed"))
        );
    }

    #[test]
    fn startup_panic_is_joined_and_returned_to_caller() {
        let result = spawn_runner_thread(|| -> Result<FakeVcpu, BackendError> {
            panic!("fake startup panic");
        });
        let Err(err) = result else {
            panic!("startup panic should be returned");
        };

        assert_eq!(err, HvfVcpuRunnerError::ThreadPanicked);
    }

    #[test]
    fn shutdown_reports_thread_panic_after_started_runner_exits() {
        let (runner, runner_unwind_receiver) = start_panic_notifying_runner(|| Ok(PanicOnRunVcpu));

        assert_eq!(
            runner.run_once(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        wait_for_panic_notifying_runner_unwind(runner_unwind_receiver);
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }
}
