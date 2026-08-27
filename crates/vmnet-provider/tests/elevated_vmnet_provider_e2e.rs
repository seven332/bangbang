// Integration-test helpers are ordinary functions, so keep the test-only
// exceptions scoped to this dedicated exact-host evidence target.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod macos_arm64 {
    use std::env;
    use std::fs;
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::fs::MetadataExt as _;
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::CommandExt as _;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, ExitStatus, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    use bangbang_session::credential::CredentialTarget;
    use bangbang_session::vmnet_provider::{
        ControlClientEvent, ControlMessage, DataClientEvent, MAX_PROVIDER_TIMEOUT,
        ProviderCancelReason, ProviderCleanup, ProviderTerminalCode, RequestedVmnetParameters,
        VmnetControlClient, VmnetDataClient, VmnetInterfaceId, VmnetPacketBatch, VmnetPolicySlot,
        VmnetProviderTransport,
    };
    use bangbang_session::{SessionId, VmnetAuthority};
    use bangbang_vmnet_provider::{BrokerBootstrap, PRIVATE_BROKER_MODE};

    const PROVIDER_ENV: &str = "BANGBANG_ELEVATED_VMNET_PROVIDER";
    const TARGET_UID_ENV: &str = "BANGBANG_ELEVATED_VMNET_TARGET_UID";
    const TARGET_GID_ENV: &str = "BANGBANG_ELEVATED_VMNET_TARGET_GID";
    const PROVIDER_NAME: &str = "bangbang-vmnet-provider";
    const PROVIDER_LIMIT: u64 = 512 * 1024 * 1024;
    const PROCESS_TIMEOUT: Duration = Duration::from_secs(10);
    const MIN_SOURCE_FD: libc::c_int = 10;
    const BOOTSTRAP_FD: libc::c_int = 3;
    const CONTROL_FD: libc::c_int = 4;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum EvidenceError {
        Authority,
        Artifact,
        Process,
        Protocol,
        Broker,
        ControlSend,
        ControlReceive,
        ControlState,
        BrokerConfiguration,
        BrokerProtocol,
        BrokerProcess,
        BrokerTimeout,
        BrokerCleanup,
        BrokerIo,
        BrokerAuthority,
        BrokerDescriptor,
        BrokerBootstrapDescriptor,
        BrokerProviderDescriptor,
        Start,
        Hello,
        Write,
        Read,
        DataStop,
        ControlStopSend,
        ControlStopReceive,
        ControlStopState,
        ControlStopUncertain,
        ControlStopTerminalProtocol,
        ControlStopTerminalBackend,
        ControlStopTerminalCleanup,
        ControlStopTerminalSupervisor,
        Shutdown,
        Data,
        Cleanup,
    }

    impl EvidenceError {
        const fn name(self) -> &'static str {
            match self {
                Self::Authority => "authority",
                Self::Artifact => "artifact",
                Self::Process => "process",
                Self::Protocol => "protocol",
                Self::Broker => "broker",
                Self::ControlSend => "control-send",
                Self::ControlReceive => "control-receive",
                Self::ControlState => "control-state",
                Self::BrokerConfiguration => "broker-configuration",
                Self::BrokerProtocol => "broker-protocol",
                Self::BrokerProcess => "broker-process",
                Self::BrokerTimeout => "broker-timeout",
                Self::BrokerCleanup => "broker-cleanup",
                Self::BrokerIo => "broker-io",
                Self::BrokerAuthority => "broker-authority",
                Self::BrokerDescriptor => "broker-descriptor",
                Self::BrokerBootstrapDescriptor => "broker-bootstrap-descriptor",
                Self::BrokerProviderDescriptor => "broker-provider-descriptor",
                Self::Start => "start",
                Self::Hello => "hello",
                Self::Write => "write",
                Self::Read => "read",
                Self::DataStop => "data-stop",
                Self::ControlStopSend => "control-stop-send",
                Self::ControlStopReceive => "control-stop-receive",
                Self::ControlStopState => "control-stop-state",
                Self::ControlStopUncertain => "control-stop-uncertain",
                Self::ControlStopTerminalProtocol => "control-stop-terminal-protocol",
                Self::ControlStopTerminalBackend => "control-stop-terminal-backend",
                Self::ControlStopTerminalCleanup => "control-stop-terminal-cleanup",
                Self::ControlStopTerminalSupervisor => "control-stop-terminal-supervisor",
                Self::Shutdown => "shutdown",
                Self::Data => "data",
                Self::Cleanup => "cleanup",
            }
        }
    }

    type EvidenceResult<T> = Result<T, EvidenceError>;

    fn current_ids() -> (u32, u32, u32, u32) {
        // SAFETY: Darwin credential getters have no pointer or ownership contract.
        unsafe {
            (
                libc::getuid(),
                libc::geteuid(),
                libc::getgid(),
                libc::getegid(),
            )
        }
    }

    fn require_root() -> EvidenceResult<()> {
        if current_ids() == (0, 0, 0, 0) {
            Ok(())
        } else {
            Err(EvidenceError::Authority)
        }
    }

    fn require_nonroot() -> EvidenceResult<()> {
        let (uid, euid, gid, egid) = current_ids();
        if uid != 0 && uid == euid && gid != 0 && gid == egid {
            Ok(())
        } else {
            Err(EvidenceError::Authority)
        }
    }

    fn parse_target(name: &str) -> EvidenceResult<u32> {
        let value = env::var(name).map_err(|_| EvidenceError::Authority)?;
        if value.is_empty()
            || value.starts_with('0')
            || !value.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(EvidenceError::Authority);
        }
        value
            .parse::<u32>()
            .ok()
            .filter(|value| *value != 0)
            .ok_or(EvidenceError::Authority)
    }

    fn target() -> EvidenceResult<CredentialTarget> {
        CredentialTarget::new(parse_target(TARGET_UID_ENV)?, parse_target(TARGET_GID_ENV)?)
            .map_err(|_| EvidenceError::Authority)
    }

    fn provider_path(require_root_owner: bool) -> EvidenceResult<PathBuf> {
        let path = PathBuf::from(env::var_os(PROVIDER_ENV).ok_or(EvidenceError::Artifact)?);
        if !path.is_absolute()
            || path.file_name().and_then(|name| name.to_str()) != Some(PROVIDER_NAME)
        {
            return Err(EvidenceError::Artifact);
        }
        let metadata = fs::symlink_metadata(&path).map_err(|_| EvidenceError::Artifact)?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() == 0
            || metadata.len() > PROVIDER_LIMIT
            || metadata.nlink() != 1
            || metadata.mode() & 0o7777 != 0o555
            || (require_root_owner && (metadata.uid() != 0 || metadata.gid() != 0))
        {
            return Err(EvidenceError::Artifact);
        }
        Ok(path)
    }

    struct BrokerProcess {
        child: Option<Child>,
        process_group: libc::pid_t,
        _bootstrap: UnixStream,
    }

    impl BrokerProcess {
        fn spawn(path: &Path, bootstrap: BrokerBootstrap) -> EvidenceResult<(Self, UnixStream)> {
            let (mut bootstrap_client, bootstrap_child) =
                UnixStream::pair().map_err(|_| EvidenceError::Process)?;
            let (control_client, control_child) =
                UnixStream::pair().map_err(|_| EvidenceError::Process)?;
            let bootstrap_child = duplicate_stream(bootstrap_child)?;
            let control_child = duplicate_stream(control_child)?;
            let bootstrap_descriptor = bootstrap_child.as_raw_fd();
            let control_descriptor = control_child.as_raw_fd();
            let mut command = Command::new(path);
            command
                .arg(PRIVATE_BROKER_MODE)
                .current_dir("/")
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0);
            // SAFETY: The closure performs only async-signal-safe descriptor
            // duplication against live, collision-free inherited descriptors.
            unsafe {
                command.pre_exec(move || {
                    if libc::dup2(bootstrap_descriptor, BOOTSTRAP_FD) < 0
                        || libc::dup2(control_descriptor, CONTROL_FD) < 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let child = command.spawn().map_err(|_| EvidenceError::Process)?;
            drop(bootstrap_child);
            drop(control_child);
            bootstrap_client
                .write_all(&bootstrap.encode())
                .map_err(|_| EvidenceError::Protocol)?;
            let process_group =
                libc::pid_t::try_from(child.id()).map_err(|_| EvidenceError::Process)?;
            Ok((
                Self {
                    child: Some(child),
                    process_group,
                    _bootstrap: bootstrap_client,
                },
                control_client,
            ))
        }

        fn wait_clean(mut self) -> EvidenceResult<()> {
            let mut child = self.child.take().ok_or(EvidenceError::Cleanup)?;
            let status = match wait_child(&mut child, PROCESS_TIMEOUT) {
                Ok(status) => status,
                Err(error) => {
                    signal_group(self.process_group, libc::SIGKILL);
                    let _ = child.wait();
                    return Err(error);
                }
            };
            if !status.success() || process_group_exists(self.process_group)? {
                signal_group(self.process_group, libc::SIGKILL);
                return Err(EvidenceError::Cleanup);
            }
            Ok(())
        }

        fn failure_category(&mut self) -> EvidenceError {
            let status = self
                .child
                .as_mut()
                .and_then(|child| child.try_wait().ok().flatten());
            match status.and_then(|status| status.code()) {
                Some(10) => EvidenceError::BrokerConfiguration,
                Some(11) => EvidenceError::BrokerProtocol,
                Some(12) => EvidenceError::BrokerProcess,
                Some(13) => EvidenceError::BrokerTimeout,
                Some(14) => EvidenceError::BrokerCleanup,
                Some(15) => EvidenceError::BrokerIo,
                Some(16) => EvidenceError::BrokerAuthority,
                Some(17) => EvidenceError::BrokerDescriptor,
                Some(18) => EvidenceError::BrokerBootstrapDescriptor,
                Some(19) => EvidenceError::BrokerProviderDescriptor,
                _ => EvidenceError::ControlReceive,
            }
        }
    }

    impl Drop for BrokerProcess {
        fn drop(&mut self) {
            if let Some(mut child) = self.child.take() {
                signal_group(self.process_group, libc::SIGTERM);
                if wait_child(&mut child, Duration::from_secs(2)).is_err() {
                    signal_group(self.process_group, libc::SIGKILL);
                    let _ = child.wait();
                }
            }
        }
    }

    fn duplicate_stream(stream: UnixStream) -> EvidenceResult<UnixStream> {
        // SAFETY: The source is live and success returns a fresh owned
        // close-on-exec descriptor above both fixed child destinations.
        let descriptor =
            unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_DUPFD_CLOEXEC, MIN_SOURCE_FD) };
        if descriptor < 0 {
            return Err(EvidenceError::Process);
        }
        // SAFETY: `descriptor` is the fresh uniquely owned fcntl result.
        Ok(UnixStream::from(unsafe {
            OwnedFd::from_raw_fd(descriptor)
        }))
    }

    fn signal_group(process_group: libc::pid_t, signal: libc::c_int) {
        if process_group > 0 {
            // SAFETY: The negative checked group is the isolated provider group.
            let _ = unsafe { libc::kill(-process_group, signal) };
        }
    }

    fn process_group_exists(process_group: libc::pid_t) -> EvidenceResult<bool> {
        // SAFETY: Signal zero queries only the isolated provider process group.
        if unsafe { libc::kill(-process_group, 0) } == 0 {
            return Ok(true);
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ESRCH) => Ok(false),
            _ => Err(EvidenceError::Cleanup),
        }
    }

    fn wait_child(child: &mut Child, timeout: Duration) -> EvidenceResult<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return Ok(status),
                Ok(None) => {}
                Err(_) => return Err(EvidenceError::Cleanup),
            }
            if Instant::now() >= deadline {
                return Err(EvidenceError::Cleanup);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    struct ProviderSession {
        broker: BrokerProcess,
        session: SessionId,
        state: VmnetControlClient,
        transport: VmnetProviderTransport,
    }

    impl ProviderSession {
        fn start(value: u8) -> EvidenceResult<Self> {
            let provider = provider_path(true)?;
            let session = SessionId::from_bytes([value; 32]);
            let bootstrap = BrokerBootstrap::new(
                session,
                target()?,
                VmnetAuthority::try_new(false, true, 4, &[])
                    .map_err(|_| EvidenceError::Protocol)?,
            )
            .map_err(|_| EvidenceError::Protocol)?;
            let (mut broker, control) =
                BrokerProcess::spawn(&provider, bootstrap).map_err(|_| EvidenceError::Broker)?;
            let mut transport = VmnetProviderTransport::new(control, MAX_PROVIDER_TIMEOUT)
                .map_err(|_| EvidenceError::Protocol)?;
            let mut state =
                VmnetControlClient::new(session).map_err(|_| EvidenceError::Protocol)?;
            transport
                .send(state.hello().map_err(|_| EvidenceError::ControlState)?)
                .map_err(|_| EvidenceError::ControlSend)?;
            let response = match transport.receive() {
                Ok(response) => response,
                Err(_) => return Err(broker.failure_category()),
            };
            if !matches!(
                state
                    .receive(response)
                    .map_err(|_| EvidenceError::ControlState)?,
                ControlClientEvent::Ready
            ) {
                return Err(EvidenceError::ControlState);
            }
            Ok(Self {
                broker,
                session,
                state,
                transport,
            })
        }

        fn start_interface(
            &mut self,
        ) -> EvidenceResult<(
            VmnetInterfaceId,
            [u8; 6],
            VmnetDataClient,
            VmnetProviderTransport,
        )> {
            let interface = VmnetInterfaceId::new(1).map_err(|_| EvidenceError::Protocol)?;
            self.transport
                .send(
                    self.state
                        .start(
                            interface,
                            VmnetPolicySlot::Shared,
                            RequestedVmnetParameters::new(None, None)
                                .map_err(|_| EvidenceError::Protocol)?,
                        )
                        .map_err(|_| EvidenceError::Protocol)?,
                )
                .map_err(|_| EvidenceError::Protocol)?;
            let event = self
                .state
                .receive(
                    self.transport
                        .receive()
                        .map_err(|_| EvidenceError::Protocol)?,
                )
                .map_err(|_| EvidenceError::Protocol)?;
            let ControlClientEvent::Started {
                interface: started_interface,
                generation,
                parameters,
                stream,
            } = event
            else {
                return Err(EvidenceError::Protocol);
            };
            if started_interface != interface {
                return Err(EvidenceError::Protocol);
            }
            let mac = parameters.mac();
            let data_state = VmnetDataClient::new(self.session, interface, generation, parameters)
                .map_err(|_| EvidenceError::Protocol)?;
            let data_transport = VmnetProviderTransport::new(stream, MAX_PROVIDER_TIMEOUT)
                .map_err(|_| EvidenceError::Protocol)?;
            Ok((interface, mac, data_state, data_transport))
        }

        fn stop_interface(&mut self, interface: VmnetInterfaceId) -> EvidenceResult<()> {
            self.transport
                .send(
                    self.state
                        .stop(interface)
                        .map_err(|_| EvidenceError::ControlStopState)?,
                )
                .map_err(|_| EvidenceError::ControlStopSend)?;
            let response = self
                .transport
                .receive()
                .map_err(|_| EvidenceError::ControlStopReceive)?;
            let message = response.frame().control_message().cloned();
            match self.state.receive(response) {
                Ok(ControlClientEvent::Stopped { .. }) => {}
                Ok(ControlClientEvent::PeerTerminal { code }) => {
                    return Err(match code {
                        ProviderTerminalCode::Protocol => {
                            EvidenceError::ControlStopTerminalProtocol
                        }
                        ProviderTerminalCode::Backend => EvidenceError::ControlStopTerminalBackend,
                        ProviderTerminalCode::Cleanup => EvidenceError::ControlStopTerminalCleanup,
                        ProviderTerminalCode::Supervisor => {
                            EvidenceError::ControlStopTerminalSupervisor
                        }
                    });
                }
                Err(_)
                    if matches!(
                        message,
                        Some(ControlMessage::Stopped {
                            cleanup: ProviderCleanup::Uncertain
                        })
                    ) =>
                {
                    return Err(EvidenceError::ControlStopUncertain);
                }
                Ok(_) | Err(_) => return Err(EvidenceError::ControlStopState),
            }
            Ok(())
        }

        fn shutdown(mut self) -> EvidenceResult<()> {
            self.transport
                .send(self.state.shutdown().map_err(|_| EvidenceError::Protocol)?)
                .map_err(|_| EvidenceError::Protocol)?;
            if !matches!(
                self.state
                    .receive(
                        self.transport
                            .receive()
                            .map_err(|_| EvidenceError::Protocol)?,
                    )
                    .map_err(|_| EvidenceError::Protocol)?,
                ControlClientEvent::Shutdown
            ) {
                return Err(EvidenceError::Protocol);
            }
            drop(self.transport);
            self.broker.wait_clean()
        }
    }

    fn handshake_data(
        state: &mut VmnetDataClient,
        transport: &mut VmnetProviderTransport,
    ) -> EvidenceResult<()> {
        transport
            .send(state.hello().map_err(|_| EvidenceError::Protocol)?)
            .map_err(|_| EvidenceError::Protocol)?;
        if !matches!(
            state
                .receive(transport.receive().map_err(|_| EvidenceError::Protocol)?,)
                .map_err(|_| EvidenceError::Protocol)?,
            DataClientEvent::Ready
        ) {
            return Err(EvidenceError::Protocol);
        }
        Ok(())
    }

    fn run_data_lifecycle() -> EvidenceResult<()> {
        require_root()?;
        let mut session = ProviderSession::start(41)?;
        let (interface, realized_mac, mut data, mut transport) = session
            .start_interface()
            .map_err(|_| EvidenceError::Start)?;
        handshake_data(&mut data, &mut transport).map_err(|_| EvidenceError::Hello)?;

        let mut frame = [0_u8; 60];
        frame[..6].fill(0xff);
        frame[6..12].copy_from_slice(&realized_mac);
        frame[12..14].copy_from_slice(&[0x88, 0xb5]);
        let batch = VmnetPacketBatch::write(&[&frame]).map_err(|_| EvidenceError::Data)?;
        transport
            .send(data.write(batch).map_err(|_| EvidenceError::Write)?)
            .map_err(|_| EvidenceError::Write)?;
        let mut ready = false;
        let mut written = false;
        for _ in 0..8 {
            match data
                .receive(transport.receive().map_err(|_| EvidenceError::Write)?)
                .map_err(|_| EvidenceError::Write)?
            {
                DataClientEvent::Readiness { .. } => ready = true,
                DataClientEvent::WriteComplete {
                    completed_packets: 1,
                } => written = true,
                _ => return Err(EvidenceError::Write),
            }
            if ready && written {
                break;
            }
        }
        if !ready || !written {
            return Err(EvidenceError::Data);
        }
        transport
            .send(data.read(1).map_err(|_| EvidenceError::Read)?)
            .map_err(|_| EvidenceError::Read)?;
        let mut read = false;
        for _ in 0..8 {
            match data
                .receive(transport.receive().map_err(|_| EvidenceError::Read)?)
                .map_err(|_| EvidenceError::Read)?
            {
                DataClientEvent::Readiness { .. } => {}
                DataClientEvent::ReadComplete { packets } if packets.packet_count() == 1 => {
                    read = true;
                    break;
                }
                _ => return Err(EvidenceError::Read),
            }
        }
        if !read {
            return Err(EvidenceError::Data);
        }
        transport
            .send(data.stop().map_err(|_| EvidenceError::DataStop)?)
            .map_err(|_| EvidenceError::DataStop)?;
        loop {
            match data
                .receive(transport.receive().map_err(|_| EvidenceError::DataStop)?)
                .map_err(|_| EvidenceError::DataStop)?
            {
                DataClientEvent::Readiness { .. } => {}
                DataClientEvent::Stopped => break,
                _ => return Err(EvidenceError::DataStop),
            }
        }
        transport
            .send(data.shutdown().map_err(|_| EvidenceError::Shutdown)?)
            .map_err(|_| EvidenceError::Shutdown)?;
        if !matches!(
            data.receive(transport.receive().map_err(|_| EvidenceError::Shutdown)?,)
                .map_err(|_| EvidenceError::Shutdown)?,
            DataClientEvent::Shutdown
        ) {
            return Err(EvidenceError::Shutdown);
        }
        drop(transport);
        session.stop_interface(interface)?;
        session.shutdown().map_err(|_| EvidenceError::Shutdown)
    }

    fn run_cancellation() -> EvidenceResult<()> {
        require_root()?;
        let mut session = ProviderSession::start(42)?;
        let (_interface, _mac, mut data, mut data_transport) = session.start_interface()?;
        handshake_data(&mut data, &mut data_transport)?;
        session
            .transport
            .send(
                session
                    .state
                    .cancel(ProviderCancelReason::Launcher)
                    .map_err(|_| EvidenceError::Protocol)?,
            )
            .map_err(|_| EvidenceError::Protocol)?;
        let mut supervisor_terminal = false;
        for _ in 0..8 {
            match data
                .receive(
                    data_transport
                        .receive()
                        .map_err(|_| EvidenceError::Protocol)?,
                )
                .map_err(|_| EvidenceError::Protocol)?
            {
                DataClientEvent::Readiness { .. } => {}
                DataClientEvent::PeerTerminal {
                    code: ProviderTerminalCode::Supervisor,
                } => {
                    supervisor_terminal = true;
                    break;
                }
                _ => return Err(EvidenceError::Protocol),
            }
        }
        if !supervisor_terminal {
            return Err(EvidenceError::Protocol);
        }
        if !matches!(
            session
                .state
                .receive(
                    session
                        .transport
                        .receive()
                        .map_err(|_| EvidenceError::Protocol)?,
                )
                .map_err(|_| EvidenceError::Protocol)?,
            ControlClientEvent::Cancelled
        ) {
            return Err(EvidenceError::Protocol);
        }
        drop(data_transport);
        drop(session.transport);
        session.broker.wait_clean()
    }

    fn run_ordinary_denial() -> EvidenceResult<()> {
        require_nonroot()?;
        let provider = provider_path(false)?;
        let status = Command::new(provider)
            .arg(PRIVATE_BROKER_MODE)
            .current_dir("/")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| EvidenceError::Process)?;
        if status.code() == Some(16) {
            Ok(())
        } else {
            Err(EvidenceError::Authority)
        }
    }

    fn assert_result(result: EvidenceResult<()>) {
        if let Err(error) = result {
            panic!(
                "bangbang elevated vmnet provider evidence failed category={}",
                error.name()
            );
        }
    }

    #[test]
    fn ordinary_user_provider_broker_is_denied() {
        assert_result(run_ordinary_denial());
    }

    #[test]
    fn dropped_provider_serves_data_lifecycle() {
        assert_result(run_data_lifecycle());
    }

    #[test]
    fn control_cancellation_reaps_dropped_provider() {
        assert_result(run_cancellation());
    }
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
mod unsupported {
    #[test]
    fn ordinary_user_provider_broker_is_denied() {
        panic!("bangbang elevated vmnet provider evidence failed category=platform");
    }

    #[test]
    fn dropped_provider_serves_data_lifecycle() {
        panic!("bangbang elevated vmnet provider evidence failed category=platform");
    }

    #[test]
    fn control_cancellation_reaps_dropped_provider() {
        panic!("bangbang elevated vmnet provider evidence failed category=platform");
    }
}
