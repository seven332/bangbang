use std::fmt;
use std::marker::PhantomData;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::thread::{self, JoinHandle};

use bangbang_runtime::BackendError;
use bangbang_runtime::mmio::{MmioDispatchOutcome, MmioDispatcher};

use crate::backend::HvfBackend;
use crate::exit::{HvfResolvedMmioAccess, HvfVcpuExit, HvfVcpuExitResolveError};
use crate::mmio::HvfMmioDispatchError;
use crate::vcpu::{HvfArm64BootRegisters, HvfSystemRegister, HvfVcpuOwner};

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
const RUNNER_STATE_POISONED_MESSAGE: &str = "vCPU runner state lock is poisoned";
const MMIO_DISPATCHER_BUSY_MESSAGE: &str = "vCPU runner MMIO dispatcher lock is busy";
const MMIO_DISPATCHER_POISONED_MESSAGE: &str = "vCPU runner MMIO dispatcher lock is poisoned";
const COMMAND_CHANNEL_CLOSED_MESSAGE: &str = "vCPU runner command channel is closed";
const RESPONSE_CHANNEL_CLOSED_MESSAGE: &str = "vCPU runner response channel is closed";

type CancelVcpu = Arc<dyn Fn(crate::ffi::HvVcpu) -> Result<(), BackendError> + Send + Sync>;
type SharedMmioDispatcher = Arc<Mutex<MmioDispatcher>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfVcpuRunnerError {
    Backend(BackendError),
    VcpuExitResolve(HvfVcpuExitResolveError),
    MmioDispatch(HvfMmioDispatchError),
    InvalidState(&'static str),
    ThreadSpawn(String),
    ChannelClosed(&'static str),
    ThreadPanicked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfVcpuRunStepOutcome {
    Canceled,
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
            Self::VcpuExitResolve(err) => write!(f, "{err}"),
            Self::MmioDispatch(err) => write!(f, "{err}"),
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
            Self::VcpuExitResolve(err) => Some(err),
            Self::MmioDispatch(err) => Some(err),
            Self::InvalidState(_)
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
    state: Mutex<RunnerHandleState>,
    _vm: PhantomData<&'vm HvfBackend>,
}

#[derive(Debug)]
struct RunnerHandleState {
    thread: Option<JoinHandle<()>>,
    shutting_down: bool,
    in_flight_runs: usize,
    mmio_dispatch_in_flight: bool,
    boot_register_setup_in_flight: bool,
    metadata_read_in_flight: bool,
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
    fn mpidr_el1(&mut self) -> Result<u64, BackendError> {
        Err(BackendError::InvalidState(
            "vCPU does not support MPIDR_EL1 reads",
        ))
    }
    fn destroy(&mut self) -> Result<(), BackendError>;
}

struct RealRunnerVcpu {
    owner: HvfVcpuOwner,
}

impl RealRunnerVcpu {
    fn create() -> Result<Self, BackendError> {
        Ok(Self {
            owner: HvfVcpuOwner::new()?,
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

    fn mpidr_el1(&mut self) -> Result<u64, BackendError> {
        self.owner.get_system_register(HvfSystemRegister::MPIDR_EL1)
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
                HvfVcpuRunnerError::InvalidState(_)
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
        // Keep the state lock until the HVF exit request returns so shutdown
        // cannot destroy the vCPU while cancellation uses its raw id.
        let _state_guard = self.prepare_cancel()?;
        self.cancel_vcpu()
    }

    /// Read the primary vCPU MPIDR on the vCPU-owning runner thread.
    pub fn mpidr_el1(&self) -> Result<u64, HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        let _in_flight_read = self.start_mpidr_el1_read(response_sender)?;

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
            state: Mutex::new(RunnerHandleState {
                thread: Some(started.thread),
                shutting_down: false,
                in_flight_runs: 0,
                mmio_dispatch_in_flight: false,
                boot_register_setup_in_flight: false,
                metadata_read_in_flight: false,
                boot_register_setup_failed: false,
                boot_registers_configured: false,
                run_started: false,
            }),
            _vm: PhantomData,
        })
    }

    fn start_arm64_boot_register_setup(
        &self,
        registers: HvfArm64BootRegisters,
        response_sender: mpsc::Sender<Result<(), HvfVcpuRunnerError>>,
    ) -> Result<InFlightBootRegisterSetup<'_>, HvfVcpuRunnerError> {
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
    ) -> Result<InFlightRun<'_>, HvfVcpuRunnerError> {
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
    ) -> Result<InFlightRun<'_>, HvfVcpuRunnerError> {
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
    ) -> Result<InFlightMmioDispatch<'_>, HvfVcpuRunnerError> {
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
    ) -> Result<InFlightMetadataRead<'_>, HvfVcpuRunnerError> {
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

    fn prepare_cancel(&self) -> Result<MutexGuard<'_, RunnerHandleState>, HvfVcpuRunnerError> {
        let state = self.lock_state()?;
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
        Ok(state)
    }

    fn cancel_vcpu(&self) -> Result<(), HvfVcpuRunnerError> {
        (self.cancel_vcpu)(self.vcpu).map_err(HvfVcpuRunnerError::Backend)
    }

    fn take_thread(&self) -> Result<Option<JoinHandle<()>>, HvfVcpuRunnerError> {
        Ok(self.lock_state()?.thread.take())
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, RunnerHandleState>, HvfVcpuRunnerError> {
        self.state
            .lock()
            .map_err(|_| HvfVcpuRunnerError::InvalidState(RUNNER_STATE_POISONED_MESSAGE))
    }
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

struct InFlightBootRegisterSetup<'state> {
    state: &'state Mutex<RunnerHandleState>,
    configured: bool,
    failed: bool,
}

