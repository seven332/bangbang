use std::ffi::{CString, OsStr, OsString, c_char};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::ptr;
use std::time::{Duration, Instant};

use bangbang_session::SessionId;
use bangbang_session::credential::CredentialTarget;
use bangbang_session::vmnet_topology::{
    VMNET_TOPOLOGY_ENV_KEY, VMNET_TOPOLOGY_ENV_VALUE, VMNET_TOPOLOGY_FD,
    VMNET_TOPOLOGY_PROVIDER_FD, VmnetTopologyContext, VmnetTopologyMessage, VmnetTopologyMode,
    VmnetTopologyTerminal, VmnetTopologyTransport, VmnetTopologyTransportError,
};

use crate::BrokerBootstrap;
use crate::broker::BrokerError;
use crate::topology::{BootstrapRequest, parse_private_transition_args};

use super::broker_service;
use super::process::{
    OwnedChild, PinnedExecutable, spawn_daemon_broker, spawn_launcher_transition,
    validate_child_credentials,
};
use super::{
    BOOTSTRAP_FD, VMNET_DAEMON_ENV_KEY, VMNET_DAEMON_ENV_VALUE, adopt_connected_stream,
    require_exact_root,
};

const PROVIDER_EXECUTABLE_NAME: &str = "bangbang-vmnet-provider";
const OUTER_BUNDLE_NAME: &str = "Bangbang.app";
const LAUNCHER_EXECUTABLE_NAME: &str = "bangbang";
const WORKER_BUNDLE_NAME: &str = "BangbangWorker.app";
const WORKER_EXECUTABLE_NAME: &str = "bangbang-worker";
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const TERMINATION_TIMEOUT: Duration = Duration::from_secs(6);
const TOPOLOGY_IO_TIMEOUT: Duration = Duration::from_secs(60);

pub fn run_public_bootstrap(args: Vec<OsString>) -> Result<u8, BrokerError> {
    require_exact_root()?;
    let request = BootstrapRequest::parse(args)?;
    let provider = PinnedExecutable::current()?;
    let layout = ProviderProductLayout::from_provider_executable(current_executable_path()?)?;
    let outer = PinnedExecutable::at(&layout.launcher)?;
    let _worker = PinnedExecutable::at(&layout.worker)?;
    provider.revalidate_path()?;

    match request.mode() {
        VmnetTopologyMode::Foreground => {
            let spawned = spawn_launcher_transition(&provider, request.launcher_args())?;
            let correlation =
                SessionId::generate().map_err(|_| BrokerError::InvalidConfiguration)?;
            run_spawned_topology(
                spawned,
                request.target(),
                request.mode(),
                outer,
                correlation,
                None,
            )
        }
        VmnetTopologyMode::Daemon => run_public_daemon(request, provider, outer),
    }
}

