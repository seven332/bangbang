use std::env;
use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use bangbang_session::elevated_probe::{ProbeMode, RuntimeWorkload};
use bangbang_session::macos::{set_cloexec, verify_peer, verify_peer_pid};
use bangbang_session::{ObjectIdentity, SessionId};
use signal_hook::SigId;
use signal_hook::consts::signal::{SIGINT, SIGTERM};

use super::code_sign::WorkerProfile;
use super::spawn::{
    DAEMON_ENV_KEY, DAEMON_ENV_VALUE, DAEMON_HANDOFF_FD, ELEVATED_DAEMON_ENV_KEY,
    ELEVATED_DAEMON_ENV_VALUE, OwnedWorker, spawn_elevated_daemon_suspended,
};
use crate::elevated_probe::{Config, DaemonBarrier, DaemonFault};
use crate::launch_policy::{LaunchRequest, LaunchTiming};
use crate::{BundleLayout, ElevatedDaemonStage, LauncherError};

const MAGIC: [u8; 4] = *b"BBD1";
const VERSION: u16 = 1;
const FRAME_BYTES: usize = 128;
const KIND_HELLO: u16 = 1;
const KIND_START: u16 = 2;
const KIND_TRANSITIONED: u16 = 3;
const KIND_TRANSITION_ACK: u16 = 4;
const KIND_READY: u16 = 5;
const KIND_ACK: u16 = 6;
const KIND_FAILED: u16 = 7;
const HANDOFF_TIMEOUT: Duration = Duration::from_secs(60);
const PARENT_POLL: Duration = Duration::from_millis(100);
const STATE_INITIAL_ROOT: u8 = 1;
const STATE_TARGET: u8 = 2;
const STATE_RETAINED_ROOT: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum FailurePhase {
    Config = 1,
    Transition = 2,
    TransitionSelf = 3,
    TransitionProcesses = 4,
    TransitionCredentials = 5,
    TransitionTopology = 6,
    TransitionCode = 7,
    TransitionStart = 8,
    TransitionProtocol = 9,
    TransitionAck = 10,
    SessionBind = 11,
    ReadySend = 12,
    ReadyAck = 13,
    TransitionParentPeer = 14,
    TransitionParentCode = 15,
    TransitionParentCredentials = 16,
    TransitionParentStart = 17,
    TransitionTransport = 18,
    RootAdoption = 19,
}

impl FailurePhase {
    fn from_byte(value: u8) -> Result<Self, LauncherError> {
        match value {
            1 => Ok(Self::Config),
            2 => Ok(Self::Transition),
            3 => Ok(Self::TransitionSelf),
            4 => Ok(Self::TransitionProcesses),
            5 => Ok(Self::TransitionCredentials),
            6 => Ok(Self::TransitionTopology),
            7 => Ok(Self::TransitionCode),
            8 => Ok(Self::TransitionStart),
            9 => Ok(Self::TransitionProtocol),
            10 => Ok(Self::TransitionAck),
            11 => Ok(Self::SessionBind),
            12 => Ok(Self::ReadySend),
            13 => Ok(Self::ReadyAck),
            14 => Ok(Self::TransitionParentPeer),
            15 => Ok(Self::TransitionParentCode),
            16 => Ok(Self::TransitionParentCredentials),
            17 => Ok(Self::TransitionParentStart),
            18 => Ok(Self::TransitionTransport),
            19 => Ok(Self::RootAdoption),
            _ => Err(LauncherError::DaemonHandoff),
        }
    }

    const fn stage(self) -> ElevatedDaemonStage {
        match self {
            Self::RootAdoption => ElevatedDaemonStage::RootAdoption,
            Self::Config => ElevatedDaemonStage::Config,
            Self::Transition => ElevatedDaemonStage::TransitionWitness,
            Self::TransitionSelf => ElevatedDaemonStage::TransitionSelf,
            Self::TransitionProcesses => ElevatedDaemonStage::TransitionProcesses,
            Self::TransitionCredentials => ElevatedDaemonStage::TransitionCredentials,
            Self::TransitionTopology => ElevatedDaemonStage::TransitionTopology,
            Self::TransitionCode => ElevatedDaemonStage::TransitionCode,
            Self::TransitionStart => ElevatedDaemonStage::TransitionStart,
            Self::TransitionProtocol => ElevatedDaemonStage::TransitionProtocol,
            Self::TransitionParentPeer => ElevatedDaemonStage::TransitionParentPeer,
            Self::TransitionParentCode => ElevatedDaemonStage::TransitionParentCode,
            Self::TransitionParentCredentials => ElevatedDaemonStage::TransitionParentCredentials,
            Self::TransitionParentStart => ElevatedDaemonStage::TransitionParentStart,
            Self::TransitionTransport => ElevatedDaemonStage::TransitionTransport,
            Self::TransitionAck => ElevatedDaemonStage::TransitionAck,
            Self::SessionBind => ElevatedDaemonStage::SessionBind,
            Self::ReadySend => ElevatedDaemonStage::ReadySend,
            Self::ReadyAck => ElevatedDaemonStage::ReadyAck,
        }
    }

    const fn error(self) -> LauncherError {
        LauncherError::ElevatedDaemonHandoff(self.stage())
    }
}

const ELEVATED_DAEMON_LAUNCHER_ARTIFACT: &str = "bangbang-elevated-daemon-launcher-v1-BBD1-exact-root-worker-supervisor-parent-transition-session-ready-parent-loss-exit-watch-parent-before-transition-ack-child-after-transition-ack-parent-after-ready-post-ack-watch-parent-after-ack-daemon-namespace-retirement";

#[derive(Clone, Copy, PartialEq, Eq)]
struct Frame {
    kind: u16,
    sequence: u64,
    correlation: SessionId,
    parent_pid: libc::pid_t,
    supervisor_pid: libc::pid_t,
    worker_pid: libc::pid_t,
    mode: u8,
    workload: u8,
    credential_state: u8,
    flags: u8,
    root: ObjectIdentity,
    session: SessionId,
    monotonic_us: u64,
    parent_cpu_us: u64,
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ElevatedDaemonFrame(<redacted>)")
    }
}

impl Frame {
    fn hello(parent_pid: libc::pid_t, supervisor_pid: libc::pid_t, root: ObjectIdentity) -> Self {
        Self {
            kind: KIND_HELLO,
            sequence: 0,
            correlation: SessionId::pre_session(),
            parent_pid,
            supervisor_pid,
            worker_pid: 0,
            mode: 0,
            workload: 0,
            credential_state: 0,
            flags: 0,
            root,
            session: SessionId::pre_session(),
            monotonic_us: 0,
            parent_cpu_us: 0,
        }
    }

    fn start(
        context: Context,
        monotonic_us: u64,
        parent_cpu_us: u64,
    ) -> Result<Self, LauncherError> {
        if monotonic_us == 0 {
            return Err(LauncherError::DaemonHandoff);
        }
        Ok(Self {
            kind: KIND_START,
            sequence: 0,
            correlation: context.correlation,
            parent_pid: context.parent_pid,
            supervisor_pid: context.supervisor_pid,
            worker_pid: 0,
            mode: context.mode,
            workload: context.workload,
            credential_state: STATE_INITIAL_ROOT,
            flags: 0,
            root: context.root,
            session: SessionId::pre_session(),
            monotonic_us,
            parent_cpu_us,
        })
    }

    fn transitioned(context: Context, worker_pid: libc::pid_t, state: u8) -> Self {
        Self {
            kind: KIND_TRANSITIONED,
            sequence: 1,
            correlation: context.correlation,
            parent_pid: context.parent_pid,
            supervisor_pid: context.supervisor_pid,
            worker_pid,
            mode: context.mode,
            workload: context.workload,
            credential_state: state,
            flags: 0,
            root: context.root,
            session: SessionId::pre_session(),
            monotonic_us: 0,
            parent_cpu_us: 0,
        }
    }

    fn transition_ack(context: Context, worker_pid: libc::pid_t, state: u8) -> Self {
        let mut frame = Self::transitioned(context, worker_pid, state);
        frame.kind = KIND_TRANSITION_ACK;
        frame
    }

    fn ready(context: Context, worker_pid: libc::pid_t, state: u8, session: SessionId) -> Self {
        let mut frame = Self::transitioned(context, worker_pid, state);
        frame.kind = KIND_READY;
        frame.sequence = 2;
        frame.session = session;
        frame
    }

    fn ack(context: Context, worker_pid: libc::pid_t, state: u8, session: SessionId) -> Self {
        let mut frame = Self::ready(context, worker_pid, state, session);
        frame.kind = KIND_ACK;
        frame
    }

