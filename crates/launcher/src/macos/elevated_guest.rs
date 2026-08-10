//! Feature-only supervision for the post-grant elevated guest evidence channel.

use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::{UnixDatagram, UnixStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bangbang_session::elevated_probe::{
    CredentialRole, GuestEvidenceKind, GuestEvidencePhase, GuestEvidenceRecord,
    GuestSerialTranscript, MAX_GUEST_SERIAL_TRANSCRIPT_BYTES, ProbeBootstrap, ProbeErrorCategory,
    ProbeStage, RuntimeFault, RuntimeWorkload, classify_guest_serial_transcript,
};
use bangbang_session::macos::grant_transport::GrantTransportError;
use bangbang_session::macos::guest_evidence::{receive_guest_evidence, send_guest_evidence};
use bangbang_session::macos::runtime::LauncherNamespace;
use bangbang_session::macos::verify_peer_pid;
use bangbang_session::{Readiness, SessionId, TerminalCategory};

use super::code_sign::{WorkerProfile, validate_launcher_process, validate_worker_process};
use super::local_socket::{
    LocalSocketConnectStart, PendingLocalSocket, anchored_child_is_absent, begin_connect_anchored,
    finish_connect_anchored, validate_anchored_child,
};
use super::vhost_user_broker::ScopedConnectError;
use crate::LauncherError;
use crate::grant_manifest::{ElevatedGuestContract, SocketDirectoryAnchor};

const WITNESS_TIMEOUT: Duration = Duration::from_secs(5);
const GUEST_COMPLETION_TIMEOUT: Duration = Duration::from_secs(30);
const API_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const API_SOCKET_MODE: u32 = 0o600;
const API_RESPONSE: &[u8] =
    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

fn extend_api_response(response: &mut Vec<u8>, bytes: &[u8]) -> Result<(), ()> {
    response.extend_from_slice(bytes);
    if response.len() > API_RESPONSE.len() || !API_RESPONSE.starts_with(response) {
        return Err(());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ElevatedGuestFailure {
    stage: ProbeStage,
    category: ProbeErrorCategory,
}

impl ElevatedGuestFailure {
    pub(crate) const fn stage(self) -> ProbeStage {
        self.stage
    }

    pub(crate) const fn category(self) -> ProbeErrorCategory {
        self.category
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ElevatedGuestFailureHandle {
    failure: Arc<Mutex<Option<ElevatedGuestFailure>>>,
}

impl ElevatedGuestFailureHandle {
    pub(crate) fn failure(&self) -> Option<ElevatedGuestFailure> {
        *self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WitnessStep {
    AwaitingGrantAcceptance,
    AwaitingResourceRequest,
    SendingResourceAck,
    AwaitingHvfRequest,
    SendingHvfAck,
    AwaitingHvfCreated,
    AwaitingGuestShutdown,
    Complete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ApiRequestStep {
    Logger,
    Metrics,
    Serial,
    Machine,
    Boot,
    Drive,
    InstanceStart,
    Complete,
}

impl ApiRequestStep {
    const fn stage(self) -> ProbeStage {
        match self {
            Self::Logger => ProbeStage::ApiLoggerConfiguration,
            Self::Metrics => ProbeStage::ApiMetricsConfiguration,
            Self::Serial => ProbeStage::ApiSerialConfiguration,
            Self::Machine => ProbeStage::ApiMachineConfiguration,
            Self::Boot => ProbeStage::ApiBootConfiguration,
            Self::Drive => ProbeStage::ApiDriveConfiguration,
            Self::InstanceStart => ProbeStage::ApiInstanceStart,
            Self::Complete => ProbeStage::GuestTerminalEvidence,
        }
    }

    const fn next(self) -> Self {
        match self {
            Self::Logger => Self::Metrics,
            Self::Metrics => Self::Serial,
            Self::Serial => Self::Machine,
            Self::Machine => Self::Boot,
            Self::Boot => Self::Drive,
            Self::Drive => Self::InstanceStart,
            Self::InstanceStart | Self::Complete => Self::Complete,
        }
    }
}

#[derive(Debug)]
enum ApiIo {
    Connecting(PendingLocalSocket),
    Writing {
        stream: UnixStream,
        source_identity: bangbang_session::ObjectIdentity,
        request: Vec<u8>,
        written: usize,
    },
    Reading {
        stream: UnixStream,
        source_identity: bangbang_session::ObjectIdentity,
        response: Vec<u8>,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ApiEventInterest {
    pub(crate) read: bool,
    pub(crate) write: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ApiDriverFailure {
    stage: ProbeStage,
    category: ProbeErrorCategory,
}

#[derive(Debug)]
struct ElevatedApiDriver {
    anchor: SocketDirectoryAnchor,
    expected_uid: u32,
    expected_gid: u32,
    fault: RuntimeFault,
    step: ApiRequestStep,
    io: Option<ApiIo>,
    socket_identity: Option<bangbang_session::ObjectIdentity>,
    retired: Vec<UnixStream>,
    deadline: Option<Instant>,
}

impl ElevatedApiDriver {
    fn new(
        anchor: SocketDirectoryAnchor,
        expected_uid: u32,
        expected_gid: u32,
        fault: RuntimeFault,
        worker_pid: libc::pid_t,
    ) -> Result<Self, ApiDriverFailure> {
        let mut driver = Self {
            anchor,
            expected_uid,
            expected_gid,
            fault,
            step: ApiRequestStep::Logger,
            io: None,
            socket_identity: None,
            retired: Vec::new(),
            deadline: None,
        };
        driver.begin_step(worker_pid)?;
        Ok(driver)
    }

    fn begin_step(&mut self, worker_pid: libc::pid_t) -> Result<(), ApiDriverFailure> {
        let stage = self.step.stage();
        if self.step == ApiRequestStep::Complete || self.fault.stage() == Some(stage) {
            return Err(ApiDriverFailure {
                stage,
                category: ProbeErrorCategory::Other,
            });
        }
        let connection = begin_connect_anchored(
            self.anchor.descriptor(),
            self.anchor.identity(),
            c"evidence-api.sock",
            self.expected_uid,
            self.expected_gid,
            API_SOCKET_MODE,
        )
        .map_err(|error| api_socket_failure(stage, error))?;
        self.io = Some(match connection {
            LocalSocketConnectStart::Connected(connection) => {
                let source_identity = connection.source_identity();
                self.accept_socket_identity(stage, source_identity)?;
                let stream = connection.into_stream();
                verify_peer_pid(stream.as_raw_fd(), worker_pid).map_err(|_| ApiDriverFailure {
                    stage,
                    category: ProbeErrorCategory::PermissionDenied,
                })?;
                ApiIo::Writing {
                    stream,
                    source_identity,
                    request: api_request(self.step),
                    written: 0,
                }
            }
            LocalSocketConnectStart::Pending(pending) => {
                self.accept_socket_identity(stage, pending.source_identity())?;
                ApiIo::Connecting(pending)
            }
        });
        self.deadline = Some(api_deadline(stage)?);
        Ok(())
    }

    fn accept_socket_identity(
        &mut self,
        stage: ProbeStage,
        identity: bangbang_session::ObjectIdentity,
    ) -> Result<(), ApiDriverFailure> {
        if self
            .socket_identity
            .is_some_and(|expected| expected != identity)
        {
            return Err(ApiDriverFailure {
                stage,
                category: ProbeErrorCategory::InvalidInput,
            });
        }
        self.socket_identity.get_or_insert(identity);
        Ok(())
    }

    fn descriptor(&self) -> Option<RawFd> {
        match self.io.as_ref()? {
            ApiIo::Connecting(pending) => Some(pending.as_raw_fd()),
            ApiIo::Writing { stream, .. } | ApiIo::Reading { stream, .. } => {
                Some(stream.as_raw_fd())
            }
        }
    }

    fn interest(&self) -> ApiEventInterest {
        match self.io {
            Some(ApiIo::Connecting(_) | ApiIo::Writing { .. }) => ApiEventInterest {
                read: false,
                write: true,
            },
            Some(ApiIo::Reading { .. }) => ApiEventInterest {
                read: true,
                write: false,
            },
            None => ApiEventInterest::default(),
        }
    }

    fn handle_event(
        &mut self,
        readable: bool,
        writable: bool,
        worker_pid: libc::pid_t,
    ) -> Result<(), ApiDriverFailure> {
        let stage = self.step.stage();
        let Some(io) = self.io.take() else {
            return if self.step == ApiRequestStep::Complete {
                Ok(())
            } else {
                Err(ApiDriverFailure {
                    stage,
                    category: ProbeErrorCategory::InvalidInput,
                })
            };
        };
        let io = match io {
            ApiIo::Connecting(pending) if readable || writable => {
                match finish_connect_anchored(pending)
                    .map_err(|error| api_socket_failure(stage, error))?
                {
                    LocalSocketConnectStart::Pending(pending) => ApiIo::Connecting(pending),
                    LocalSocketConnectStart::Connected(connection) => {
                        let source_identity = connection.source_identity();
                        self.accept_socket_identity(stage, source_identity)?;
                        let stream = connection.into_stream();
                        verify_peer_pid(stream.as_raw_fd(), worker_pid).map_err(|_| {
                            ApiDriverFailure {
                                stage,
                                category: ProbeErrorCategory::PermissionDenied,
                            }
                        })?;
                        ApiIo::Writing {
                            stream,
                            source_identity,
                            request: api_request(self.step),
                            written: 0,
                        }
                    }
                }
            }
            io => io,
        };
        let io = match io {
            ApiIo::Writing {
                mut stream,
                source_identity,
                request,
                mut written,
            } if writable => {
                while written < request.len() {
                    let remaining = request.get(written..).ok_or(ApiDriverFailure {
                        stage,
                        category: ProbeErrorCategory::InvalidInput,
                    })?;
                    match stream.write(remaining) {
                        Ok(0) => {
                            return Err(ApiDriverFailure {
                                stage,
                                category: ProbeErrorCategory::Other,
                            });
                        }
                        Ok(length) => written += length,
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                        Err(error) => {
                            return Err(ApiDriverFailure {
                                stage,
                                category: ProbeErrorCategory::from_io_kind(error.kind()),
                            });
                        }
                    }
                }
                if written == request.len() {
                    stream
                        .shutdown(std::net::Shutdown::Write)
                        .map_err(|error| ApiDriverFailure {
                            stage,
                            category: ProbeErrorCategory::from_io_kind(error.kind()),
                        })?;
                    ApiIo::Reading {
                        stream,
                        source_identity,
                        response: Vec::with_capacity(API_RESPONSE.len()),
                    }
                } else {
                    ApiIo::Writing {
                        stream,
                        source_identity,
                        request,
                        written,
                    }
                }
            }
            io => io,
        };
        let io = match io {
            ApiIo::Reading {
                mut stream,
                source_identity,
                mut response,
            } if readable => {
                let mut buffer = [0_u8; 128];
                loop {
                    match stream.read(&mut buffer) {
                        Ok(0) => {
                            if response != API_RESPONSE {
                                return Err(ApiDriverFailure {
                                    stage,
                                    category: ProbeErrorCategory::InvalidInput,
                                });
                            }
                            validate_anchored_child(
                                self.anchor.descriptor(),
                                self.anchor.identity(),
                                c"evidence-api.sock",
                                self.expected_uid,
                                self.expected_gid,
                                API_SOCKET_MODE,
                                source_identity,
                            )
                            .map_err(|error| api_socket_failure(stage, error))?;
                            verify_peer_pid(stream.as_raw_fd(), worker_pid).map_err(|_| {
                                ApiDriverFailure {
                                    stage,
                                    category: ProbeErrorCategory::PermissionDenied,
                                }
                            })?;
                            self.retired.push(stream);
                            self.step = self.step.next();
                            self.deadline = None;
                            if self.step != ApiRequestStep::Complete {
                                self.begin_step(worker_pid)?;
                            }
                            return Ok(());
                        }
                        Ok(length) => {
                            let bytes = buffer.get(..length).ok_or(ApiDriverFailure {
                                stage,
                                category: ProbeErrorCategory::InvalidInput,
                            })?;
                            if extend_api_response(&mut response, bytes).is_err() {
                                return Err(ApiDriverFailure {
                                    stage,
                                    category: ProbeErrorCategory::InvalidInput,
                                });
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                        Err(error) => {
                            return Err(ApiDriverFailure {
                                stage,
                                category: ProbeErrorCategory::from_io_kind(error.kind()),
                            });
                        }
                    }
                }
                ApiIo::Reading {
                    stream,
                    source_identity,
                    response,
                }
            }
            io => io,
        };
        self.io = Some(io);
        Ok(())
    }

    fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    fn has_timed_out(&self, now: Instant) -> Option<ApiDriverFailure> {
        self.deadline
            .is_some_and(|deadline| now >= deadline)
            .then_some(ApiDriverFailure {
                stage: self.step.stage(),
                category: ProbeErrorCategory::Other,
            })
    }

    fn instance_start_in_flight(&self) -> bool {
        self.step == ApiRequestStep::InstanceStart && matches!(self.io, Some(ApiIo::Reading { .. }))
    }

    fn is_complete(&self) -> bool {
        self.step == ApiRequestStep::Complete && self.io.is_none()
    }

    fn finish_after_peer_exit(&mut self, worker_pid: libc::pid_t) -> Result<(), ApiDriverFailure> {
        for _ in 0..=7 {
            if self.is_complete() {
                return Ok(());
            }
            self.handle_event(true, true, worker_pid)?;
        }
        Err(ApiDriverFailure {
            stage: self.step.stage(),
            category: ProbeErrorCategory::Other,
        })
    }

    fn clear_retired(&mut self) {
        self.retired.clear();
    }
}

fn api_socket_failure(stage: ProbeStage, error: ScopedConnectError) -> ApiDriverFailure {
    let category = match error {
        ScopedConnectError::Failure(kind) => ProbeErrorCategory::from_io_kind(kind),
        ScopedConnectError::Rejected | ScopedConnectError::Invalid => {
            ProbeErrorCategory::InvalidInput
        }
    };
    ApiDriverFailure { stage, category }
}

fn api_deadline(stage: ProbeStage) -> Result<Instant, ApiDriverFailure> {
    Instant::now()
        .checked_add(API_REQUEST_TIMEOUT)
        .ok_or(ApiDriverFailure {
            stage,
            category: ProbeErrorCategory::Other,
        })
}

fn api_request(step: ApiRequestStep) -> Vec<u8> {
    use bangbang_session::elevated_probe::{
        GUEST_BOOT_ARGS, GUEST_INITRD_REFERENCE, GUEST_KERNEL_REFERENCE, GUEST_LOGGER_REFERENCE,
        GUEST_METRICS_REFERENCE, GUEST_ROOTFS_REFERENCE, GUEST_SERIAL_REFERENCE,
    };

    let (path, body) = match step {
        ApiRequestStep::Logger => (
            "/logger",
            format!(
                r#"{{"log_path":"{GUEST_LOGGER_REFERENCE}","level":"Info","show_level":true,"show_log_origin":true}}"#,
            ),
        ),
        ApiRequestStep::Metrics => (
            "/metrics",
            format!(r#"{{"metrics_path":"{GUEST_METRICS_REFERENCE}"}}"#),
        ),
        ApiRequestStep::Serial => (
            "/serial",
            format!(r#"{{"serial_out_path":"{GUEST_SERIAL_REFERENCE}"}}"#),
        ),
        ApiRequestStep::Machine => (
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":128}"#.to_string(),
        ),
        ApiRequestStep::Boot => (
            "/boot-source",
            format!(
                r#"{{"kernel_image_path":"{GUEST_KERNEL_REFERENCE}","initrd_path":"{GUEST_INITRD_REFERENCE}","boot_args":"{GUEST_BOOT_ARGS}"}}"#,
            ),
        ),
        ApiRequestStep::Drive => (
            "/drives/rootfs",
            format!(
                r#"{{"drive_id":"rootfs","path_on_host":"{GUEST_ROOTFS_REFERENCE}","is_root_device":true,"is_read_only":true}}"#,
            ),
        ),
        ApiRequestStep::InstanceStart => {
            ("/actions", r#"{"action_type":"InstanceStart"}"#.to_string())
        }
        ApiRequestStep::Complete => return Vec::new(),
    };
    format!(
        "PUT {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// Closed launcher state for one elevated guest continuation.
pub(crate) struct ElevatedGuestSupervisor {
    bootstrap: ProbeBootstrap,
    contract: ElevatedGuestContract,
    expected_worker_profile: WorkerProfile,
    session: SessionId,
    step: WitnessStep,
    pending_ack: Option<GuestEvidenceRecord>,
    deadline: Option<Instant>,
    transport_closed: bool,
    readiness: Option<Readiness>,
    api: Option<ElevatedApiDriver>,
    failure: Arc<Mutex<Option<ElevatedGuestFailure>>>,
}

impl std::fmt::Debug for ElevatedGuestSupervisor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ElevatedGuestSupervisor(<redacted>)")
    }
}

impl ElevatedGuestSupervisor {
    pub(crate) fn new(
        bootstrap: ProbeBootstrap,
        contract: ElevatedGuestContract,
        expected_worker_profile: WorkerProfile,
        session: SessionId,
    ) -> Result<(Self, ElevatedGuestFailureHandle), LauncherError> {
        let workload = bootstrap
            .mode()
            .runtime_workload()
            .ok_or(LauncherError::InvalidLaunchPolicy)?;
        let valid = matches!(
            workload,
            RuntimeWorkload::GuestNoApi | RuntimeWorkload::GuestApi
        ) && workload == contract.workload()
            && match workload {
                RuntimeWorkload::GuestNoApi => contract.api_anchor().is_none(),
                RuntimeWorkload::GuestApi => contract.api_anchor().is_some(),
                RuntimeWorkload::RepresentativeGrants => false,
            }
            && !session.is_pre_session();
        if !valid {
            return Err(LauncherError::InvalidLaunchPolicy);
        }
        let failure = Arc::new(Mutex::new(None));
        let handle = ElevatedGuestFailureHandle {
            failure: Arc::clone(&failure),
        };
        Ok((
            Self {
                bootstrap,
                contract,
                expected_worker_profile,
                session,
                step: WitnessStep::AwaitingGrantAcceptance,
                pending_ack: None,
                deadline: None,
                transport_closed: false,
                readiness: None,
                api: None,
                failure,
            },
            handle,
        ))
    }

    pub(crate) fn activate(
        &mut self,
        socket: &UnixDatagram,
        worker_pid: libc::pid_t,
        session_stream: &UnixStream,
        namespace: Option<&LauncherNamespace>,
    ) -> Result<(), LauncherError> {
        let api_child_absent = match self.contract.workload() {
            RuntimeWorkload::GuestApi => self.contract.api_anchor().is_some_and(|anchor| {
                anchored_child_is_absent(
                    anchor.descriptor(),
                    anchor.identity(),
                    c"evidence-api.sock",
                ) == Ok(true)
            }),
            RuntimeWorkload::GuestNoApi => self.contract.api_anchor().is_none(),
            RuntimeWorkload::RepresentativeGrants => false,
        };
        if self.step != WitnessStep::AwaitingGrantAcceptance
            || self.pending_ack.is_some()
            || !matches!(transport_state(socket), TransportState::Empty)
            || !api_child_absent
        {
            return self.fail(
                ProbeStage::GuestResourceWitness,
                ProbeErrorCategory::InvalidInput,
            );
        }
        self.revalidate(
            ProbeStage::GuestResourceWitness,
            socket,
            worker_pid,
            session_stream,
            namespace,
        )?;
        self.step = WitnessStep::AwaitingResourceRequest;
        self.deadline = Some(deadline_after(WITNESS_TIMEOUT)?);
        Ok(())
    }

    pub(crate) fn handle_readable(
        &mut self,
        socket: &UnixDatagram,
        worker_pid: libc::pid_t,
        session_stream: &UnixStream,
        namespace: Option<&LauncherNamespace>,
    ) -> Result<(), LauncherError> {
        if self.transport_closed {
            return self.fail(self.expected_stage(), ProbeErrorCategory::InvalidInput);
        }
        loop {
            match transport_state(socket) {
                TransportState::Empty => return Ok(()),
                TransportState::Closed => {
                    self.transport_closed = true;
                    return if matches!(
                        self.step,
                        WitnessStep::AwaitingGuestShutdown | WitnessStep::Complete
                    ) && self.pending_ack.is_none()
                    {
                        Ok(())
                    } else {
                        self.fail(
                            missing_serial_stage(self.contract.workload(), self.readiness),
                            ProbeErrorCategory::Other,
                        )
                    };
                }
                TransportState::Error(kind) => {
                    return self.fail(
                        self.expected_stage(),
                        ProbeErrorCategory::from_io_kind(kind),
                    );
                }
                TransportState::Data => {}
            }
            if self.pending_ack.is_some()
                || matches!(
                    self.step,
                    WitnessStep::AwaitingGrantAcceptance
                        | WitnessStep::SendingResourceAck
                        | WitnessStep::SendingHvfAck
                        | WitnessStep::Complete
                )
            {
                return self.fail(self.expected_stage(), ProbeErrorCategory::InvalidInput);
            }
            let record = match receive_guest_evidence(socket) {
                Ok(record) => record,
                Err(GrantTransportError::Io(io::ErrorKind::Interrupted)) => continue,
                Err(GrantTransportError::Io(io::ErrorKind::WouldBlock)) => return Ok(()),
                Err(GrantTransportError::Io(kind)) => {
                    return self.fail(
                        self.expected_stage(),
                        ProbeErrorCategory::from_io_kind(kind),
                    );
                }
                Err(GrantTransportError::Invalid) => {
                    return self.fail(self.expected_stage(), ProbeErrorCategory::InvalidInput);
                }
            };
            self.accept_record(record, socket, worker_pid, session_stream, namespace)?;
            if self.bootstrap.fault() == RuntimeFault::GuestTimeout
                && self.step == WitnessStep::AwaitingGuestShutdown
            {
                return Ok(());
            }
            if self.pending_ack.is_some() {
                return Ok(());
            }
        }
    }

    fn accept_record(
        &mut self,
        record: GuestEvidenceRecord,
        socket: &UnixDatagram,
        worker_pid: libc::pid_t,
        session_stream: &UnixStream,
        namespace: Option<&LauncherNamespace>,
    ) -> Result<(), LauncherError> {
        let (phase, kind, stage, next) = match self.step {
            WitnessStep::AwaitingResourceRequest => (
                GuestEvidencePhase::ResourceClaim,
                GuestEvidenceKind::Request,
                ProbeStage::GuestResourceWitness,
                WitnessStep::SendingResourceAck,
            ),
            WitnessStep::AwaitingHvfRequest => (
                GuestEvidencePhase::HvfCreate,
                GuestEvidenceKind::Request,
                ProbeStage::GuestHvfWitness,
                WitnessStep::SendingHvfAck,
            ),
            WitnessStep::AwaitingHvfCreated => (
                GuestEvidencePhase::HvfCreated,
                GuestEvidenceKind::Report,
                ProbeStage::GuestHvfCreate,
                WitnessStep::AwaitingGuestShutdown,
            ),
            WitnessStep::AwaitingGuestShutdown => (
                GuestEvidencePhase::GuestShutdown,
                GuestEvidenceKind::Report,
                ProbeStage::GuestTerminalEvidence,
                WitnessStep::Complete,
            ),
            WitnessStep::AwaitingGrantAcceptance
            | WitnessStep::SendingResourceAck
            | WitnessStep::SendingHvfAck
            | WitnessStep::Complete => {
                return self.fail(self.expected_stage(), ProbeErrorCategory::InvalidInput);
            }
        };
        let api_phase_valid = match (self.contract.workload(), phase) {
            (_, GuestEvidencePhase::ResourceClaim) => {
                self.readiness.is_none() && self.api.is_none()
            }
            (RuntimeWorkload::GuestNoApi, GuestEvidencePhase::HvfCreate)
            | (RuntimeWorkload::GuestNoApi, GuestEvidencePhase::HvfCreated) => {
                matches!(self.readiness, None | Some(Readiness::NoApi)) && self.api.is_none()
            }
            (RuntimeWorkload::GuestApi, GuestEvidencePhase::HvfCreate) => {
                self.readiness == Some(Readiness::Api)
                    && self
                        .api
                        .as_ref()
                        .is_some_and(ElevatedApiDriver::instance_start_in_flight)
            }
            (RuntimeWorkload::GuestApi, GuestEvidencePhase::HvfCreated) => {
                self.readiness == Some(Readiness::Api)
                    && self
                        .api
                        .as_ref()
                        .is_some_and(|api| api.instance_start_in_flight() || api.is_complete())
            }
            (RuntimeWorkload::GuestNoApi, GuestEvidencePhase::GuestShutdown) => {
                self.readiness == Some(Readiness::NoApi)
            }
            (RuntimeWorkload::GuestApi, GuestEvidencePhase::GuestShutdown) => {
                self.readiness == Some(Readiness::Api)
                    && self
                        .api
                        .as_ref()
                        .is_some_and(|api| api.instance_start_in_flight() || api.is_complete())
            }
            (RuntimeWorkload::RepresentativeGrants, _) => false,
        };
        if !api_phase_valid {
            return self.fail(stage, ProbeErrorCategory::InvalidInput);
        }
        if self.contract.workload() == RuntimeWorkload::GuestNoApi
            && phase == GuestEvidencePhase::HvfCreate
            && self.bootstrap.fault() == RuntimeFault::NoApiStartup
        {
            return self.fail(ProbeStage::NoApiStartup, ProbeErrorCategory::Other);
        }
        if !record.matches_expected(
            self.bootstrap.mode(),
            phase,
            kind,
            CredentialRole::Worker,
            self.bootstrap.nonce(),
            self.session,
        ) {
            return self.fail(stage, ProbeErrorCategory::InvalidInput);
        }
        self.revalidate(stage, socket, worker_pid, session_stream, namespace)?;
        self.step = next;
        match kind {
            GuestEvidenceKind::Request => {
                self.pending_ack = Some(
                    GuestEvidenceRecord::launcher_ack(
                        self.bootstrap.mode(),
                        phase,
                        self.bootstrap.nonce(),
                        self.session,
                    )
                    .map_err(|_| LauncherError::SessionProtocol)?,
                );
                self.deadline = Some(deadline_after(WITNESS_TIMEOUT)?);
            }
            GuestEvidenceKind::Report => {
                self.deadline = if next == WitnessStep::AwaitingGuestShutdown
                    && self.bootstrap.fault() == RuntimeFault::GuestTimeout
                {
                    Some(Instant::now())
                } else if next == WitnessStep::Complete {
                    None
                } else {
                    Some(deadline_after(GUEST_COMPLETION_TIMEOUT)?)
                };
            }
            GuestEvidenceKind::Ack => {
                return self.fail(stage, ProbeErrorCategory::InvalidInput);
            }
        }
        Ok(())
    }

    pub(crate) fn pump_write(&mut self, socket: &UnixDatagram) -> Result<(), LauncherError> {
        let Some(record) = self.pending_ack else {
            return Ok(());
        };
        match send_guest_evidence(socket, record) {
            Ok(()) => {
                self.pending_ack = None;
                self.step = match self.step {
                    WitnessStep::SendingResourceAck => WitnessStep::AwaitingHvfRequest,
                    WitnessStep::SendingHvfAck => WitnessStep::AwaitingHvfCreated,
                    _ => {
                        return self.fail(self.expected_stage(), ProbeErrorCategory::InvalidInput);
                    }
                };
                self.deadline = Some(deadline_after(
                    if self.step == WitnessStep::AwaitingHvfRequest {
                        GUEST_COMPLETION_TIMEOUT
                    } else {
                        WITNESS_TIMEOUT
                    },
                )?);
                Ok(())
            }
            Err(GrantTransportError::Io(
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock,
            )) => Ok(()),
            Err(GrantTransportError::Io(kind)) => self.fail(
                self.expected_stage(),
                ProbeErrorCategory::from_io_kind(kind),
            ),
            Err(GrantTransportError::Invalid) => {
                self.fail(self.expected_stage(), ProbeErrorCategory::InvalidInput)
            }
        }
    }

    pub(crate) const fn requires_write_event(&self) -> bool {
        self.pending_ack.is_some()
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        [
            self.deadline,
            self.api.as_ref().and_then(ElevatedApiDriver::deadline),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub(crate) fn has_timed_out(&mut self, now: Instant) -> Result<(), LauncherError> {
        if let Some(failure) = self.api.as_ref().and_then(|api| api.has_timed_out(now)) {
            return self.fail(failure.stage, failure.category);
        }
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            let stage = if self.step == WitnessStep::AwaitingGuestShutdown {
                ProbeStage::GuestTimeout
            } else {
                self.expected_stage()
            };
            return self.fail(stage, ProbeErrorCategory::Other);
        }
        Ok(())
    }

    pub(crate) fn observe_readiness(
        &mut self,
        readiness: Readiness,
        socket: &UnixDatagram,
        worker_pid: libc::pid_t,
        session_stream: &UnixStream,
        namespace: Option<&LauncherNamespace>,
    ) -> Result<(), LauncherError> {
        let workload = self.contract.workload();
        if !matches!(
            (workload, readiness),
            (RuntimeWorkload::GuestNoApi, Readiness::NoApi)
                | (RuntimeWorkload::GuestApi, Readiness::Api)
        ) {
            let stage = match workload {
                RuntimeWorkload::GuestApi => ProbeStage::ApiSocketPublication,
                RuntimeWorkload::GuestNoApi | RuntimeWorkload::RepresentativeGrants => {
                    ProbeStage::NoApiStartup
                }
            };
            return self.fail(stage, ProbeErrorCategory::InvalidInput);
        }
        if self.readiness.replace(readiness).is_some() || self.api.is_some() {
            return self.fail(
                ProbeStage::ApiSocketPublication,
                ProbeErrorCategory::InvalidInput,
            );
        }
        let stage = match workload {
            RuntimeWorkload::GuestApi => ProbeStage::ApiSocketPublication,
            RuntimeWorkload::GuestNoApi => ProbeStage::NoApiStartup,
            RuntimeWorkload::RepresentativeGrants => ProbeStage::GuestGrantContract,
        };
        if self.bootstrap.fault().stage() == Some(stage) {
            return self.fail(stage, ProbeErrorCategory::Other);
        }
        self.revalidate(stage, socket, worker_pid, session_stream, namespace)?;
        if workload == RuntimeWorkload::GuestApi {
            let anchor = self
                .contract
                .api_anchor()
                .ok_or(LauncherError::SessionProtocol)?;
            self.api = Some(
                ElevatedApiDriver::new(
                    anchor,
                    self.bootstrap.target_uid(),
                    self.bootstrap.target_gid(),
                    self.bootstrap.fault(),
                    worker_pid,
                )
                .map_err(|failure| self.record_api_failure(failure))?,
            );
        }
        Ok(())
    }

    pub(crate) fn api_descriptor(&self) -> Option<RawFd> {
        self.api.as_ref().and_then(ElevatedApiDriver::descriptor)
    }

    pub(crate) fn api_interest(&self) -> ApiEventInterest {
        self.api
            .as_ref()
            .map_or_else(ApiEventInterest::default, ElevatedApiDriver::interest)
    }

    pub(crate) fn handle_api_event(
        &mut self,
        readable: bool,
        writable: bool,
        worker_pid: libc::pid_t,
    ) -> Result<(), LauncherError> {
        let result = self
            .api
            .as_mut()
            .ok_or(LauncherError::SessionProtocol)?
            .handle_event(readable, writable, worker_pid);
        match result {
            Ok(()) => Ok(()),
            Err(failure) => self.fail(failure.stage, failure.category),
        }
    }

    pub(crate) fn fail_api_event(&mut self) -> Result<(), LauncherError> {
        let stage = self
            .api
            .as_ref()
            .map_or(ProbeStage::ApiSocketPublication, |api| api.step.stage());
        self.fail(stage, ProbeErrorCategory::Other)
    }

    pub(crate) fn fail_transport_event(&mut self) -> Result<(), LauncherError> {
        self.fail(self.expected_stage(), ProbeErrorCategory::Other)
    }

    pub(crate) fn fail_endpoint_death(&mut self) -> Result<(), LauncherError> {
        self.fail(
            missing_serial_stage(self.contract.workload(), self.readiness),
            ProbeErrorCategory::Other,
        )
    }

    pub(crate) fn clear_retired_api_streams(&mut self) {
        if let Some(api) = self.api.as_mut() {
            api.clear_retired();
        }
    }

    pub(crate) fn finish_after_worker_exit(
        &mut self,
        socket: &UnixDatagram,
        worker_pid: libc::pid_t,
        terminal: Option<(TerminalCategory, u8)>,
    ) -> Result<(), LauncherError> {
        let api_result = self
            .api
            .as_mut()
            .map(|api| api.finish_after_peer_exit(worker_pid))
            .transpose();
        if let Err(failure) = api_result {
            return self.fail(failure.stage, failure.category);
        }
        match read_guest_serial_evidence(
            self.contract,
            self.bootstrap.target_uid(),
            self.bootstrap.target_gid(),
        ) {
            GuestSerialEvidence::Success => {}
            GuestSerialEvidence::Failure | GuestSerialEvidence::Invalid => {
                return self.fail(ProbeStage::GuestOracle, ProbeErrorCategory::InvalidInput);
            }
            GuestSerialEvidence::Missing => {
                return self.fail(
                    missing_serial_stage(self.contract.workload(), self.readiness),
                    ProbeErrorCategory::Other,
                );
            }
        }
        if terminal.is_some_and(|(category, _)| category == TerminalCategory::Success) {
            let workload_complete = match self.contract.workload() {
                RuntimeWorkload::GuestNoApi => self.readiness == Some(Readiness::NoApi),
                RuntimeWorkload::GuestApi => {
                    self.readiness == Some(Readiness::Api)
                        && self
                            .api
                            .as_ref()
                            .is_some_and(ElevatedApiDriver::is_complete)
                }
                RuntimeWorkload::RepresentativeGrants => false,
            };
            if self.step != WitnessStep::Complete
                || self.pending_ack.is_some()
                || !workload_complete
            {
                return self.fail(
                    ProbeStage::GuestTerminalEvidence,
                    ProbeErrorCategory::InvalidInput,
                );
            }
            match transport_state(socket) {
                TransportState::Closed => self.transport_closed = true,
                TransportState::Data => {
                    return self.fail(
                        ProbeStage::GuestTerminalEvidence,
                        ProbeErrorCategory::InvalidInput,
                    );
                }
                TransportState::Empty | TransportState::Error(_) => {
                    return self.fail(ProbeStage::GuestEndpointDeath, ProbeErrorCategory::Other);
                }
            }
        } else if self.step != WitnessStep::Complete || self.pending_ack.is_some() {
            return self.fail(ProbeStage::GuestEndpointDeath, ProbeErrorCategory::Other);
        }
        Ok(())
    }

    pub(crate) fn finish_cleanup(&mut self) -> Result<(), LauncherError> {
        if let Some(anchor) = self.contract.api_anchor()
            && anchored_child_is_absent(
                anchor.descriptor(),
                anchor.identity(),
                c"evidence-api.sock",
            ) != Ok(true)
        {
            return self.fail(ProbeStage::GuestCleanup, ProbeErrorCategory::InvalidInput);
        }
        if self.bootstrap.fault() == RuntimeFault::GuestCleanup {
            return self.fail(ProbeStage::GuestCleanup, ProbeErrorCategory::Other);
        }
        Ok(())
    }

    fn revalidate(
        &mut self,
        stage: ProbeStage,
        socket: &UnixDatagram,
        worker_pid: libc::pid_t,
        session_stream: &UnixStream,
        namespace: Option<&LauncherNamespace>,
    ) -> Result<(), LauncherError> {
        // SAFETY: `getpid` has no pointer or ownership contract.
        let launcher_pid = unsafe { libc::getpid() };
        let valid = bangbang_session::elevated_credential::attest_current_process(
            self.bootstrap.mode(),
            self.bootstrap.target_uid(),
            self.bootstrap.target_gid(),
        )
        .is_ok()
            && verify_peer_pid(session_stream.as_raw_fd(), worker_pid).is_ok()
            && verify_peer_pid(socket.as_raw_fd(), worker_pid).is_ok()
            && validate_launcher_process(launcher_pid).is_ok()
            && validate_worker_process(worker_pid)
                .is_ok_and(|profile| profile == self.expected_worker_profile)
            && namespace.is_some_and(|namespace| namespace.verify_worker_lock().is_ok())
            && self.contract.workload()
                == self
                    .bootstrap
                    .mode()
                    .runtime_workload()
                    .unwrap_or(RuntimeWorkload::RepresentativeGrants);
        if valid {
            Ok(())
        } else {
            self.fail(stage, ProbeErrorCategory::PermissionDenied)
        }
    }

    fn expected_stage(&self) -> ProbeStage {
        match self.step {
            WitnessStep::AwaitingGrantAcceptance
            | WitnessStep::AwaitingResourceRequest
            | WitnessStep::SendingResourceAck => ProbeStage::GuestResourceWitness,
            WitnessStep::AwaitingHvfRequest | WitnessStep::SendingHvfAck => {
                ProbeStage::GuestHvfWitness
            }
            WitnessStep::AwaitingHvfCreated => ProbeStage::GuestHvfCreate,
            WitnessStep::AwaitingGuestShutdown | WitnessStep::Complete => {
                ProbeStage::GuestTerminalEvidence
            }
        }
    }

    fn record_api_failure(&mut self, failure: ApiDriverFailure) -> LauncherError {
        let _ = self.fail::<()>(failure.stage, failure.category);
        LauncherError::SessionProtocol
    }

    fn fail<T>(
        &mut self,
        stage: ProbeStage,
        category: ProbeErrorCategory,
    ) -> Result<T, LauncherError> {
        let mut failure = self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if failure.is_none() {
            *failure = Some(ElevatedGuestFailure { stage, category });
        }
        self.deadline = None;
        Err(LauncherError::SessionProtocol)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GuestSerialEvidence {
    Success,
    Failure,
    Missing,
    Invalid,
}

const fn missing_serial_stage(
    workload: RuntimeWorkload,
    readiness: Option<Readiness>,
) -> ProbeStage {
    match (workload, readiness) {
        (RuntimeWorkload::GuestApi, None) => ProbeStage::ApiSocketPublication,
        (RuntimeWorkload::GuestNoApi, None) => ProbeStage::NoApiStartup,
        _ => ProbeStage::GuestEndpointDeath,
    }
}

fn read_guest_serial_evidence(
    contract: ElevatedGuestContract,
    target_uid: u32,
    target_gid: u32,
) -> GuestSerialEvidence {
    let descriptor = contract.serial_evidence_descriptor();
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `stat` is writable and the contract borrows a live readback descriptor.
    if unsafe { libc::fstat(descriptor, stat.as_mut_ptr()) } != 0 {
        return GuestSerialEvidence::Invalid;
    }
    // SAFETY: successful fstat initialized the complete structure.
    let stat = unsafe { stat.assume_init() };
    let identity = bangbang_session::ObjectIdentity {
        device: normalized_device(stat.st_dev),
        inode: stat.st_ino,
    };
    // SAFETY: both fcntl operations inspect the same live borrowed descriptor.
    let descriptor_flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    // SAFETY: F_GETFL has no pointer or ownership contract.
    let status_flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    let Ok(length) = usize::try_from(stat.st_size) else {
        return GuestSerialEvidence::Invalid;
    };
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG
        || identity != contract.serial_evidence_identity()
        || stat.st_uid != target_uid
        || stat.st_gid != target_gid
        || stat.st_mode & 0o7777 != 0o600
        || stat.st_nlink != 1
        || stat.st_size < 0
        || length > MAX_GUEST_SERIAL_TRANSCRIPT_BYTES
        || descriptor_flags < 0
        || descriptor_flags & libc::FD_CLOEXEC == 0
        || status_flags < 0
        || status_flags & libc::O_ACCMODE != libc::O_RDONLY
    {
        return GuestSerialEvidence::Invalid;
    }
    if length == 0 {
        return GuestSerialEvidence::Missing;
    }
    let mut bytes = vec![0_u8; length];
    let mut read = 0_usize;
    while read < bytes.len() {
        let offset = match libc::off_t::try_from(read) {
            Ok(offset) => offset,
            Err(_) => return GuestSerialEvidence::Invalid,
        };
        // SAFETY: the unread suffix is writable and offset stays within the bounded file.
        let remaining = match bytes.get_mut(read..) {
            Some(remaining) => remaining,
            None => return GuestSerialEvidence::Invalid,
        };
        // SAFETY: The unread suffix is writable and offset stays within the bounded file.
        let result = unsafe {
            libc::pread(
                descriptor,
                remaining.as_mut_ptr().cast(),
                remaining.len(),
                offset,
            )
        };
        if result > 0 {
            let Some(next) = usize::try_from(result)
                .ok()
                .and_then(|length| read.checked_add(length))
                .filter(|next| *next <= bytes.len())
            else {
                return GuestSerialEvidence::Invalid;
            };
            read = next;
        } else if result >= 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return GuestSerialEvidence::Invalid;
        }
    }
    classify_guest_serial_bytes(&bytes)
}

fn normalized_device(device: libc::dev_t) -> u64 {
    u64::from(u32::from_ne_bytes(device.to_ne_bytes()))
}

fn classify_guest_serial_bytes(bytes: &[u8]) -> GuestSerialEvidence {
    if bytes.is_empty() {
        GuestSerialEvidence::Missing
    } else {
        match classify_guest_serial_transcript(bytes) {
            GuestSerialTranscript::Success => GuestSerialEvidence::Success,
            GuestSerialTranscript::Failure => GuestSerialEvidence::Failure,
            GuestSerialTranscript::Invalid => GuestSerialEvidence::Invalid,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TransportState {
    Empty,
    Data,
    Closed,
    Error(io::ErrorKind),
}

fn transport_state(socket: &UnixDatagram) -> TransportState {
    let mut byte = 0_u8;
    // SAFETY: `byte` is writable for one non-consuming byte probe and the socket is live.
    let result = unsafe {
        libc::recv(
            socket.as_raw_fd(),
            (&raw mut byte).cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    if result > 0 {
        return TransportState::Data;
    }
    if result == 0 {
        return match socket.send(&[]) {
            Ok(_) => TransportState::Data,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => TransportState::Data,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionReset
                        | io::ErrorKind::BrokenPipe
                        | io::ErrorKind::NotConnected
                ) =>
            {
                TransportState::Closed
            }
            Err(error) => TransportState::Error(error.kind()),
        };
    }
    let error = io::Error::last_os_error();
    if matches!(
        error.kind(),
        io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe | io::ErrorKind::NotConnected
    ) {
        return TransportState::Closed;
    }
    if error
        .raw_os_error()
        .is_some_and(|code| code == libc::EAGAIN || code == libc::EWOULDBLOCK)
    {
        TransportState::Empty
    } else {
        TransportState::Error(error.kind())
    }
}

fn deadline_after(duration: Duration) -> Result<Instant, LauncherError> {
    Instant::now()
        .checked_add(duration)
        .ok_or(LauncherError::SessionProtocol)
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixDatagram;

    use serde_json::Value;

    use super::*;

    #[test]
    fn api_requests_are_closed_ordered_and_use_only_guest_grant_references() {
        let steps = [
            ApiRequestStep::Logger,
            ApiRequestStep::Metrics,
            ApiRequestStep::Serial,
            ApiRequestStep::Machine,
            ApiRequestStep::Boot,
            ApiRequestStep::Drive,
            ApiRequestStep::InstanceStart,
        ];
        let paths = [
            "/logger",
            "/metrics",
            "/serial",
            "/machine-config",
            "/boot-source",
            "/drives/rootfs",
            "/actions",
        ];
        let bodies = [
            serde_json::json!({
                "log_path": bangbang_session::elevated_probe::GUEST_LOGGER_REFERENCE,
                "level": "Info",
                "show_level": true,
                "show_log_origin": true,
            }),
            serde_json::json!({
                "metrics_path": bangbang_session::elevated_probe::GUEST_METRICS_REFERENCE,
            }),
            serde_json::json!({
                "serial_out_path": bangbang_session::elevated_probe::GUEST_SERIAL_REFERENCE,
            }),
            serde_json::json!({"vcpu_count": 1, "mem_size_mib": 128}),
            serde_json::json!({
                "kernel_image_path": bangbang_session::elevated_probe::GUEST_KERNEL_REFERENCE,
                "initrd_path": bangbang_session::elevated_probe::GUEST_INITRD_REFERENCE,
                "boot_args": bangbang_session::elevated_probe::GUEST_BOOT_ARGS,
            }),
            serde_json::json!({
                "drive_id": "rootfs",
                "path_on_host": bangbang_session::elevated_probe::GUEST_ROOTFS_REFERENCE,
                "is_root_device": true,
                "is_read_only": true,
            }),
            serde_json::json!({"action_type": "InstanceStart"}),
        ];
        let mut step = ApiRequestStep::Logger;
        for ((expected, path), expected_body) in steps.into_iter().zip(paths).zip(bodies) {
            assert_eq!(step, expected);
            let request = api_request(step);
            assert!(!request.contains(&0), "request must never contain NUL");
            let boundary = request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .expect("request should contain a header boundary");
            let (head, body) = request.split_at(boundary + 4);
            let head = std::str::from_utf8(head).expect("request head should be UTF-8");
            assert!(head.starts_with(&format!("PUT {path} HTTP/1.1\r\n")));
            assert!(head.contains("Host: localhost\r\n"));
            assert!(head.contains("Content-Type: application/json\r\n"));
            assert!(head.contains("Connection: close\r\n"));
            assert!(head.contains(&format!("Content-Length: {}\r\n", body.len())));
            let body: Value = serde_json::from_slice(body).expect("body should be exact JSON");
            assert_eq!(body, expected_body);
            step = step.next();
        }
        assert_eq!(step, ApiRequestStep::Complete);
        assert!(api_request(ApiRequestStep::Complete).is_empty());

        let all = steps.into_iter().flat_map(api_request).collect::<Vec<_>>();
        let text = std::str::from_utf8(&all).expect("requests should be UTF-8");
        for reference in [
            bangbang_session::elevated_probe::GUEST_LOGGER_REFERENCE,
            bangbang_session::elevated_probe::GUEST_METRICS_REFERENCE,
            bangbang_session::elevated_probe::GUEST_SERIAL_REFERENCE,
            bangbang_session::elevated_probe::GUEST_KERNEL_REFERENCE,
            bangbang_session::elevated_probe::GUEST_INITRD_REFERENCE,
            bangbang_session::elevated_probe::GUEST_ROOTFS_REFERENCE,
        ] {
            assert!(text.contains(reference));
        }
        assert!(!text.contains("/tmp/"));
        assert!(!text.contains("/Users/"));
    }

    #[test]
    fn api_steps_map_to_their_exact_first_failure_stage() {
        for (step, stage) in [
            (ApiRequestStep::Logger, ProbeStage::ApiLoggerConfiguration),
            (ApiRequestStep::Metrics, ProbeStage::ApiMetricsConfiguration),
            (ApiRequestStep::Serial, ProbeStage::ApiSerialConfiguration),
            (ApiRequestStep::Machine, ProbeStage::ApiMachineConfiguration),
            (ApiRequestStep::Boot, ProbeStage::ApiBootConfiguration),
            (ApiRequestStep::Drive, ProbeStage::ApiDriveConfiguration),
            (ApiRequestStep::InstanceStart, ProbeStage::ApiInstanceStart),
        ] {
            assert_eq!(step.stage(), stage);
        }
    }

    #[test]
    fn transport_state_distinguishes_empty_data_and_closed_peer() {
        let (sender, receiver) = UnixDatagram::pair().expect("datagram pair should open");
        assert_eq!(transport_state(&receiver), TransportState::Empty);
        sender.send(&[]).expect("empty contamination should send");
        assert_eq!(transport_state(&receiver), TransportState::Data);
        let mut empty = [0_u8; 1];
        assert_eq!(
            receiver
                .recv(&mut empty)
                .expect("empty datagram should drain"),
            0
        );
        assert_eq!(transport_state(&receiver), TransportState::Empty);
        sender.send(b"x").expect("one byte should send");
        assert_eq!(transport_state(&receiver), TransportState::Data);
        let mut byte = [0_u8; 1];
        receiver.recv(&mut byte).expect("one byte should drain");
        drop(sender);
        assert_eq!(transport_state(&receiver), TransportState::Closed);
    }

    #[test]
    fn no_content_response_is_exact_and_bounded() {
        assert_eq!(
            API_RESPONSE,
            b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        assert!(API_RESPONSE.len() < 128);
    }

    #[test]
    fn api_response_decoder_accepts_partial_exact_bytes_and_rejects_every_surplus_shape() {
        for split in 0..=API_RESPONSE.len() {
            let mut response = Vec::new();
            extend_api_response(&mut response, &API_RESPONSE[..split])
                .expect("exact prefix should be accepted");
            extend_api_response(&mut response, &API_RESPONSE[split..])
                .expect("remaining exact response should be accepted");
            assert_eq!(response, API_RESPONSE);
        }

        for invalid in [
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".as_slice(),
            b"HTTP/1.1 204 No Content\r\nContent-Length: 1\r\nConnection: close\r\n\r\nx"
                .as_slice(),
            b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\nx"
                .as_slice(),
        ] {
            let mut response = Vec::new();
            assert!(extend_api_response(&mut response, invalid).is_err());
        }
    }

    #[test]
    fn guest_serial_evidence_is_closed_to_one_complete_oracle() {
        let success = b"BANGBANG_ROOTFS_WORKFLOW_OK\r\n[    0.026251] reboot: Power down\r\n";
        let failure = b"BANGBANG_ROOTFS_WORKFLOW_FAIL\r\n[    0.026251] reboot: Power down\r\n";
        assert_eq!(
            classify_guest_serial_bytes(success),
            GuestSerialEvidence::Success
        );
        assert_eq!(
            classify_guest_serial_bytes(failure),
            GuestSerialEvidence::Failure
        );
        assert_eq!(
            classify_guest_serial_bytes(b""),
            GuestSerialEvidence::Missing
        );
        for invalid in [
            b"BANGBANG_ROOTFS_WORKFLOW_OK".as_slice(),
            b"BANGBANG_ROOTFS_WORKFLOW_OK\r\n".as_slice(),
            b"BANGBANG_ROOTFS_WORKFLOW_OK\r\n[00000.026251] reboot: Power down\r\n".as_slice(),
            b"BANGBANG_ROOTFS_WORKFLOW_OK\r\n[    0.026251] reboot: Power down\r\nextra".as_slice(),
            b"unstructured\n".as_slice(),
        ] {
            assert_eq!(
                classify_guest_serial_bytes(invalid),
                GuestSerialEvidence::Invalid
            );
        }
    }

    #[test]
    fn missing_serial_before_readiness_reports_the_workload_boundary() {
        assert_eq!(
            missing_serial_stage(RuntimeWorkload::GuestApi, None),
            ProbeStage::ApiSocketPublication
        );
        assert_eq!(
            missing_serial_stage(RuntimeWorkload::GuestNoApi, None),
            ProbeStage::NoApiStartup
        );
        assert_eq!(
            missing_serial_stage(RuntimeWorkload::GuestApi, Some(Readiness::Api)),
            ProbeStage::GuestEndpointDeath
        );
        assert_eq!(
            missing_serial_stage(RuntimeWorkload::GuestNoApi, Some(Readiness::NoApi)),
            ProbeStage::GuestEndpointDeath
        );
    }
}