fn run_public_daemon(
    request: BootstrapRequest,
    provider: PinnedExecutable,
    outer: PinnedExecutable,
) -> Result<u8, BrokerError> {
    let spawned = spawn_daemon_broker(&provider, request.launcher_args())?;
    let super::process::SpawnedDaemonBroker { mut child, handoff } = spawned;
    let correlation = SessionId::generate().map_err(|_| BrokerError::InvalidConfiguration)?;
    let broker_pid = u32::try_from(child.pid()).map_err(|_| BrokerError::Process)?;
    let handoff_context = VmnetTopologyContext::new(
        correlation,
        request.target(),
        broker_pid,
        VmnetTopologyMode::Daemon,
    )
    .map_err(|_| BrokerError::InvalidConfiguration)?;
    let mut handoff =
        VmnetTopologyTransport::new(handoff, TOPOLOGY_IO_TIMEOUT).map_err(map_topology_error)?;
    let result = (|| -> Result<(u32, SessionId), BrokerError> {
        handoff
            .send(VmnetTopologyMessage::Start(handoff_context))
            .map_err(map_topology_error)?;
        expect(
            handoff.receive().map_err(map_topology_error)?,
            VmnetTopologyMessage::Dropped(handoff_context),
        )?;
        provider.validate_child(child.pid())?;
        validate_child_credentials(
            child.pid(),
            CredentialTarget::new(0, 0).map_err(|_| BrokerError::Authority)?,
        )?;
        handoff
            .send(VmnetTopologyMessage::DropAck(handoff_context))
            .map_err(map_topology_error)?;
        let deadline = Instant::now()
            .checked_add(TOPOLOGY_IO_TIMEOUT)
            .ok_or(BrokerError::Timeout)?;
        loop {
            if child.try_wait()?.is_some() {
                return Err(BrokerError::Process);
            }
            if poll_readable(handoff.as_raw_fd(), POLL_INTERVAL)? {
                let (context, session) = match handoff.receive().map_err(map_topology_error)? {
                    VmnetTopologyMessage::LauncherReady { context, session } => (context, session),
                    _ => return Err(BrokerError::Protocol),
                };
                if context.correlation() != correlation
                    || context.target() != request.target()
                    || context.mode() != VmnetTopologyMode::Daemon
                    || context.launcher_pid() == broker_pid
                    || session.is_pre_session()
                {
                    return Err(BrokerError::Protocol);
                }
                let launcher_pid =
                    i32::try_from(context.launcher_pid()).map_err(|_| BrokerError::Process)?;
                outer.validate_child(launcher_pid)?;
                validate_child_credentials(launcher_pid, request.target())?;
                handoff
                    .send(VmnetTopologyMessage::ReadyAck { context, session })
                    .map_err(map_topology_error)?;
                expect(
                    handoff.receive().map_err(map_topology_error)?,
                    VmnetTopologyMessage::BrokerReady { context, session },
                )?;
                return Ok((context.launcher_pid(), session));
            }
            if Instant::now() >= deadline {
                return Err(BrokerError::Timeout);
            }
        }
    })();
    match result {
        Ok((launcher_pid, _session)) => {
            handoff.shutdown();
            let released = child.release();
            debug_assert_eq!(released, i32::try_from(broker_pid).unwrap_or_default());
            println!("bangbang daemon pid: {launcher_pid}");
            Ok(0)
        }
        Err(error) => {
            handoff.shutdown();
            if child.terminate_and_reap().is_err() {
                Err(BrokerError::CleanupUncertain)
            } else {
                Err(error)
            }
        }
    }
}

fn run_spawned_topology(
    spawned: super::process::SpawnedLauncherTransition,
    target: CredentialTarget,
    mode: VmnetTopologyMode,
    outer: PinnedExecutable,
    correlation: SessionId,
    daemon_handoff: Option<DaemonChildHandoff>,
) -> Result<u8, BrokerError> {
    let super::process::SpawnedLauncherTransition {
        mut child,
        topology,
        provider,
    } = spawned;
    let launcher_pid = u32::try_from(child.pid()).map_err(|_| BrokerError::Process)?;
    let context = VmnetTopologyContext::new(correlation, target, launcher_pid, mode)
        .map_err(|_| BrokerError::InvalidConfiguration)?;
    let mut topology =
        VmnetTopologyTransport::new(topology, TOPOLOGY_IO_TIMEOUT).map_err(map_topology_error)?;

    let result = run_topology_protocol(
        &mut child,
        &mut topology,
        provider,
        outer,
        context,
        daemon_handoff,
    );
    if result.is_err() {
        topology.shutdown();
        let cleanup = child.terminate_and_reap();
        if cleanup.is_err() {
            return Err(BrokerError::CleanupUncertain);
        }
    }
    result
}

struct DaemonChildHandoff {
    transport: VmnetTopologyTransport,
}

impl DaemonChildHandoff {
    fn authorize_ready(
        &mut self,
        context: VmnetTopologyContext,
        session: SessionId,
    ) -> Result<(), BrokerError> {
        self.transport
            .send(VmnetTopologyMessage::LauncherReady { context, session })
            .map_err(map_topology_error)?;
        expect(
            self.transport.receive().map_err(map_topology_error)?,
            VmnetTopologyMessage::ReadyAck { context, session },
        )?;
        Ok(())
    }

    fn confirm_ready(
        &mut self,
        context: VmnetTopologyContext,
        session: SessionId,
    ) -> Result<(), BrokerError> {
        self.transport
            .send(VmnetTopologyMessage::BrokerReady { context, session })
            .map_err(map_topology_error)?;
        self.transport.shutdown();
        Ok(())
    }
}

impl std::fmt::Debug for DaemonChildHandoff {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DaemonChildHandoff(<redacted>)")
    }
}