    fn failed(
        context: Context,
        phase: FailurePhase,
        worker_pid: libc::pid_t,
        state: u8,
        session: SessionId,
    ) -> Self {
        let sequence = match phase {
            FailurePhase::Config | FailurePhase::RootAdoption => 0,
            FailurePhase::Transition
            | FailurePhase::TransitionSelf
            | FailurePhase::TransitionProcesses
            | FailurePhase::TransitionCredentials
            | FailurePhase::TransitionTopology
            | FailurePhase::TransitionCode
            | FailurePhase::TransitionStart
            | FailurePhase::TransitionProtocol
            | FailurePhase::TransitionParentPeer
            | FailurePhase::TransitionParentCode
            | FailurePhase::TransitionParentCredentials
            | FailurePhase::TransitionParentStart
            | FailurePhase::TransitionTransport
            | FailurePhase::TransitionAck => 1,
            FailurePhase::SessionBind | FailurePhase::ReadySend | FailurePhase::ReadyAck => 2,
        };
        Self {
            kind: KIND_FAILED,
            sequence,
            correlation: context.correlation,
            parent_pid: context.parent_pid,
            supervisor_pid: context.supervisor_pid,
            worker_pid,
            mode: context.mode,
            workload: context.workload,
            credential_state: state,
            flags: phase as u8,
            root: context.root,
            session,
            monotonic_us: 0,
            parent_cpu_us: 0,
        }
    }

    fn failure_phase(self) -> Result<FailurePhase, LauncherError> {
        if self.kind != KIND_FAILED {
            return Err(LauncherError::DaemonHandoff);
        }
        FailurePhase::from_byte(self.flags)
    }
}

#[derive(Clone, Copy)]
struct Context {
    correlation: SessionId,
    parent_pid: libc::pid_t,
    supervisor_pid: libc::pid_t,
    mode: u8,
    workload: u8,
    root: ObjectIdentity,
    parent_start: ProcessStart,
    supervisor_start: ProcessStart,
}

impl std::fmt::Debug for Context {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ElevatedDaemonContext(<redacted>)")
    }
}

pub(crate) struct ChildBootstrap {
    pub(crate) timing: LaunchTiming,
    pub(crate) root: OwnedFd,
    pub(crate) notifier: Notifier,
}

impl std::fmt::Debug for ChildBootstrap {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ElevatedDaemonChildBootstrap(<redacted>)")
    }
}

pub(crate) fn child_bootstrap() -> Result<Option<ChildBootstrap>, LauncherError> {
    let Some(feature_marker) = env::var_os(ELEVATED_DAEMON_ENV_KEY) else {
        return Ok(None);
    };
    let daemon_marker = env::var_os(DAEMON_ENV_KEY).ok_or(LauncherError::DaemonHandoff)?;
    // SAFETY: This is the first launcher-library boundary before any thread is created.
    unsafe {
        env::remove_var(ELEVATED_DAEMON_ENV_KEY);
        env::remove_var(DAEMON_ENV_KEY);
    }
    if feature_marker != ELEVATED_DAEMON_ENV_VALUE || daemon_marker != DAEMON_ENV_VALUE {
        return Err(LauncherError::DaemonHandoff);
    }
    set_cloexec(DAEMON_HANDOFF_FD).map_err(|_| LauncherError::DaemonHandoff)?;
    let root_fd = bangbang_session::elevated_probe::ROOT_FD;
    set_cloexec(root_fd).map_err(|_| LauncherError::DaemonHandoff)?;
    // SAFETY: The authenticated default-close spawn transfers each fixed descriptor once.
    let stream = unsafe { OwnedFd::from_raw_fd(DAEMON_HANDOFF_FD) };
    // SAFETY: The authenticated default-close spawn transfers each fixed descriptor once.
    let root = unsafe { OwnedFd::from_raw_fd(root_fd) };
    let mut stream = UnixStream::from(stream);
    // SAFETY: These process identity getters take no retained pointers.
    let (pid, parent, session, process_group) = unsafe {
        (
            libc::getpid(),
            libc::getppid(),
            libc::getsid(0),
            libc::getpgrp(),
        )
    };
    if pid <= 0 || parent <= 0 || session != pid || process_group != pid {
        return Err(LauncherError::DaemonHandoff);
    }
    verify_peer(stream.as_raw_fd(), parent).map_err(|_| LauncherError::InvalidDaemonIdentity)?;
    super::code_sign::validate_launcher_process(parent)?;
    let root_identity = descriptor_identity(root.as_raw_fd())?;
    let parent_start = process_info(parent)?.start;
    let supervisor_start = process_info(pid)?.start;
    let deadline = Instant::now()
        .checked_add(HANDOFF_TIMEOUT)
        .ok_or(LauncherError::DaemonHandoff)?;
    send_frame(&mut stream, Frame::hello(parent, pid, root_identity))?;
    let start = read_frame_until(&mut stream, deadline, || Ok(()))?;
    if start.kind != KIND_START
        || start.sequence != 0
        || start.parent_pid != parent
        || start.supervisor_pid != pid
        || start.worker_pid != 0
        || start.credential_state != STATE_INITIAL_ROOT
        || start.flags != 0
        || start.root != root_identity
        || start.session != SessionId::pre_session()
        || start.correlation.is_pre_session()
        || start.monotonic_us == 0
    {
        return Err(LauncherError::DaemonHandoff);
    }
    verify_peer(stream.as_raw_fd(), parent).map_err(|_| LauncherError::InvalidDaemonIdentity)?;
    let timing = LaunchTiming::from_daemon_handoff(start.monotonic_us, start.parent_cpu_us)?;
    Ok(Some(ChildBootstrap {
        timing,
        root,
        notifier: Notifier::new(
            stream,
            deadline,
            Context {
                correlation: start.correlation,
                parent_pid: parent,
                supervisor_pid: pid,
                mode: start.mode,
                workload: start.workload,
                root: root_identity,
                parent_start,
                supervisor_start,
            },
        ),
    }))
}

pub(crate) fn launch_parent(
    request: &LaunchRequest,
    timing: LaunchTiming,
    executable: &Path,
    layout: &BundleLayout,
    profile: &WorkerProfile,
    config: Config,
) -> Result<(), LauncherError> {
    std::hint::black_box(ELEVATED_DAEMON_LAUNCHER_ARTIFACT);
    request.validate(layout.worker_executable(), true)?;
    let mode = config.mode();
    let workload = mode
        .runtime_workload()
        .ok_or(LauncherError::InvalidLaunchPolicy)?;
    let target_uid = config.target_uid();
    let target_gid = config.target_gid();
    let root = config.root_identity();
    let barrier = config.daemon_barrier();
    let fault = config.daemon_fault();
    let args = config.daemon_args(request.raw_args())?;
    let signals = ParentSignals::install()?;
    // SAFETY: `getpid` has no pointer or ownership contract.
    let parent_pid = unsafe { libc::getpid() };
    let correlation = SessionId::generate().map_err(|_| LauncherError::DaemonHandoff)?;
    let (mut child, mut stream) =
        spawn_elevated_daemon_suspended(executable, args, config.root_fd())?;
    let parent_start = process_info(parent_pid)?.start;
    let supervisor_start = process_info(child.pid())?.start;
    let context = Context {
        correlation,
        parent_pid,
        supervisor_pid: child.pid(),
        mode: mode as u8,
        workload: workload_code(workload),
        root,
        parent_start,
        supervisor_start,
    };
    super::code_sign::validate_launcher_process(child.pid())?;
    child.resume().map_err(|_| LauncherError::DaemonHandoff)?;
    let deadline = Instant::now()
        .checked_add(HANDOFF_TIMEOUT)
        .ok_or(LauncherError::DaemonHandoff)?;
    let hello = read_parent_frame(&mut stream, &mut child, &signals, deadline)?;
    if hello != Frame::hello(parent_pid, child.pid(), root) {
        return Err(LauncherError::DaemonHandoff);
    }
    verify_peer(stream.as_raw_fd(), child.pid())
        .map_err(|_| LauncherError::InvalidDaemonIdentity)?;
    send_frame(
        &mut stream,
        Frame::start(
            context,
            timing.monotonic_us(),
            timing.elapsed_process_cpu_us()?,
        )?,
    )?;
    let transitioned = read_parent_frame(&mut stream, &mut child, &signals, deadline)?;
    if transitioned.kind == KIND_FAILED {
        let error = transitioned.failure_phase()?.error();
        wait_for_failed_child_exit(&mut child, &signals, deadline)?;
        return Err(error);
    }
    let expected_state = credential_state(mode)?;
    if transitioned.kind != KIND_TRANSITIONED
        || transitioned.sequence != 1
        || !matches_context(transitioned, context)
        || transitioned.worker_pid <= 0
        || transitioned.credential_state != expected_state
        || transitioned.flags != 0
        || transitioned.session != SessionId::pre_session()
        || transitioned.monotonic_us != 0
        || transitioned.parent_cpu_us != 0
    {
        return Err(LauncherError::DaemonHandoff);
    }
    let transitioned_witness = validate_transitioned_processes(
        parent_pid,
        child.pid(),
        transitioned.worker_pid,
        mode,
        target_uid,
        target_gid,
        profile,
    )?;
    if transitioned_witness.parent != context.parent_start
        || transitioned_witness.supervisor != context.supervisor_start
    {
        return Err(LauncherError::DaemonHandoff);
    }
    drop(config);
    bangbang_session::elevated_credential::transition_process(mode, target_uid, target_gid)
        .map_err(|_| LauncherError::DaemonHandoff)?;
    bangbang_session::elevated_credential::attest_current_process(mode, target_uid, target_gid)
        .map_err(|_| LauncherError::DaemonHandoff)?;
    if validate_process_credentials(parent_pid, mode, target_uid, target_gid)?.start
        != context.parent_start
    {
        return Err(LauncherError::DaemonHandoff);
    }
    if fault == DaemonFault::TransitionAck {
        return Err(LauncherError::ElevatedDaemonHandoff(
            ElevatedDaemonStage::TransitionAck,
        ));
    }
    if barrier == DaemonBarrier::ParentBeforeTransitionAck {
        stop_current()?;
        validate_parent_side(
            &mut child,
            &stream,
            &signals,
            deadline,
            LiveProcessSpec {
                parent_pid,
                worker_pid: transitioned.worker_pid,
                mode,
                target_uid,
                target_gid,
                profile,
                witness: transitioned_witness,
            },
        )?;
    }
    send_frame(
        &mut stream,
        Frame::transition_ack(context, transitioned.worker_pid, expected_state),
    )?;
    let ready = read_parent_frame(&mut stream, &mut child, &signals, deadline)?;
    if ready.kind == KIND_FAILED {
        let error = ready.failure_phase()?.error();
        wait_for_failed_child_exit(&mut child, &signals, deadline)?;
        return Err(error);
    }
    if ready.kind != KIND_READY
        || ready.sequence != 2
        || !matches_context(ready, context)
        || ready.worker_pid != transitioned.worker_pid
        || ready.credential_state != expected_state
        || ready.flags != 0
        || ready.session.is_pre_session()
        || ready.monotonic_us != 0
        || ready.parent_cpu_us != 0
    {
        return Err(LauncherError::DaemonHandoff);
    }
    if validate_transitioned_processes(
        parent_pid,
        child.pid(),
        transitioned.worker_pid,
        mode,
        target_uid,
        target_gid,
        profile,
    )? != transitioned_witness
    {
        return Err(LauncherError::DaemonHandoff);
    }
    if validate_process_credentials(parent_pid, mode, target_uid, target_gid)?.start
        != context.parent_start
    {
        return Err(LauncherError::DaemonHandoff);
    }
    if barrier == DaemonBarrier::ParentAfterReady {
        stop_current()?;
        validate_parent_side(
            &mut child,
            &stream,
            &signals,
            deadline,
            LiveProcessSpec {
                parent_pid,
                worker_pid: transitioned.worker_pid,
                mode,
                target_uid,
                target_gid,
                profile,
                witness: transitioned_witness,
            },
        )?;
    }
    if fault == DaemonFault::ReadyAck {
        return Err(LauncherError::ElevatedDaemonHandoff(
            ElevatedDaemonStage::ReadyAck,
        ));
    }
    send_frame(
        &mut stream,
        Frame::ack(
            context,
            transitioned.worker_pid,
            expected_state,
            ready.session,
        ),
    )?;
    if barrier == DaemonBarrier::ParentAfterAck {
        stop_current()?;
    }
    if child.try_wait()?.is_some() || signals.received() {
        return Err(LauncherError::DaemonHandoff);
    }
    let pid = child.pid();
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "bangbang daemon pid: {pid}")
        .and_then(|()| stdout.flush())
        .map_err(|_| LauncherError::DaemonHandoff)?;
    let released = child.release();
    debug_assert_eq!(released, pid);
    if barrier == DaemonBarrier::PostAckWatch {
        wait_for_post_ack_detach(&mut stream, &signals, deadline)?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    AwaitTransition,
    AwaitSession,
    AwaitReady,
    AwaitAck,
    Detached,
    ParentLost,
}