impl<'state> InFlightBootRegisterSetup<'state> {
    fn new(state: &'state Mutex<RunnerHandleState>) -> Self {
        Self {
            state,
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

impl Drop for InFlightBootRegisterSetup<'_> {
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

struct InFlightRun<'state> {
    state: &'state Mutex<RunnerHandleState>,
}

impl<'state> InFlightRun<'state> {
    fn new(state: &'state Mutex<RunnerHandleState>) -> Self {
        Self { state }
    }
}

impl Drop for InFlightRun<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.in_flight_runs = state.in_flight_runs.saturating_sub(1);
        }
    }
}

struct InFlightMmioDispatch<'state> {
    state: &'state Mutex<RunnerHandleState>,
}

impl<'state> InFlightMmioDispatch<'state> {
    fn new(state: &'state Mutex<RunnerHandleState>) -> Self {
        Self { state }
    }
}

impl Drop for InFlightMmioDispatch<'_> {
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

struct InFlightMetadataRead<'state> {
    state: &'state Mutex<RunnerHandleState>,
}

impl<'state> InFlightMetadataRead<'state> {
    fn new(state: &'state Mutex<RunnerHandleState>) -> Self {
        Self { state }
    }
}

impl Drop for InFlightMetadataRead<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.metadata_read_in_flight = false;
        }
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
            let access = exit
                .decode_mmio_access()
                .map_err(|source| HvfVcpuExitResolveError::MmioDecode { exit, source })?;
            let mut dispatcher = lock_shared_mmio_dispatcher(dispatcher)?;
            let access = access
                .resolve(dispatcher.bus())
                .map_err(|source| HvfVcpuExitResolveError::MmioResolve { source })?;
            let outcome = vcpu.dispatch_mmio_access(access, &mut dispatcher)?;

            Ok(HvfVcpuRunStepOutcome::Mmio { access, outcome })
        }
    }
}