fn run_topology_protocol(
    child: &mut OwnedChild,
    topology: &mut VmnetTopologyTransport,
    provider: UnixStream,
    outer: PinnedExecutable,
    context: VmnetTopologyContext,
    mut daemon_handoff: Option<DaemonChildHandoff>,
) -> Result<u8, BrokerError> {
    topology
        .send(VmnetTopologyMessage::Start(context))
        .map_err(map_topology_error)?;
    expect(
        topology.receive().map_err(map_topology_error)?,
        VmnetTopologyMessage::Dropped(context),
    )?;
    validate_child_credentials(child.pid(), context.target())?;
    topology
        .send(VmnetTopologyMessage::DropAck(context))
        .map_err(map_topology_error)?;
    topology
        .send(VmnetTopologyMessage::OuterStart(context))
        .map_err(map_topology_error)?;
    expect(
        topology.receive().map_err(map_topology_error)?,
        VmnetTopologyMessage::OuterHello(context),
    )?;
    outer.validate_child(child.pid())?;
    validate_child_credentials(child.pid(), context.target())?;
    topology
        .send(VmnetTopologyMessage::Proceed(context))
        .map_err(map_topology_error)?;

    let (session, authority) = match topology.receive().map_err(map_topology_error)? {
        VmnetTopologyMessage::Activate {
            context: received,
            session,
            authority,
        } if received == context => (session, authority),
        _ => return Err(BrokerError::Protocol),
    };
    let bootstrap = BrokerBootstrap::new(session, context.target(), authority)
        .map_err(|_| BrokerError::InvalidConfiguration)?;
    let provider_shutdown = provider
        .try_clone()
        .map_err(|error| BrokerError::Io(error.kind()))?;
    let broker = std::thread::Builder::new()
        .name("bangbang-vmnet-broker".into())
        .spawn(move || broker_service::run_bootstrap(bootstrap, provider))
        .map_err(|error| BrokerError::Io(error.kind()))?;
    let mut broker = BrokerTask {
        handle: Some(broker),
        shutdown: provider_shutdown,
    };
    topology
        .send(VmnetTopologyMessage::BrokerReady { context, session })
        .map_err(map_topology_error)?;

    if let Err(error) = wait_ready(topology, child, &broker, context, session) {
        let broker_result = broker.finish();
        return match broker_result {
            Err(BrokerError::CleanupUncertain) => Err(BrokerError::CleanupUncertain),
            Ok(()) | Err(_) => Err(error),
        };
    }
    if let Some(handoff) = daemon_handoff.as_mut() {
        handoff.authorize_ready(context, session)?;
    }
    topology
        .send(VmnetTopologyMessage::ReadyAck { context, session })
        .map_err(map_topology_error)?;
    if let Some(handoff) = daemon_handoff.as_mut() {
        handoff.confirm_ready(context, session)?;
    }

    let terminal = wait_terminal(topology, child, &broker, context, session)?;
    let broker_result = broker.finish();
    let acknowledged = match (terminal, broker_result) {
        (VmnetTopologyTerminal::Complete, Ok(())) => VmnetTopologyTerminal::Complete,
        (_, Err(_)) => VmnetTopologyTerminal::Provider,
        (result, Ok(())) => result,
    };
    topology
        .send(VmnetTopologyMessage::TerminalAck {
            context,
            session,
            result: acknowledged,
        })
        .map_err(map_topology_error)?;
    topology.shutdown();
    let status = child.wait()?;
    broker_result?;
    if acknowledged == VmnetTopologyTerminal::Complete && !status.success() {
        return Err(BrokerError::Process);
    }
    exit_code(status)
}

fn wait_ready(
    topology: &mut VmnetTopologyTransport,
    child: &mut OwnedChild,
    broker: &BrokerTask,
    context: VmnetTopologyContext,
    session: SessionId,
) -> Result<(), BrokerError> {
    let deadline = Instant::now()
        .checked_add(TOPOLOGY_IO_TIMEOUT)
        .ok_or(BrokerError::Timeout)?;
    let mut broker_finished_deadline = None;
    loop {
        if poll_readable(topology.as_raw_fd(), POLL_INTERVAL)? {
            expect(
                topology.receive().map_err(map_topology_error)?,
                VmnetTopologyMessage::LauncherReady { context, session },
            )?;
            return Ok(());
        }
        if child.try_wait()?.is_some() {
            return Err(BrokerError::Process);
        }
        // A short-lived provider consumer may complete Shutdown before the
        // ordinary launcher thread gets scheduled to publish LauncherReady.
        // Keep the final broker result authoritative, but allow the already
        // bound topology handshake one bounded interval to catch up.
        if broker.is_finished() && broker_finished_deadline.is_none() {
            broker_finished_deadline = Instant::now().checked_add(TERMINATION_TIMEOUT);
        }
        if broker_finished_deadline.is_some_and(|finished| Instant::now() >= finished) {
            return Err(BrokerError::Process);
        }
        if Instant::now() >= deadline {
            return Err(BrokerError::Timeout);
        }
    }
}