pub(crate) struct Notifier {
    stream: Option<UnixStream>,
    decoder: Decoder,
    deadline: Instant,
    context: Context,
    state: State,
    mode: Option<ProbeMode>,
    target_uid: u32,
    target_gid: u32,
    worker_pid: libc::pid_t,
    worker_profile: Option<WorkerProfile>,
    worker_witness: Option<LiveProcessWitness>,
    session: SessionId,
    barrier: DaemonBarrier,
    fault: DaemonFault,
    failure_phase: FailurePhase,
}

impl std::fmt::Debug for Notifier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ElevatedDaemonNotifier(<redacted>)")
    }
}

impl Notifier {
    fn new(stream: UnixStream, deadline: Instant, context: Context) -> Self {
        Self {
            stream: Some(stream),
            decoder: Decoder::default(),
            deadline,
            context,
            state: State::AwaitTransition,
            mode: None,
            target_uid: 0,
            target_gid: 0,
            worker_pid: 0,
            worker_profile: None,
            worker_witness: None,
            session: SessionId::pre_session(),
            barrier: DaemonBarrier::None,
            fault: DaemonFault::None,
            failure_phase: FailurePhase::Config,
        }
    }

    pub(crate) fn validate_config(&mut self, config: &Config) -> Result<(), LauncherError> {
        let workload = config
            .mode()
            .runtime_workload()
            .ok_or(LauncherError::InvalidLaunchPolicy)?;
        if self.state != State::AwaitTransition
            || self.context.mode != config.mode() as u8
            || self.context.workload != workload_code(workload)
            || self.context.root != config.root_identity()
        {
            return Err(LauncherError::DaemonHandoff);
        }
        self.mode = Some(config.mode());
        self.target_uid = config.target_uid();
        self.target_gid = config.target_gid();
        self.barrier = config.daemon_barrier();
        self.fault = config.daemon_fault();
        if self.fault == DaemonFault::RootAdoption {
            self.failure_phase = FailurePhase::RootAdoption;
            return Err(LauncherError::DaemonHandoff);
        }
        Ok(())
    }

    pub(crate) fn notify_transitioned(
        &mut self,
        worker_pid: libc::pid_t,
        profile: &WorkerProfile,
    ) -> Result<(), LauncherError> {
        let mode = self.mode.ok_or(LauncherError::DaemonHandoff)?;
        if self.state != State::AwaitTransition || worker_pid <= 0 {
            return Err(LauncherError::DaemonHandoff);
        }
        self.worker_pid = worker_pid;
        self.worker_profile = Some(profile.clone());
        if self.fault == DaemonFault::TransitionWitness {
            self.failure_phase = FailurePhase::Transition;
            return Err(LauncherError::DaemonHandoff);
        }
        self.failure_phase = FailurePhase::TransitionProtocol;
        if self.check_parent()? != super::daemon::NotifierEvent::Pending {
            return Err(LauncherError::DaemonHandoff);
        }
        self.failure_phase = FailurePhase::TransitionSelf;
        bangbang_session::elevated_credential::attest_current_process(
            mode,
            self.target_uid,
            self.target_gid,
        )
        .map_err(|_| LauncherError::DaemonHandoff)?;
        self.failure_phase = FailurePhase::TransitionProcesses;
        let records = observe_transitioned_children(
            self.context.parent_pid,
            self.context.parent_start,
            self.context.supervisor_pid,
            worker_pid,
        )?;
        self.failure_phase = FailurePhase::TransitionCredentials;
        validate_transitioned_credentials(
            &records,
            self.context.supervisor_pid,
            worker_pid,
            mode,
            self.target_uid,
            self.target_gid,
        )?;
        self.failure_phase = FailurePhase::TransitionTopology;
        validate_transitioned_topology(
            &records,
            self.context.parent_pid,
            self.context.supervisor_pid,
            worker_pid,
        )?;
        self.failure_phase = FailurePhase::TransitionCode;
        validate_transitioned_code(self.context.supervisor_pid, worker_pid, profile)?;
        let witness = transitioned_witness(&records);
        self.failure_phase = FailurePhase::TransitionStart;
        if witness.parent != self.context.parent_start
            || witness.supervisor != self.context.supervisor_start
        {
            return Err(LauncherError::DaemonHandoff);
        }
        self.failure_phase = FailurePhase::TransitionProtocol;
        let state = credential_state(mode)?;
        let stream = self.stream.as_mut().ok_or(LauncherError::DaemonHandoff)?;
        send_frame(stream, Frame::transitioned(self.context, worker_pid, state))?;
        let ack = read_frame_until(stream, self.deadline, || Ok(()))?;
        if ack != Frame::transition_ack(self.context, worker_pid, state) {
            return Err(LauncherError::DaemonHandoff);
        }
        self.failure_phase = FailurePhase::TransitionParentPeer;
        verify_peer_pid(stream.as_raw_fd(), self.context.parent_pid)
            .map_err(|_| LauncherError::InvalidDaemonIdentity)?;
        self.failure_phase = FailurePhase::TransitionParentCode;
        super::code_sign::validate_launcher_process(self.context.parent_pid)?;
        self.failure_phase = FailurePhase::TransitionParentCredentials;
        if validate_process_credentials(
            self.context.parent_pid,
            mode,
            self.target_uid,
            self.target_gid,
        )?
        .start
            != self.context.parent_start
        {
            self.failure_phase = FailurePhase::TransitionParentStart;
            return Err(LauncherError::DaemonHandoff);
        }
        self.failure_phase = FailurePhase::TransitionTransport;
        stream
            .set_nonblocking(true)
            .map_err(|_| LauncherError::DaemonHandoff)?;
        self.state = State::AwaitSession;
        self.failure_phase = FailurePhase::TransitionAck;
        self.worker_witness = Some(witness);
        if self.barrier == DaemonBarrier::ChildAfterTransitionAck {
            stop_current()?;
            if self.check_parent()? != super::daemon::NotifierEvent::Pending {
                return Err(LauncherError::DaemonHandoff);
            }
        }
        Ok(())
    }

