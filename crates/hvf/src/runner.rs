use std::fmt;
use std::marker::PhantomData;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};

use bangbang_runtime::BackendError;

use crate::backend::HvfBackend;
use crate::exit::HvfVcpuExit;
use crate::vcpu::HvfVcpuOwner;

const RUNNER_SHUT_DOWN_MESSAGE: &str = "vCPU runner is shut down";
const RUNNER_SHUTTING_DOWN_MESSAGE: &str = "vCPU runner shutdown is already in progress";
const RUN_IN_FLIGHT_MESSAGE: &str = "vCPU runner already has a run in flight";
const RUNNER_STATE_POISONED_MESSAGE: &str = "vCPU runner state lock is poisoned";
const COMMAND_CHANNEL_CLOSED_MESSAGE: &str = "vCPU runner command channel is closed";
const RESPONSE_CHANNEL_CLOSED_MESSAGE: &str = "vCPU runner response channel is closed";

type CancelVcpu = Arc<dyn Fn(crate::ffi::HvVcpu) -> Result<(), BackendError> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfVcpuRunnerError {
    Backend(BackendError),
    InvalidState(&'static str),
    ThreadSpawn(String),
    ChannelClosed(&'static str),
    ThreadPanicked,
}

impl fmt::Display for HvfVcpuRunnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(err) => write!(f, "{err}"),
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
}

enum RunnerCommand {
    RunOnce {
        response_sender: mpsc::Sender<Result<HvfVcpuExit, HvfVcpuRunnerError>>,
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
    fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError>;
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

    fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
        self.owner.run_once()
    }

    fn destroy(&mut self) -> Result<(), BackendError> {
        self.owner.destroy()
    }
}

impl<'vm> HvfVcpuRunner<'vm> {
    pub(crate) fn new(_: &'vm HvfBackend) -> Result<Self, HvfVcpuRunnerError> {
        Self::from_started(
            spawn_runner_thread(RealRunnerVcpu::create)?,
            real_cancel_vcpu(),
        )
    }

    pub fn run_once(&self) -> Result<HvfVcpuExit, HvfVcpuRunnerError> {
        let (response_sender, response_receiver) = mpsc::channel();
        let _in_flight_run = self.start_run_once(response_sender)?;

        response_receiver
            .recv()
            .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    }

    pub fn cancel(&self) -> Result<(), HvfVcpuRunnerError> {
        self.prepare_cancel()?;
        self.cancel_vcpu()
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

        first_error(response_result, join_result)
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
            }),
            _vm: PhantomData,
        })
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

        Ok(InFlightRun::new(&self.state))
    }

    fn prepare_shutdown(&self) -> Result<(mpsc::Sender<RunnerCommand>, bool), HvfVcpuRunnerError> {
        let mut state = self.lock_state()?;
        if state.thread.is_none() {
            return Err(HvfVcpuRunnerError::InvalidState(RUNNER_SHUT_DOWN_MESSAGE));
        }
        if state.shutting_down {
            return Err(HvfVcpuRunnerError::InvalidState(
                RUNNER_SHUTTING_DOWN_MESSAGE,
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

    fn prepare_cancel(&self) -> Result<(), HvfVcpuRunnerError> {
        let state = self.lock_state()?;
        if state.thread.is_none() {
            return Err(HvfVcpuRunnerError::InvalidState(RUNNER_SHUT_DOWN_MESSAGE));
        }
        if state.shutting_down {
            return Err(HvfVcpuRunnerError::InvalidState(
                RUNNER_SHUTTING_DOWN_MESSAGE,
            ));
        }
        Ok(())
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
            )
        });

        match state {
            Ok((active, shutting_down, in_flight_runs)) => f
                .debug_struct("HvfVcpuRunner")
                .field("active", &active)
                .field("shutting_down", &shutting_down)
                .field("in_flight_runs", &in_flight_runs)
                .finish_non_exhaustive(),
            Err(_) => f
                .debug_struct("HvfVcpuRunner")
                .field("state", &RUNNER_STATE_POISONED_MESSAGE)
                .finish_non_exhaustive(),
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

fn real_cancel_vcpu() -> CancelVcpu {
    Arc::new(|vcpu| {
        let mut vcpus = [vcpu];
        crate::ffi::exit_vcpus(&mut vcpus)
    })
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

    match startup_receiver
        .recv()
        .map_err(|_| HvfVcpuRunnerError::ChannelClosed(RESPONSE_CHANNEL_CLOSED_MESSAGE))?
    {
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
            RunnerCommand::RunOnce { response_sender } => {
                let result = vcpu.run_once().map_err(HvfVcpuRunnerError::Backend);
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

fn join_runner_thread(thread: Option<JoinHandle<()>>) -> Result<(), HvfVcpuRunnerError> {
    if let Some(thread) = thread {
        thread
            .join()
            .map_err(|_| HvfVcpuRunnerError::ThreadPanicked)?;
    }

    Ok(())
}

fn first_error(
    first: Result<(), HvfVcpuRunnerError>,
    second: Result<(), HvfVcpuRunnerError>,
) -> Result<(), HvfVcpuRunnerError> {
    match first {
        Ok(()) => second,
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, mpsc};
    use std::thread;

    use bangbang_runtime::BackendError;

    use super::{CancelVcpu, HvfVcpuRunner, HvfVcpuRunnerError, RunnerVcpu, spawn_runner_thread};
    use crate::exit::HvfVcpuExit;

    struct FakeVcpu {
        entered_run_sender: mpsc::Sender<()>,
        release_run_receiver: mpsc::Receiver<Result<HvfVcpuExit, BackendError>>,
        destroyed_sender: mpsc::Sender<()>,
    }

    impl RunnerVcpu for FakeVcpu {
        fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
            Ok(7)
        }

        fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
            self.entered_run_sender
                .send(())
                .map_err(|_| BackendError::InvalidState("fake run entry receiver closed"))?;
            self.release_run_receiver
                .recv()
                .map_err(|_| BackendError::InvalidState("fake run release sender closed"))?
        }

        fn destroy(&mut self) -> Result<(), BackendError> {
            self.destroyed_sender
                .send(())
                .map_err(|_| BackendError::InvalidState("fake destroy receiver closed"))
        }
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

        let (response_sender, response_receiver) = mpsc::channel();
        command_sender
            .send(super::RunnerCommand::Shutdown { response_sender })
            .expect("shutdown command should be sent");
        let thread = runner
            .take_thread()
            .expect("runner state should be lockable");
        let response = response_receiver
            .recv()
            .expect("shutdown response should be sent");

        assert_eq!(response, Ok(()));
        super::join_runner_thread(thread).expect("runner thread should join");
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
}