fn wait_terminal(
    topology: &mut VmnetTopologyTransport,
    child: &mut OwnedChild,
    broker: &BrokerTask,
    context: VmnetTopologyContext,
    session: SessionId,
) -> Result<VmnetTopologyTerminal, BrokerError> {
    let mut broker_finished_deadline = None;
    loop {
        if child.try_wait()?.is_some() {
            return Err(BrokerError::Process);
        }
        if poll_readable(topology.as_raw_fd(), POLL_INTERVAL)? {
            return match topology.receive().map_err(map_topology_error)? {
                VmnetTopologyMessage::Terminal {
                    context: received,
                    session: received_session,
                    result,
                } if received == context && received_session == session => Ok(result),
                VmnetTopologyMessage::Cancel {
                    context: received,
                    session: received_session,
                    ..
                } if received == context && received_session == session => {
                    Err(BrokerError::Protocol)
                }
                _ => Err(BrokerError::Protocol),
            };
        }
        if broker.is_finished() && broker_finished_deadline.is_none() {
            broker_finished_deadline = Instant::now().checked_add(TERMINATION_TIMEOUT);
        }
        if broker_finished_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(BrokerError::Timeout);
        }
    }
}

struct BrokerTask {
    handle: Option<std::thread::JoinHandle<Result<(), BrokerError>>>,
    shutdown: UnixStream,
}

impl BrokerTask {
    fn is_finished(&self) -> bool {
        self.handle
            .as_ref()
            .is_none_or(std::thread::JoinHandle::is_finished)
    }

    fn finish(&mut self) -> Result<(), BrokerError> {
        let deadline = Instant::now()
            .checked_add(TERMINATION_TIMEOUT)
            .ok_or(BrokerError::Timeout)?;
        while !self.is_finished() && Instant::now() < deadline {
            std::thread::sleep(POLL_INTERVAL);
        }
        if !self.is_finished() {
            let _ = self.shutdown.shutdown(std::net::Shutdown::Both);
        }
        self.handle
            .take()
            .ok_or(BrokerError::Process)?
            .join()
            .map_err(|_| BrokerError::Process)?
    }
}