fn dispatch_mmio_access_on_runner_thread(
    vcpu: &mut impl RunnerVcpu,
    access: HvfResolvedMmioAccess,
    dispatcher: &SharedMmioDispatcher,
) -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
    let mut dispatcher = lock_shared_mmio_dispatcher(dispatcher)?;

    vcpu.dispatch_mmio_access(access, &mut dispatcher)
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
        HvfExceptionExit, HvfMmioAccessSize, HvfMmioDirection, HvfMmioRegister,
        HvfResolvedMmioAccess, HvfResolvedVcpuExit, HvfVcpuExit, HvfVcpuExitResolveError,
    };
    use crate::mmio::{HvfMmioCompletionError, HvfMmioDispatchError};
    use crate::vcpu::HvfArm64BootRegisters;

    const ESR_EC_DATA_ABORT_LOWER_EL: u64 = 0x24;
    const ESR_EC_SHIFT: u64 = 26;
    const ESR_ISS_ISV: u64 = 1 << 24;
    const ESR_ISS_SAS_SHIFT: u64 = 22;
    const ESR_ISS_SRT_SHIFT: u64 = 16;
    const ESR_ISS_WNR: u64 = 1 << 6;
    const ESR_ISS_SF: u64 = 1 << 15;

    struct FakeVcpu {
        entered_run_sender: mpsc::Sender<()>,
        release_run_receiver: mpsc::Receiver<Result<HvfVcpuExit, BackendError>>,
        destroyed_sender: mpsc::Sender<()>,
    }

    struct PanicOnRunVcpu;

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

    struct MmioDispatchRecordingVcpu {
        dispatched_sender: mpsc::Sender<HvfResolvedMmioAccess>,
        result: Result<MmioDispatchOutcome, HvfVcpuRunnerError>,
    }

    struct BlockingMmioDispatchVcpu {
        entered_dispatch_sender: mpsc::Sender<()>,
        release_dispatch_receiver: mpsc::Receiver<Result<MmioDispatchOutcome, HvfVcpuRunnerError>>,
    }

    struct RunStepRecordingVcpu {
        run_once_result: Result<HvfVcpuExit, BackendError>,
        dispatched_sender: Option<mpsc::Sender<HvfResolvedMmioAccess>>,
        dispatch_result: Result<MmioDispatchOutcome, HvfVcpuRunnerError>,
    }

    fn unsupported_mmio_dispatch() -> Result<MmioDispatchOutcome, HvfVcpuRunnerError> {
        Err(HvfVcpuRunnerError::InvalidState(
            "fake vCPU does not support MMIO dispatch",
        ))
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

    fn shared_dispatcher() -> Arc<Mutex<MmioDispatcher>> {
        Arc::new(Mutex::new(MmioDispatcher::new()))
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
    ) {
        let (dispatched_sender, dispatched_receiver) = mpsc::channel();
        let started = spawn_runner_thread(move || {
            Ok(RunStepRecordingVcpu {
                run_once_result,
                dispatched_sender: Some(dispatched_sender),
                dispatch_result,
            })
        })
        .expect("fake runner should start");

        (
            HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
                .expect("runner should be created"),
            dispatched_receiver,
        )
    }

    fn start_run_step_exit_runner(
        run_once_result: Result<HvfVcpuExit, BackendError>,
    ) -> HvfVcpuRunner<'static> {
        let started = spawn_runner_thread(move || {
            Ok(RunStepRecordingVcpu {
                run_once_result,
                dispatched_sender: None,
                dispatch_result: Ok(MmioDispatchOutcome::Write),
            })
        })
        .expect("fake runner should start");

        HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
            .expect("runner should be created")
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
    fn commands_during_mpidr_read_are_rejected_without_queueing() {
        let (runner, entered_read_receiver, release_read_sender) = start_blocking_mpidr_runner();

        thread::scope(|scope| {
            let read = scope.spawn(|| runner.mpidr_el1());
            entered_read_receiver
                .recv()
                .expect("runner should enter fake MPIDR read");

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
        let started =
            spawn_runner_thread(|| Ok(PanicOnConfigureVcpu)).expect("panic runner should start");
        let runner = HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
            .expect("runner should be created");

        assert_eq!(
            runner.configure_arm64_boot_registers(boot_registers()),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
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
            let cancel = scope.spawn(|| runner.cancel());
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
        let (runner, dispatched_receiver) = start_run_step_recording_runner(
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
        let (runner, dispatched_receiver) = start_run_step_recording_runner(
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
        let started =
            spawn_runner_thread(|| Ok(PanicOnRunVcpu)).expect("panic runner should start");
        let runner = HvfVcpuRunner::from_started(started, Arc::new(|_| Ok(())))
            .expect("runner should be created");

        assert_eq!(
            runner.run_once(),
            Err(HvfVcpuRunnerError::ChannelClosed(
                super::RESPONSE_CHANNEL_CLOSED_MESSAGE
            ))
        );
        assert_eq!(runner.shutdown(), Err(HvfVcpuRunnerError::ThreadPanicked));
    }
}