    pub(crate) fn bind_session(&mut self, session: SessionId) -> Result<(), LauncherError> {
        if self.state != State::AwaitSession || session.is_pre_session() {
            return Err(LauncherError::DaemonHandoff);
        }
        if self.fault == DaemonFault::SessionBind {
            self.failure_phase = FailurePhase::SessionBind;
            return Err(LauncherError::DaemonHandoff);
        }
        self.session = session;
        self.state = State::AwaitReady;
        Ok(())
    }

    pub(crate) fn as_raw_fd(&self) -> Result<libc::c_int, LauncherError> {
        self.stream
            .as_ref()
            .map(AsRawFd::as_raw_fd)
            .ok_or(LauncherError::DaemonHandoff)
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        matches!(self.state, State::AwaitReady | State::AwaitAck).then_some(self.deadline)
    }

    pub(crate) fn is_awaiting_ready(&self) -> bool {
        self.state == State::AwaitReady
    }

    pub(crate) fn uses_namespace_retirement_barrier(&self) -> bool {
        self.barrier == DaemonBarrier::NamespaceRetirement
    }

    pub(crate) fn notify_ready(
        &mut self,
        supervisor_pid: libc::pid_t,
    ) -> Result<(), LauncherError> {
        let mode = self.mode.ok_or(LauncherError::DaemonHandoff)?;
        if self.state != State::AwaitReady
            || supervisor_pid != self.context.supervisor_pid
            || self.worker_pid <= 0
            || self.session.is_pre_session()
        {
            return Err(LauncherError::DaemonHandoff);
        }
        if self.check_parent()? != super::daemon::NotifierEvent::Pending {
            return Err(LauncherError::DaemonHandoff);
        }
        if self.fault == DaemonFault::ReadySend {
            return Err(LauncherError::DaemonHandoff);
        }
        if validate_transitioned_processes(
            self.context.parent_pid,
            supervisor_pid,
            self.worker_pid,
            mode,
            self.target_uid,
            self.target_gid,
            self.worker_profile
                .as_ref()
                .ok_or(LauncherError::DaemonHandoff)?,
        )? != self.worker_witness.ok_or(LauncherError::DaemonHandoff)?
        {
            return Err(LauncherError::DaemonHandoff);
        }
        if validate_process_credentials(
            self.context.parent_pid,
            mode,
            self.target_uid,
            self.target_gid,
        )?
        .start
            != self.context.parent_start
        {
            return Err(LauncherError::DaemonHandoff);
        }
        let state = credential_state(mode)?;
        send_frame(
            self.stream.as_mut().ok_or(LauncherError::DaemonHandoff)?,
            Frame::ready(self.context, self.worker_pid, state, self.session),
        )?;
        self.state = State::AwaitAck;
        Ok(())
    }