impl Drop for BrokerTask {
    fn drop(&mut self) {
        let _ = self.shutdown.shutdown(std::net::Shutdown::Both);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl std::fmt::Debug for BrokerTask {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("BrokerTask(<redacted>)")
    }
}

pub fn run_private_daemon_broker(args: Vec<OsString>) -> Result<(), BrokerError> {
    require_exact_root()?;
    require_private_environment(VMNET_DAEMON_ENV_KEY, VMNET_DAEMON_ENV_VALUE)?;
    let launcher_args = parse_private_transition_args(args)?;
    // SAFETY: The suspended same-image daemon child is not a process-group
    // leader and has not created threads; `setsid` establishes its fixed
    // detached session before it creates any product child.
    if unsafe { libc::setsid() } <= 0 {
        return Err(BrokerError::Process);
    }
    // SAFETY: These process/session queries have no pointer contract.
    let (pid, session, process_group) =
        unsafe { (libc::getpid(), libc::getsid(0), libc::getpgrp()) };
    if pid <= 0 || session != pid || process_group != pid {
        return Err(BrokerError::Process);
    }
    std::env::set_current_dir("/").map_err(|error| BrokerError::Io(error.kind()))?;
    let handoff =
        adopt_connected_stream(BOOTSTRAP_FD).map_err(|_| BrokerError::BootstrapDescriptor)?;
    let mut handoff =
        VmnetTopologyTransport::new(handoff, TOPOLOGY_IO_TIMEOUT).map_err(map_topology_error)?;
    let handoff_context = match handoff.receive().map_err(map_topology_error)? {
        VmnetTopologyMessage::Start(context) => context,
        _ => return Err(BrokerError::Protocol),
    };
    if handoff_context.launcher_pid() != std::process::id()
        || handoff_context.mode() != VmnetTopologyMode::Daemon
    {
        return Err(BrokerError::Protocol);
    }
    let peer = bangbang_session::macos::peer_identity(handoff.as_raw_fd())
        .map_err(|_| BrokerError::Descriptor)?;
    // SAFETY: `getppid` has no pointer contract.
    let parent = unsafe { libc::getppid() };
    if peer.uid != 0 || peer.gid != 0 || peer.pid != parent || parent <= 0 {
        return Err(BrokerError::Authority);
    }
    handoff
        .send(VmnetTopologyMessage::Dropped(handoff_context))
        .map_err(map_topology_error)?;
    expect(
        handoff.receive().map_err(map_topology_error)?,
        VmnetTopologyMessage::DropAck(handoff_context),
    )?;

    let provider = PinnedExecutable::current()?;
    let layout = ProviderProductLayout::from_provider_executable(current_executable_path()?)?;
    let outer = PinnedExecutable::at(&layout.launcher)?;
    let _worker = PinnedExecutable::at(&layout.worker)?;
    provider.revalidate_path()?;
    let spawned = spawn_launcher_transition(&provider, &launcher_args)?;
    let code = run_spawned_topology(
        spawned,
        handoff_context.target(),
        VmnetTopologyMode::Daemon,
        outer,
        handoff_context.correlation(),
        Some(DaemonChildHandoff { transport: handoff }),
    )?;
    if code == 0 {
        Ok(())
    } else {
        Err(BrokerError::Process)
    }
}

pub fn run_private_launcher_transition(args: Vec<OsString>) -> Result<(), BrokerError> {
    require_exact_root()?;
    require_private_environment(VMNET_TOPOLOGY_ENV_KEY, VMNET_TOPOLOGY_ENV_VALUE)?;
    let launcher_args = parse_private_transition_args(args)?;
    let topology =
        adopt_connected_stream(VMNET_TOPOLOGY_FD).map_err(|_| BrokerError::BootstrapDescriptor)?;
    let provider = adopt_connected_stream(VMNET_TOPOLOGY_PROVIDER_FD)
        .map_err(|_| BrokerError::ProviderDescriptor)?;
    let mut transport =
        VmnetTopologyTransport::new(topology, TOPOLOGY_IO_TIMEOUT).map_err(map_topology_error)?;
    let context = match transport.receive().map_err(map_topology_error)? {
        VmnetTopologyMessage::Start(context) => context,
        _ => return Err(BrokerError::Protocol),
    };
    if context.launcher_pid() != std::process::id()
        || context.target().uid() == 0
        || context.target().gid() == 0
    {
        return Err(BrokerError::Protocol);
    }
    let peer = bangbang_session::macos::peer_identity(transport.as_raw_fd())
        .map_err(|_| BrokerError::Descriptor)?;
    // SAFETY: `getppid` has no pointer or ownership contract.
    let parent = unsafe { libc::getppid() };
    if peer.uid != 0 || peer.gid != 0 || peer.pid != parent || parent <= 0 {
        return Err(BrokerError::Authority);
    }

    let layout = ProviderProductLayout::from_provider_executable(current_executable_path()?)?;
    let outer = PinnedExecutable::at(&layout.launcher)?;
    let _worker = PinnedExecutable::at(&layout.worker)?;
    enter_launcher_session(context.mode())?;
    // Start from a fixed location before discarding root authority. Launcher
    // inputs are required to carry their own absolute resource authority.
    std::env::set_current_dir("/").map_err(|error| BrokerError::Io(error.kind()))?;
    bangbang_session::macos::credential::transition_process(context.target())
        .map_err(|_| BrokerError::Authority)?;
    bangbang_session::macos::credential::attest_current_process(context.target())
        .map_err(|_| BrokerError::Authority)?;
    transport
        .send(VmnetTopologyMessage::Dropped(context))
        .map_err(map_topology_error)?;
    expect(
        transport.receive().map_err(map_topology_error)?,
        VmnetTopologyMessage::DropAck(context),
    )?;
    outer.revalidate_path()?;

    clear_cloexec(transport.as_raw_fd())?;
    clear_cloexec(provider.as_raw_fd())?;
    exec_outer(outer.path(), launcher_args, transport, provider)
}

fn enter_launcher_session(mode: VmnetTopologyMode) -> Result<(), BrokerError> {
    if !mode.is_daemon() {
        return Ok(());
    }
    // SAFETY: This same-image transition child has not created threads and is
    // not a process-group leader. A distinct session keeps the ordinary outer
    // daemon contract independent from its detached root broker parent.
    if unsafe { libc::setsid() } <= 0 {
        return Err(BrokerError::Process);
    }
    // SAFETY: These process/session queries have no pointer contract.
    let (pid, session, process_group) =
        unsafe { (libc::getpid(), libc::getsid(0), libc::getpgrp()) };
    if pid > 0 && session == pid && process_group == pid {
        Ok(())
    } else {
        Err(BrokerError::Process)
    }
}

fn exec_outer(
    path: &std::ffi::CStr,
    launcher_args: Vec<OsString>,
    topology: VmnetTopologyTransport,
    provider: UnixStream,
) -> Result<(), BrokerError> {
    let mut arguments = Vec::new();
    arguments
        .try_reserve_exact(launcher_args.len().saturating_add(1))
        .map_err(|_| BrokerError::InvalidConfiguration)?;
    arguments.push(CString::from(path));
    for argument in launcher_args {
        arguments.push(CString::new(argument.into_vec()).map_err(|_| BrokerError::Process)?);
    }
    let argv = arguments
        .iter()
        .map(|argument| argument.as_ptr())
        .chain(std::iter::once(ptr::null::<c_char>()))
        .collect::<Vec<_>>();
    let marker = CString::new(format!(
        "{VMNET_TOPOLOGY_ENV_KEY}={VMNET_TOPOLOGY_ENV_VALUE}"
    ))
    .map_err(|_| BrokerError::Process)?;
    let environment = [marker.as_ptr(), ptr::null::<c_char>()];
    let _topology = topology;
    let _provider = provider;
    // SAFETY: `path`, every argv/environment C string, and both null-terminated
    // pointer arrays remain live. A successful exec does not return.
    unsafe {
        libc::execve(path.as_ptr(), argv.as_ptr(), environment.as_ptr());
    }
    Err(BrokerError::Process)
}

fn require_private_environment(key: &str, value: &str) -> Result<(), BrokerError> {
    let expected_key = OsStr::new(key);
    let expected_value = OsStr::new(value);
    let mut variables = std::env::vars_os();
    match (variables.next(), variables.next()) {
        (Some((key, value)), None) if key == expected_key && value == expected_value => Ok(()),
        _ => Err(BrokerError::Authority),
    }
}

fn current_executable_path() -> Result<PathBuf, BrokerError> {
    let path = std::env::current_exe().map_err(|error| BrokerError::Io(error.kind()))?;
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(BrokerError::Process)
    }
}

struct ProviderProductLayout {
    launcher: PathBuf,
    worker: PathBuf,
}

impl ProviderProductLayout {
    fn from_provider_executable(provider: PathBuf) -> Result<Self, BrokerError> {
        if provider.file_name() != Some(OsStr::new(PROVIDER_EXECUTABLE_NAME)) {
            return Err(BrokerError::InvalidConfiguration);
        }
        let helpers = exact_parent(&provider, "Helpers")?;
        let contents = exact_parent(&helpers, "Contents")?;
        let bundle = exact_parent(&contents, OUTER_BUNDLE_NAME)?;
        validate_root_ancestry(bundle.parent().ok_or(BrokerError::InvalidConfiguration)?)?;
        for directory in [&bundle, &contents, &helpers, &contents.join("MacOS")] {
            validate_root_directory(directory)?;
        }
        let launcher = contents.join("MacOS").join(LAUNCHER_EXECUTABLE_NAME);
        let worker_bundle = helpers.join(WORKER_BUNDLE_NAME);
        let worker_contents = worker_bundle.join("Contents");
        let worker_macos = worker_contents.join("MacOS");
        for directory in [&worker_bundle, &worker_contents, &worker_macos] {
            validate_root_directory(directory)?;
        }
        Ok(Self {
            launcher,
            worker: worker_macos.join(WORKER_EXECUTABLE_NAME),
        })
    }
}

fn exact_parent(path: &Path, expected: &str) -> Result<PathBuf, BrokerError> {
    let parent = path.parent().ok_or(BrokerError::InvalidConfiguration)?;
    if parent.file_name() == Some(OsStr::new(expected)) {
        Ok(parent.to_path_buf())
    } else {
        Err(BrokerError::InvalidConfiguration)
    }
}