    pub(crate) fn check_parent(&mut self) -> Result<super::daemon::NotifierEvent, LauncherError> {
        let descriptor = self.as_raw_fd()?;
        let mut byte = 0_u8;
        // SAFETY: The descriptor is live and `byte` is writable for one non-consuming probe.
        let result = unsafe {
            libc::recv(
                descriptor,
                (&raw mut byte).cast(),
                1,
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        if result == 0 {
            self.state = State::ParentLost;
            return Ok(super::daemon::NotifierEvent::ParentLost);
        }
        if result > 0 {
            if self.state == State::AwaitReady {
                return Err(LauncherError::DaemonHandoff);
            }
            return self.drain();
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::WouldBlock {
            Ok(super::daemon::NotifierEvent::Pending)
        } else {
            Err(LauncherError::DaemonHandoff)
        }
    }

    pub(crate) fn drain(&mut self) -> Result<super::daemon::NotifierEvent, LauncherError> {
        if self.state == State::Detached {
            return Ok(super::daemon::NotifierEvent::Acknowledged);
        }
        if self.state == State::ParentLost {
            return Ok(super::daemon::NotifierEvent::ParentLost);
        }
        if self.state != State::AwaitAck {
            return Err(LauncherError::DaemonHandoff);
        }
        let mut buffer = [0_u8; FRAME_BYTES];
        loop {
            let read = self
                .stream
                .as_mut()
                .ok_or(LauncherError::DaemonHandoff)?
                .read(&mut buffer);
            match read {
                Ok(0) => {
                    self.state = State::ParentLost;
                    return Ok(super::daemon::NotifierEvent::ParentLost);
                }
                Ok(length) => {
                    self.decoder
                        .push(buffer.get(..length).ok_or(LauncherError::DaemonHandoff)?)?;
                    if let Some(frame) = self.decoder.take()? {
                        let mode = self.mode.ok_or(LauncherError::DaemonHandoff)?;
                        if frame
                            != Frame::ack(
                                self.context,
                                self.worker_pid,
                                credential_state(mode)?,
                                self.session,
                            )
                            || !self.decoder.is_empty()
                        {
                            return Err(LauncherError::DaemonHandoff);
                        }
                        verify_peer_pid(
                            self.stream
                                .as_ref()
                                .ok_or(LauncherError::DaemonHandoff)?
                                .as_raw_fd(),
                            self.context.parent_pid,
                        )
                        .map_err(|_| LauncherError::InvalidDaemonIdentity)?;
                        if self.barrier == DaemonBarrier::PostAckWatch {
                            let detached = validate_detached_processes(
                                self.context.supervisor_pid,
                                self.worker_pid,
                                mode,
                                self.target_uid,
                                self.target_gid,
                                self.worker_profile
                                    .as_ref()
                                    .ok_or(LauncherError::DaemonHandoff)?,
                            )?;
                            let expected =
                                self.worker_witness.ok_or(LauncherError::DaemonHandoff)?;
                            if detached.supervisor != expected.supervisor
                                || detached.worker != expected.worker
                            {
                                return Err(LauncherError::DaemonHandoff);
                            }
                            signal_exact(self.worker_pid, libc::SIGSTOP)?;
                            stop_current()?;
                        }
                        self.state = State::Detached;
                        if let Some(stream) = self.stream.as_ref() {
                            let _ = stream.shutdown(std::net::Shutdown::Both);
                        }
                        return Ok(super::daemon::NotifierEvent::Acknowledged);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    return Ok(super::daemon::NotifierEvent::Pending);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return Err(LauncherError::DaemonHandoff),
            }
        }
    }

    pub(crate) fn close_transport(&mut self) {
        if let Some(stream) = self.stream.take() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }

    pub(crate) fn notify_failure(&mut self, _error: LauncherError) {
        let values = match self.state {
            State::AwaitTransition if self.worker_pid == 0 => Some((
                self.failure_phase,
                0,
                STATE_INITIAL_ROOT,
                SessionId::pre_session(),
            )),
            State::AwaitTransition => self.mode.map(|mode| {
                (
                    self.failure_phase,
                    self.worker_pid,
                    credential_state(mode).unwrap_or(STATE_INITIAL_ROOT),
                    SessionId::pre_session(),
                )
            }),
            State::AwaitSession => self.mode.map(|mode| {
                (
                    self.failure_phase,
                    self.worker_pid,
                    credential_state(mode).unwrap_or(STATE_INITIAL_ROOT),
                    SessionId::pre_session(),
                )
            }),
            State::AwaitReady => self.mode.map(|mode| {
                (
                    if self.session.is_pre_session() {
                        FailurePhase::SessionBind
                    } else {
                        FailurePhase::ReadySend
                    },
                    self.worker_pid,
                    credential_state(mode).unwrap_or(STATE_INITIAL_ROOT),
                    self.session,
                )
            }),
            State::AwaitAck => self.mode.map(|mode| {
                (
                    FailurePhase::ReadyAck,
                    self.worker_pid,
                    credential_state(mode).unwrap_or(STATE_INITIAL_ROOT),
                    self.session,
                )
            }),
            State::Detached | State::ParentLost => None,
        };
        if let (Some((phase, worker_pid, state, session)), Some(stream)) =
            (values, self.stream.as_mut())
        {
            let _ = send_frame(
                stream,
                Frame::failed(self.context, phase, worker_pid, state, session),
            );
        }
        self.state = State::Detached;
        self.close_transport();
    }
}

impl super::daemon::SessionNotifier for Notifier {
    fn as_raw_fd(&self) -> Result<libc::c_int, LauncherError> {
        Self::as_raw_fd(self)
    }

    fn deadline(&self) -> Option<Instant> {
        Self::deadline(self)
    }

    fn is_awaiting_ready(&self) -> bool {
        Self::is_awaiting_ready(self)
    }

    fn notify_ready(&mut self, supervisor_pid: libc::pid_t) -> Result<(), LauncherError> {
        Self::notify_ready(self, supervisor_pid)
    }

    fn drain(&mut self) -> Result<super::daemon::NotifierEvent, LauncherError> {
        Self::drain(self)
    }

    fn close_transport(&mut self) {
        Self::close_transport(self);
    }
}

fn matches_context(frame: Frame, context: Context) -> bool {
    frame.correlation == context.correlation
        && frame.parent_pid == context.parent_pid
        && frame.supervisor_pid == context.supervisor_pid
        && frame.mode == context.mode
        && frame.workload == context.workload
        && frame.root == context.root
}

fn workload_code(workload: RuntimeWorkload) -> u8 {
    match workload {
        RuntimeWorkload::RepresentativeGrants => 1,
        RuntimeWorkload::GuestNoApi => 2,
        RuntimeWorkload::GuestApi => 3,
    }
}

fn credential_state(mode: ProbeMode) -> Result<u8, LauncherError> {
    if !mode.continues_runtime() {
        return Err(LauncherError::InvalidLaunchPolicy);
    }
    Ok(if mode.retains_root() {
        STATE_RETAINED_ROOT
    } else {
        STATE_TARGET
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ProcessStart {
    seconds: u64,
    microseconds: u64,
}

impl std::fmt::Debug for ProcessStart {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ElevatedDaemonProcessStart(<redacted>)")
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct LiveProcessWitness {
    parent: ProcessStart,
    supervisor: ProcessStart,
    worker: ProcessStart,
}

impl std::fmt::Debug for LiveProcessWitness {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ElevatedDaemonLiveProcessWitness(<redacted>)")
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct DetachedProcessWitness {
    supervisor: ProcessStart,
    worker: ProcessStart,
}

#[derive(Clone, Copy)]
struct ProcessInfo {
    pid: libc::pid_t,
    parent: libc::pid_t,
    process_group: libc::pid_t,
    session: libc::pid_t,
    uid: u32,
    gid: u32,
    real_uid: u32,
    real_gid: u32,
    saved_uid: u32,
    saved_gid: u32,
    start: ProcessStart,
}

impl std::fmt::Debug for ProcessInfo {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ElevatedDaemonProcessInfo(<redacted>)")
    }
}

fn process_info(pid: libc::pid_t) -> Result<ProcessInfo, LauncherError> {
    if pid <= 0 {
        return Err(LauncherError::DaemonHandoff);
    }
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>())
        .map_err(|_| LauncherError::DaemonHandoff)?;
    // SAFETY: `info` is writable for exactly `size`; fixed flavor returns this structure.
    let result = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    validate_process_info_result(result, size)?;
    // SAFETY: Exact successful size initialized the complete structure.
    let info = unsafe { info.assume_init() };
    // SAFETY: `pid` is positive and names the exact live process just queried.
    let session = unsafe { libc::getsid(pid) };
    process_info_from_bsd(pid, session, info)
}

fn validate_process_info_result(
    result: libc::c_int,
    expected: libc::c_int,
) -> Result<(), LauncherError> {
    (expected > 0 && result == expected)
        .then_some(())
        .ok_or(LauncherError::DaemonHandoff)
}

fn process_info_from_bsd(
    pid: libc::pid_t,
    session: libc::pid_t,
    info: libc::proc_bsdinfo,
) -> Result<ProcessInfo, LauncherError> {
    let actual_pid = i32::try_from(info.pbi_pid).map_err(|_| LauncherError::DaemonHandoff)?;
    let parent = i32::try_from(info.pbi_ppid).map_err(|_| LauncherError::DaemonHandoff)?;
    let process_group = i32::try_from(info.pbi_pgid).map_err(|_| LauncherError::DaemonHandoff)?;
    if actual_pid != pid
        || parent <= 0
        || process_group <= 0
        || session <= 0
        || info.pbi_start_tvsec == 0
        || info.pbi_start_tvusec >= 1_000_000
    {
        return Err(LauncherError::DaemonHandoff);
    }
    Ok(ProcessInfo {
        pid,
        parent,
        process_group,
        session,
        uid: info.pbi_uid,
        gid: info.pbi_gid,
        real_uid: info.pbi_ruid,
        real_gid: info.pbi_rgid,
        saved_uid: info.pbi_svuid,
        saved_gid: info.pbi_svgid,
        start: ProcessStart {
            seconds: info.pbi_start_tvsec,
            microseconds: info.pbi_start_tvusec,
        },
    })
}

fn validate_process_credentials(
    pid: libc::pid_t,
    mode: ProbeMode,
    target_uid: u32,
    target_gid: u32,
) -> Result<ProcessInfo, LauncherError> {
    let info = process_info(pid)?;
    validate_process_record_credentials(&info, pid, mode, target_uid, target_gid)?;
    Ok(info)
}

fn validate_process_record_credentials(
    info: &ProcessInfo,
    pid: libc::pid_t,
    mode: ProbeMode,
    target_uid: u32,
    target_gid: u32,
) -> Result<(), LauncherError> {
    let expected = if mode.retains_root() {
        (0, 0)
    } else {
        (target_uid, target_gid)
    };
    if info.pid != pid
        || (info.uid, info.gid) != expected
        || (info.real_uid, info.real_gid) != expected
        || (info.saved_uid, info.saved_gid) != expected
    {
        return Err(LauncherError::DaemonHandoff);
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct TransitionedProcessRecords {
    parent_pid: libc::pid_t,
    parent_start: ProcessStart,
    supervisor: ProcessInfo,
    worker: ProcessInfo,
}

fn observe_transitioned_processes(
    parent_pid: libc::pid_t,
    supervisor_pid: libc::pid_t,
    worker_pid: libc::pid_t,
) -> Result<TransitionedProcessRecords, LauncherError> {
    let parent = process_info(parent_pid)?;
    Ok(TransitionedProcessRecords {
        parent_pid: parent.pid,
        parent_start: parent.start,
        supervisor: process_info(supervisor_pid)?,
        worker: process_info(worker_pid)?,
    })
}

fn observe_transitioned_children(
    parent_pid: libc::pid_t,
    parent_start: ProcessStart,
    supervisor_pid: libc::pid_t,
    worker_pid: libc::pid_t,
) -> Result<TransitionedProcessRecords, LauncherError> {
    Ok(TransitionedProcessRecords {
        parent_pid,
        parent_start,
        supervisor: process_info(supervisor_pid)?,
        worker: process_info(worker_pid)?,
    })
}

fn validate_transitioned_credentials(
    records: &TransitionedProcessRecords,
    supervisor_pid: libc::pid_t,
    worker_pid: libc::pid_t,
    mode: ProbeMode,
    target_uid: u32,
    target_gid: u32,
) -> Result<(), LauncherError> {
    validate_process_record_credentials(
        &records.supervisor,
        supervisor_pid,
        mode,
        target_uid,
        target_gid,
    )?;
    validate_process_record_credentials(&records.worker, worker_pid, mode, target_uid, target_gid)
}

fn validate_transitioned_topology(
    records: &TransitionedProcessRecords,
    parent_pid: libc::pid_t,
    supervisor_pid: libc::pid_t,
    worker_pid: libc::pid_t,
) -> Result<(), LauncherError> {
    let supervisor = records.supervisor;
    let worker = records.worker;
    if records.parent_pid != parent_pid
        || supervisor.pid != supervisor_pid
        || supervisor.parent != parent_pid
        || supervisor.process_group != supervisor_pid
        || supervisor.session != supervisor_pid
        || worker.pid != worker_pid
        || worker.parent != supervisor_pid
        || worker.process_group != supervisor_pid
        || worker.session != supervisor_pid
    {
        return Err(LauncherError::DaemonHandoff);
    }
    Ok(())
}

fn validate_transitioned_code(
    supervisor_pid: libc::pid_t,
    worker_pid: libc::pid_t,
    profile: &WorkerProfile,
) -> Result<(), LauncherError> {
    super::code_sign::validate_launcher_process(supervisor_pid)?;
    if super::code_sign::validate_worker_process(worker_pid)? != *profile {
        return Err(LauncherError::InvalidWorkerIdentity);
    }
    Ok(())
}

const fn transitioned_witness(records: &TransitionedProcessRecords) -> LiveProcessWitness {
    LiveProcessWitness {
        parent: records.parent_start,
        supervisor: records.supervisor.start,
        worker: records.worker.start,
    }
}

fn validate_transitioned_processes(
    parent_pid: libc::pid_t,
    supervisor_pid: libc::pid_t,
    worker_pid: libc::pid_t,
    mode: ProbeMode,
    target_uid: u32,
    target_gid: u32,
    profile: &WorkerProfile,
) -> Result<LiveProcessWitness, LauncherError> {
    let records = observe_transitioned_processes(parent_pid, supervisor_pid, worker_pid)?;
    validate_transitioned_credentials(
        &records,
        supervisor_pid,
        worker_pid,
        mode,
        target_uid,
        target_gid,
    )?;
    validate_transitioned_topology(&records, parent_pid, supervisor_pid, worker_pid)?;
    validate_transitioned_code(supervisor_pid, worker_pid, profile)?;
    Ok(transitioned_witness(&records))
}

fn validate_detached_processes(
    supervisor_pid: libc::pid_t,
    worker_pid: libc::pid_t,
    mode: ProbeMode,
    target_uid: u32,
    target_gid: u32,
    profile: &WorkerProfile,
) -> Result<DetachedProcessWitness, LauncherError> {
    let supervisor = validate_process_credentials(supervisor_pid, mode, target_uid, target_gid)?;
    let worker = validate_process_credentials(worker_pid, mode, target_uid, target_gid)?;
    if supervisor.pid != supervisor_pid
        || supervisor.process_group != supervisor_pid
        || supervisor.session != supervisor_pid
        || worker.pid != worker_pid
        || worker.parent != supervisor_pid
        || worker.process_group != supervisor_pid
        || worker.session != supervisor_pid
    {
        return Err(LauncherError::DaemonHandoff);
    }
    super::code_sign::validate_launcher_process(supervisor_pid)?;
    if super::code_sign::validate_worker_process(worker_pid)? != *profile {
        return Err(LauncherError::InvalidWorkerIdentity);
    }
    Ok(DetachedProcessWitness {
        supervisor: supervisor.start,
        worker: worker.start,
    })
}

#[derive(Clone, Copy)]
struct LiveProcessSpec<'a> {
    parent_pid: libc::pid_t,
    worker_pid: libc::pid_t,
    mode: ProbeMode,
    target_uid: u32,
    target_gid: u32,
    profile: &'a WorkerProfile,
    witness: LiveProcessWitness,
}

fn validate_parent_side(
    child: &mut OwnedWorker,
    stream: &UnixStream,
    signals: &ParentSignals,
    deadline: Instant,
    spec: LiveProcessSpec<'_>,
) -> Result<(), LauncherError> {
    if Instant::now() >= deadline || signals.received() || child.try_wait()?.is_some() {
        return Err(LauncherError::DaemonHandoff);
    }
    verify_peer_pid(stream.as_raw_fd(), child.pid())
        .map_err(|_| LauncherError::InvalidDaemonIdentity)?;
    validate_process_credentials(spec.parent_pid, spec.mode, spec.target_uid, spec.target_gid)?;
    if validate_transitioned_processes(
        spec.parent_pid,
        child.pid(),
        spec.worker_pid,
        spec.mode,
        spec.target_uid,
        spec.target_gid,
        spec.profile,
    )? == spec.witness
    {
        Ok(())
    } else {
        Err(LauncherError::DaemonHandoff)
    }
}

fn signal_exact(pid: libc::pid_t, signal: libc::c_int) -> Result<(), LauncherError> {
    if pid <= 0 {
        return Err(LauncherError::DaemonHandoff);
    }
    // SAFETY: The PID was independently validated immediately before this fixed signal.
    if unsafe { libc::kill(pid, signal) } == 0 {
        Ok(())
    } else {
        Err(LauncherError::DaemonHandoff)
    }
}

fn stop_current() -> Result<(), LauncherError> {
    // SAFETY: SIGSTOP has fixed kernel semantics and takes no pointer. Callers
    // invoke this only at committed feature protocol boundaries without locks.
    if unsafe { libc::raise(libc::SIGSTOP) } == 0 {
        Ok(())
    } else {
        Err(LauncherError::DaemonHandoff)
    }
}

fn descriptor_identity(fd: libc::c_int) -> Result<ObjectIdentity, LauncherError> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `stat` is writable and `fd` is the inherited live root descriptor.
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
        return Err(LauncherError::DaemonHandoff);
    }
    // SAFETY: Successful `fstat` initialized the complete structure.
    let stat = unsafe { stat.assume_init() };
    let identity = ObjectIdentity {
        device: u64::try_from(stat.st_dev).map_err(|_| LauncherError::DaemonHandoff)?,
        inode: stat.st_ino,
    };
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR || identity.device == 0 || identity.inode == 0 {
        return Err(LauncherError::DaemonHandoff);
    }
    Ok(identity)
}

fn read_parent_frame(
    stream: &mut UnixStream,
    child: &mut OwnedWorker,
    signals: &ParentSignals,
    deadline: Instant,
) -> Result<Frame, LauncherError> {
    read_frame_until(stream, deadline, || {
        if signals.received() {
            Err(LauncherError::DaemonHandoff)
        } else {
            let _ = child.try_wait()?;
            Ok(())
        }
    })
}

fn wait_for_failed_child_exit(
    child: &mut OwnedWorker,
    signals: &ParentSignals,
    deadline: Instant,
) -> Result<(), LauncherError> {
    loop {
        if let Some(status) = child.try_wait()? {
            return (!status.success())
                .then_some(())
                .ok_or(LauncherError::DaemonHandoff);
        }
        if signals.received() {
            return Err(LauncherError::DaemonHandoff);
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or(LauncherError::DaemonHandoff)?;
        std::thread::sleep(remaining.min(PARENT_POLL));
    }
}

fn wait_for_post_ack_detach(
    stream: &mut UnixStream,
    signals: &ParentSignals,
    deadline: Instant,
) -> Result<(), LauncherError> {
    let mut byte = 0_u8;
    loop {
        if signals.received() {
            return Err(LauncherError::DaemonHandoff);
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or(LauncherError::DaemonHandoff)?;
        stream
            .set_read_timeout(Some(remaining.min(PARENT_POLL)))
            .map_err(|_| LauncherError::DaemonHandoff)?;
        match stream.read(std::slice::from_mut(&mut byte)) {
            Ok(0) => return Ok(()),
            Ok(_) => return Err(LauncherError::DaemonHandoff),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return Err(LauncherError::DaemonHandoff),
        }
    }
}

fn read_frame_until(
    stream: &mut UnixStream,
    deadline: Instant,
    mut check: impl FnMut() -> Result<(), LauncherError>,
) -> Result<Frame, LauncherError> {
    let mut decoder = Decoder::default();
    let mut buffer = [0_u8; FRAME_BYTES];
    loop {
        check()?;
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or(LauncherError::DaemonHandoff)?;
        stream
            .set_read_timeout(Some(remaining.min(PARENT_POLL)))
            .map_err(|_| LauncherError::DaemonHandoff)?;
        match stream.read(&mut buffer) {
            Ok(0) => return Err(LauncherError::DaemonHandoff),
            Ok(length) => {
                decoder.push(buffer.get(..length).ok_or(LauncherError::DaemonHandoff)?)?;
                if let Some(frame) = decoder.take()? {
                    if !decoder.is_empty() {
                        return Err(LauncherError::DaemonHandoff);
                    }
                    return Ok(frame);
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return Err(LauncherError::DaemonHandoff),
        }
    }
}

fn send_frame(stream: &mut UnixStream, frame: Frame) -> Result<(), LauncherError> {
    stream
        .set_write_timeout(Some(PARENT_POLL))
        .map_err(|_| LauncherError::DaemonHandoff)?;
    stream
        .write_all(&encode_frame(frame)?)
        .map_err(|_| LauncherError::DaemonHandoff)
}

fn encode_frame(frame: Frame) -> Result<[u8; FRAME_BYTES], LauncherError> {
    validate_frame(frame)?;
    let mut bytes = [0_u8; FRAME_BYTES];
    bytes[0..4].copy_from_slice(&MAGIC);
    bytes[4..6].copy_from_slice(&VERSION.to_be_bytes());
    bytes[6..8].copy_from_slice(&frame.kind.to_be_bytes());
    bytes[8..16].copy_from_slice(&frame.sequence.to_be_bytes());
    bytes[16..48].copy_from_slice(frame.correlation.as_bytes());
    write_pid(&mut bytes[48..52], frame.parent_pid)?;
    write_pid(&mut bytes[52..56], frame.supervisor_pid)?;
    write_pid_allow_zero(&mut bytes[56..60], frame.worker_pid)?;
    bytes[60] = frame.mode;
    bytes[61] = frame.workload;
    bytes[62] = frame.credential_state;
    bytes[63] = frame.flags;
    bytes[64..72].copy_from_slice(&frame.root.device.to_be_bytes());
    bytes[72..80].copy_from_slice(&frame.root.inode.to_be_bytes());
    bytes[80..112].copy_from_slice(frame.session.as_bytes());
    bytes[112..120].copy_from_slice(&frame.monotonic_us.to_be_bytes());
    bytes[120..128].copy_from_slice(&frame.parent_cpu_us.to_be_bytes());
    Ok(bytes)
}

fn decode_frame(bytes: &[u8]) -> Result<Frame, LauncherError> {
    if bytes.len() != FRAME_BYTES
        || bytes.get(0..4) != Some(MAGIC.as_slice())
        || read_u16(bytes, 4)? != VERSION
    {
        return Err(LauncherError::DaemonHandoff);
    }
    let frame = Frame {
        kind: read_u16(bytes, 6)?,
        sequence: read_u64(bytes, 8)?,
        correlation: SessionId::from_bytes(read_array(bytes, 16)?),
        parent_pid: read_pid(bytes, 48, false)?,
        supervisor_pid: read_pid(bytes, 52, false)?,
        worker_pid: read_pid(bytes, 56, true)?,
        mode: *bytes.get(60).ok_or(LauncherError::DaemonHandoff)?,
        workload: *bytes.get(61).ok_or(LauncherError::DaemonHandoff)?,
        credential_state: *bytes.get(62).ok_or(LauncherError::DaemonHandoff)?,
        flags: *bytes.get(63).ok_or(LauncherError::DaemonHandoff)?,
        root: ObjectIdentity {
            device: read_u64(bytes, 64)?,
            inode: read_u64(bytes, 72)?,
        },
        session: SessionId::from_bytes(read_array(bytes, 80)?),
        monotonic_us: read_u64(bytes, 112)?,
        parent_cpu_us: read_u64(bytes, 120)?,
    };
    validate_frame(frame)?;
    if encode_frame(frame)? != bytes {
        return Err(LauncherError::DaemonHandoff);
    }
    Ok(frame)
}

fn validate_frame(frame: Frame) -> Result<(), LauncherError> {
    if frame.parent_pid <= 0
        || frame.supervisor_pid <= 0
        || frame.root.device == 0
        || frame.root.inode == 0
    {
        return Err(LauncherError::DaemonHandoff);
    }
    let zero_correlation = frame.correlation.is_pre_session();
    let zero_session = frame.session.is_pre_session();
    let initial = frame.worker_pid == 0
        && frame.mode == 0
        && frame.workload == 0
        && frame.credential_state == 0
        && zero_session
        && frame.monotonic_us == 0
        && frame.parent_cpu_us == 0;
    let started = frame.worker_pid == 0
        && valid_mode_workload(frame.mode, frame.workload)
        && frame.credential_state == STATE_INITIAL_ROOT
        && zero_session
        && frame.monotonic_us > 0;
    let transitioned = frame.worker_pid > 0
        && valid_mode_workload(frame.mode, frame.workload)
        && matches!(frame.credential_state, STATE_TARGET | STATE_RETAINED_ROOT)
        && zero_session
        && frame.monotonic_us == 0
        && frame.parent_cpu_us == 0;
    let ready = frame.worker_pid > 0
        && valid_mode_workload(frame.mode, frame.workload)
        && matches!(frame.credential_state, STATE_TARGET | STATE_RETAINED_ROOT)
        && !zero_session
        && frame.monotonic_us == 0
        && frame.parent_cpu_us == 0;
    let failed = if frame.kind == KIND_FAILED {
        match FailurePhase::from_byte(frame.flags)? {
            FailurePhase::Config | FailurePhase::RootAdoption => {
                frame.sequence == 0
                    && !zero_correlation
                    && frame.worker_pid == 0
                    && valid_mode_workload(frame.mode, frame.workload)
                    && frame.credential_state == STATE_INITIAL_ROOT
                    && zero_session
                    && frame.monotonic_us == 0
                    && frame.parent_cpu_us == 0
            }
            FailurePhase::Transition
            | FailurePhase::TransitionSelf
            | FailurePhase::TransitionProcesses
            | FailurePhase::TransitionCredentials
            | FailurePhase::TransitionTopology
            | FailurePhase::TransitionCode
            | FailurePhase::TransitionStart
            | FailurePhase::TransitionProtocol
            | FailurePhase::TransitionParentPeer
            | FailurePhase::TransitionParentCode
            | FailurePhase::TransitionParentCredentials
            | FailurePhase::TransitionParentStart
            | FailurePhase::TransitionTransport
            | FailurePhase::TransitionAck => {
                frame.sequence == 1 && !zero_correlation && transitioned
            }
            FailurePhase::SessionBind => frame.sequence == 2 && !zero_correlation && transitioned,
            FailurePhase::ReadySend | FailurePhase::ReadyAck => {
                frame.sequence == 2 && !zero_correlation && ready
            }
        }
    } else {
        false
    };
    let valid = match frame.kind {
        KIND_HELLO => frame.flags == 0 && frame.sequence == 0 && zero_correlation && initial,
        KIND_START => frame.flags == 0 && frame.sequence == 0 && !zero_correlation && started,
        KIND_TRANSITIONED | KIND_TRANSITION_ACK => {
            frame.flags == 0 && frame.sequence == 1 && !zero_correlation && transitioned
        }
        KIND_READY | KIND_ACK => {
            frame.flags == 0 && frame.sequence == 2 && !zero_correlation && ready
        }
        KIND_FAILED => failed,
        _ => false,
    };
    valid.then_some(()).ok_or(LauncherError::DaemonHandoff)
}

fn valid_mode_workload(mode: u8, workload: u8) -> bool {
    matches!((mode, workload), (11..=13, 1) | (14..=16, 2) | (17..=19, 3))
}

fn write_pid(bytes: &mut [u8], pid: libc::pid_t) -> Result<(), LauncherError> {
    if pid <= 0 {
        return Err(LauncherError::DaemonHandoff);
    }
    write_pid_allow_zero(bytes, pid)
}

fn write_pid_allow_zero(bytes: &mut [u8], pid: libc::pid_t) -> Result<(), LauncherError> {
    let pid = u32::try_from(pid)
        .ok()
        .filter(|pid| *pid <= i32::MAX as u32)
        .ok_or(LauncherError::DaemonHandoff)?;
    let output = bytes.get_mut(..4).ok_or(LauncherError::DaemonHandoff)?;
    output.copy_from_slice(&pid.to_be_bytes());
    Ok(())
}

fn read_pid(bytes: &[u8], offset: usize, allow_zero: bool) -> Result<libc::pid_t, LauncherError> {
    let pid = i32::try_from(read_u32(bytes, offset)?).map_err(|_| LauncherError::DaemonHandoff)?;
    if pid < 0 || (!allow_zero && pid == 0) {
        return Err(LauncherError::DaemonHandoff);
    }
    Ok(pid)
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, LauncherError> {
    Ok(u16::from_be_bytes(read_array(bytes, offset)?))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, LauncherError> {
    Ok(u32::from_be_bytes(read_array(bytes, offset)?))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, LauncherError> {
    Ok(u64::from_be_bytes(read_array(bytes, offset)?))
}

fn read_array<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u8; N], LauncherError> {
    bytes
        .get(offset..offset + N)
        .ok_or(LauncherError::DaemonHandoff)?
        .try_into()
        .map_err(|_| LauncherError::DaemonHandoff)
}

#[derive(Default)]
struct Decoder {
    bytes: Vec<u8>,
}

impl Decoder {
    fn push(&mut self, bytes: &[u8]) -> Result<(), LauncherError> {
        if self.bytes.len().saturating_add(bytes.len()) > FRAME_BYTES {
            return Err(LauncherError::DaemonHandoff);
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn take(&mut self) -> Result<Option<Frame>, LauncherError> {
        if self.bytes.len() < FRAME_BYTES {
            return Ok(None);
        }
        let frame = decode_frame(&self.bytes)?;
        self.bytes.clear();
        Ok(Some(frame))
    }

    fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

struct ParentSignals {
    received: Arc<AtomicBool>,
    registrations: [SigId; 2],
}

impl ParentSignals {
    fn install() -> Result<Self, LauncherError> {
        let received = Arc::new(AtomicBool::new(false));
        let interrupt = signal_hook::flag::register(SIGINT, Arc::clone(&received))
            .map_err(|_| LauncherError::DaemonHandoff)?;
        let terminate = match signal_hook::flag::register(SIGTERM, Arc::clone(&received)) {
            Ok(registration) => registration,
            Err(_) => {
                signal_hook::low_level::unregister(interrupt);
                return Err(LauncherError::DaemonHandoff);
            }
        };
        Ok(Self {
            received,
            registrations: [interrupt, terminate],
        })
    }

    fn received(&self) -> bool {
        self.received.load(Ordering::Acquire)
    }
}

impl Drop for ParentSignals {
    fn drop(&mut self) {
        for registration in self.registrations {
            signal_hook::low_level::unregister(registration);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> Context {
        Context {
            correlation: SessionId::from_bytes([0x41; 32]),
            parent_pid: 41,
            supervisor_pid: 42,
            mode: ProbeMode::GuestApiDrop as u8,
            workload: workload_code(RuntimeWorkload::GuestApi),
            root: ObjectIdentity {
                device: 43,
                inode: 44,
            },
            parent_start: ProcessStart {
                seconds: 45,
                microseconds: 46,
            },
            supervisor_start: ProcessStart {
                seconds: 47,
                microseconds: 48,
            },
        }
    }

    fn process_record(
        pid: libc::pid_t,
        parent: libc::pid_t,
        process_group: libc::pid_t,
        session: libc::pid_t,
        uid: u32,
        gid: u32,
        start: u64,
    ) -> ProcessInfo {
        ProcessInfo {
            pid,
            parent,
            process_group,
            session,
            uid,
            gid,
            real_uid: uid,
            real_gid: gid,
            saved_uid: uid,
            saved_gid: gid,
            start: ProcessStart {
                seconds: start,
                microseconds: 123,
            },
        }
    }

    fn process_records(uid: u32, gid: u32) -> TransitionedProcessRecords {
        TransitionedProcessRecords {
            parent_pid: 41,
            parent_start: ProcessStart {
                seconds: 101,
                microseconds: 123,
            },
            supervisor: process_record(42, 41, 42, 42, uid, gid, 102),
            worker: process_record(43, 42, 42, 42, uid, gid, 103),
        }
    }

    #[test]
    fn every_closed_frame_round_trips_and_redacts() {
        let context = context();
        let session = SessionId::from_bytes([0x42; 32]);
        let frames = [
            Frame::hello(41, 42, context.root),
            Frame::start(context, 45, 46).expect("start should construct"),
            Frame::transitioned(context, 47, STATE_TARGET),
            Frame::transition_ack(context, 47, STATE_TARGET),
            Frame::ready(context, 47, STATE_TARGET, session),
            Frame::ack(context, 47, STATE_TARGET, session),
            Frame::failed(
                context,
                FailurePhase::Config,
                0,
                STATE_INITIAL_ROOT,
                SessionId::pre_session(),
            ),
            Frame::failed(
                context,
                FailurePhase::Transition,
                47,
                STATE_TARGET,
                SessionId::pre_session(),
            ),
            Frame::failed(
                context,
                FailurePhase::TransitionAck,
                47,
                STATE_TARGET,
                SessionId::pre_session(),
            ),
            Frame::failed(
                context,
                FailurePhase::SessionBind,
                47,
                STATE_TARGET,
                SessionId::pre_session(),
            ),
            Frame::failed(context, FailurePhase::ReadySend, 47, STATE_TARGET, session),
            Frame::failed(context, FailurePhase::ReadyAck, 47, STATE_TARGET, session),
        ];
        for frame in frames {
            let encoded = encode_frame(frame).expect("frame should encode");
            assert_eq!(decode_frame(&encoded), Ok(frame));
            assert_eq!(format!("{frame:?}"), "ElevatedDaemonFrame(<redacted>)");
            if frame.kind == KIND_FAILED {
                assert_eq!(frame.failure_phase(), FailurePhase::from_byte(frame.flags));
            }
        }
    }

    #[test]
    fn decoder_accepts_every_split_and_rejects_coalescing() {
        let frame = Frame::transitioned(context(), 47, STATE_TARGET);
        let encoded = encode_frame(frame).expect("frame should encode");
        for split in 0..=encoded.len() {
            let mut decoder = Decoder::default();
            decoder.push(&encoded[..split]).expect("prefix should fit");
            assert_eq!(
                decoder.take().expect("prefix should decode"),
                (split == encoded.len()).then_some(frame)
            );
            if split != encoded.len() {
                decoder.push(&encoded[split..]).expect("suffix should fit");
                assert_eq!(decoder.take().expect("frame should decode"), Some(frame));
            }
        }
        let mut decoder = Decoder::default();
        let mut doubled = encoded.to_vec();
        doubled.extend_from_slice(&encoded);
        assert_eq!(decoder.push(&doubled), Err(LauncherError::DaemonHandoff));
    }

    #[test]
    fn rejects_wrong_magic_reserved_fields_and_session_phase() {
        let frame = Frame::transitioned(context(), 47, STATE_TARGET);
        for (offset, replacement) in [(0, 0_u8), (63, 1_u8), (80, 1_u8)] {
            let mut encoded = encode_frame(frame).expect("frame should encode");
            encoded[offset] = replacement;
            assert_eq!(decode_frame(&encoded), Err(LauncherError::DaemonHandoff));
        }
        let mut failed = encode_frame(Frame::failed(
            context(),
            FailurePhase::Config,
            0,
            STATE_INITIAL_ROOT,
            SessionId::pre_session(),
        ))
        .expect("failure should encode");
        failed[63] = 0;
        assert_eq!(decode_frame(&failed), Err(LauncherError::DaemonHandoff));
        assert_eq!(decode_frame(b"BBH1"), Err(LauncherError::DaemonHandoff));
    }

    #[test]
    fn every_failure_phase_round_trips_to_one_value_free_stage() {
        let phases = [
            FailurePhase::Config,
            FailurePhase::Transition,
            FailurePhase::TransitionSelf,
            FailurePhase::TransitionProcesses,
            FailurePhase::TransitionCredentials,
            FailurePhase::TransitionTopology,
            FailurePhase::TransitionCode,
            FailurePhase::TransitionStart,
            FailurePhase::TransitionProtocol,
            FailurePhase::TransitionAck,
            FailurePhase::SessionBind,
            FailurePhase::ReadySend,
            FailurePhase::ReadyAck,
            FailurePhase::TransitionParentPeer,
            FailurePhase::TransitionParentCode,
            FailurePhase::TransitionParentCredentials,
            FailurePhase::TransitionParentStart,
            FailurePhase::TransitionTransport,
            FailurePhase::RootAdoption,
        ];
        for phase in phases {
            assert_eq!(FailurePhase::from_byte(phase as u8), Ok(phase));
            let name = phase.stage().name();
            assert!(!name.is_empty());
            assert!(
                name.bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'-')
            );
            let (worker, state, session) = match phase {
                FailurePhase::Config | FailurePhase::RootAdoption => {
                    (0, STATE_INITIAL_ROOT, SessionId::pre_session())
                }
                FailurePhase::Transition
                | FailurePhase::TransitionSelf
                | FailurePhase::TransitionProcesses
                | FailurePhase::TransitionCredentials
                | FailurePhase::TransitionTopology
                | FailurePhase::TransitionCode
                | FailurePhase::TransitionStart
                | FailurePhase::TransitionProtocol
                | FailurePhase::TransitionAck
                | FailurePhase::TransitionParentPeer
                | FailurePhase::TransitionParentCode
                | FailurePhase::TransitionParentCredentials
                | FailurePhase::TransitionParentStart
                | FailurePhase::TransitionTransport
                | FailurePhase::SessionBind => (43, STATE_TARGET, SessionId::pre_session()),
                FailurePhase::ReadySend | FailurePhase::ReadyAck => {
                    (43, STATE_TARGET, SessionId::from_bytes([0x42; 32]))
                }
            };
            let frame = Frame::failed(context(), phase, worker, state, session);
            let encoded = encode_frame(frame).expect("failure frame should encode");
            assert_eq!(decode_frame(&encoded), Ok(frame));
            assert_eq!(frame.failure_phase(), Ok(phase));
        }
        assert_eq!(
            FailurePhase::from_byte(0),
            Err(LauncherError::DaemonHandoff)
        );
        assert_eq!(
            FailurePhase::from_byte(20),
            Err(LauncherError::DaemonHandoff)
        );
    }

    #[test]
    fn process_records_accept_exact_mapped_unmapped_and_retained_root_ids() {
        for (mode, uid, gid) in [
            (ProbeMode::GuestNoApiDrop, 501, 20),
            (ProbeMode::GuestNoApiUnmapped, u32::MAX / 2, u32::MAX / 2),
            (ProbeMode::GuestNoApiRetainRoot, 0, 0),
        ] {
            let records = process_records(uid, gid);
            validate_transitioned_credentials(&records, 42, 43, mode, uid, gid)
                .expect("exact credential records should pass");
            validate_transitioned_topology(&records, 41, 42, 43)
                .expect("exact daemon topology should pass");
            assert_eq!(
                transitioned_witness(&records),
                LiveProcessWitness {
                    parent: records.parent_start,
                    supervisor: records.supervisor.start,
                    worker: records.worker.start,
                }
            );
        }
    }

    #[test]
    fn process_records_reject_every_credential_and_topology_mismatch() {
        let exact = process_records(501, 20);
        let mut credential_mutations = Vec::new();
        for field in 0..6 {
            let mut records = exact;
            let record = if field < 3 {
                &mut records.supervisor
            } else {
                &mut records.worker
            };
            match field % 3 {
                0 => record.uid = 0,
                1 => record.real_gid = 0,
                _ => record.saved_uid = 0,
            }
            credential_mutations.push(records);
        }
        for records in credential_mutations {
            assert_eq!(
                validate_transitioned_credentials(
                    &records,
                    42,
                    43,
                    ProbeMode::GuestNoApiDrop,
                    501,
                    20,
                ),
                Err(LauncherError::DaemonHandoff)
            );
        }

        let mut topology_mutations = Vec::new();
        for field in 0..9 {
            let mut records = exact;
            match field {
                0 => records.parent_pid = 99,
                1 => records.supervisor.pid = 99,
                2 => records.supervisor.parent = 99,
                3 => records.supervisor.process_group = 99,
                4 => records.supervisor.session = 99,
                5 => records.worker.pid = 99,
                6 => records.worker.parent = 99,
                7 => records.worker.process_group = 99,
                _ => records.worker.session = 99,
            }
            topology_mutations.push(records);
        }
        for records in topology_mutations {
            assert_eq!(
                validate_transitioned_topology(&records, 41, 42, 43),
                Err(LauncherError::DaemonHandoff)
            );
        }
    }

    #[test]
    fn process_info_boundary_rejects_partial_missing_and_malformed_records() {
        let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>())
            .expect("process info size should fit");
        assert_eq!(validate_process_info_result(size, size), Ok(()));
        for result in [-1, 0, size - 1, size + 1] {
            assert_eq!(
                validate_process_info_result(result, size),
                Err(LauncherError::DaemonHandoff)
            );
        }

        // SAFETY: A zeroed C record is valid to inspect as bytes and is rejected
        // before it could be treated as a live process identity.
        let zeroed = unsafe { MaybeUninit::<libc::proc_bsdinfo>::zeroed().assume_init() };
        assert!(matches!(
            process_info_from_bsd(42, 42, zeroed),
            Err(LauncherError::DaemonHandoff)
        ));
        assert_eq!(
            format!("{:?}", process_records(501, 20).worker),
            "ElevatedDaemonProcessInfo(<redacted>)"
        );
    }
}