fn validate_root_directory(path: &Path) -> Result<(), BrokerError> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::symlink_metadata(path).map_err(|_| BrokerError::Process)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != 0
        || metadata.gid() != 0
        || metadata.mode() & 0o7022 != 0
    {
        Err(BrokerError::Process)
    } else {
        Ok(())
    }
}

fn validate_root_ancestry(mut path: &Path) -> Result<(), BrokerError> {
    use std::os::unix::fs::MetadataExt;

    loop {
        let metadata = std::fs::symlink_metadata(path).map_err(|_| BrokerError::Process)?;
        let mode = metadata.mode();
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.uid() != 0
            || metadata.gid() != 0
            || !trusted_ancestry_mode(mode)
        {
            return Err(BrokerError::Process);
        }
        path = match path.parent() {
            Some(parent) => parent,
            None if path == Path::new("/") => return Ok(()),
            None => return Err(BrokerError::Process),
        };
    }
}

const fn trusted_ancestry_mode(mode: u32) -> bool {
    mode & 0o022 == 0 || mode & (libc::S_ISVTX as u32) != 0
}

fn clear_cloexec(descriptor: libc::c_int) -> Result<(), BrokerError> {
    // SAFETY: `F_GETFD` inspects the live owned descriptor only.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    if flags < 0 {
        return Err(BrokerError::Io(io::Error::last_os_error().kind()));
    }
    // SAFETY: `F_SETFD` changes only descriptor inheritance on the same live
    // owned descriptor.
    if unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
        return Err(BrokerError::Io(io::Error::last_os_error().kind()));
    }
    Ok(())
}

fn poll_readable(descriptor: libc::c_int, timeout: Duration) -> Result<bool, BrokerError> {
    let mut entry = libc::pollfd {
        fd: descriptor,
        events: libc::POLLIN,
        revents: 0,
    };
    let milliseconds =
        libc::c_int::try_from(timeout.as_millis()).map_err(|_| BrokerError::Timeout)?;
    // SAFETY: `entry` is one writable poll record for the synchronous call.
    let result = unsafe { libc::poll(&raw mut entry, 1, milliseconds) };
    if result < 0 {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::Interrupted {
            Ok(false)
        } else {
            Err(BrokerError::Io(error.kind()))
        };
    }
    if result == 0 {
        return Ok(false);
    }
    if entry.revents & libc::POLLNVAL != 0 {
        return Err(BrokerError::Protocol);
    }
    Ok(entry.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0)
}

fn expect(actual: VmnetTopologyMessage, expected: VmnetTopologyMessage) -> Result<(), BrokerError> {
    if actual == expected {
        Ok(())
    } else {
        Err(BrokerError::Protocol)
    }
}

fn map_topology_error(error: VmnetTopologyTransportError) -> BrokerError {
    match error {
        VmnetTopologyTransportError::Timeout => BrokerError::Timeout,
        VmnetTopologyTransportError::Disconnected | VmnetTopologyTransportError::Invalid => {
            BrokerError::Protocol
        }
        VmnetTopologyTransportError::Io(kind) => BrokerError::Io(kind),
    }
}

fn exit_code(status: std::process::ExitStatus) -> Result<u8, BrokerError> {
    use std::os::unix::process::ExitStatusExt;

    if let Some(code) = status.code() {
        u8::try_from(code).map_err(|_| BrokerError::Process)
    } else if let Some(signal) = status.signal() {
        128_i32
            .checked_add(signal)
            .and_then(|value| u8::try_from(value).ok())
            .ok_or(BrokerError::Process)
    } else {
        Err(BrokerError::Process)
    }
}

impl std::fmt::Debug for ProviderProductLayout {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ProviderProductLayout(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{trusted_ancestry_mode, validate_root_ancestry};

    #[test]
    fn root_ancestry_rejects_replaceable_nonsticky_directories() {
        assert!(trusted_ancestry_mode(0o040755));
        assert!(trusted_ancestry_mode(0o041777));
        assert!(!trusted_ancestry_mode(0o040775));
        assert!(!trusted_ancestry_mode(0o040777));
        assert_eq!(validate_root_ancestry(Path::new("/")), Ok(()));
    }
}
