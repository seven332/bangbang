use std::env;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::{UnixDatagram, UnixStream};
use std::process::ExitCode;
use std::time::Duration;

use bangbang_hvf::HvfBackend;
use bangbang_runtime::VmBackend;
use bangbang_session::elevated_probe::{
    BOOTSTRAP_RECORD_BYTES, CONTINUATION_ACK_BYTES, CREDENTIAL_DATAGRAM_BYTES,
    CREDENTIAL_RECORD_BYTES, ContinuationAck, CredentialDatagramPhase, CredentialDatagramProof,
    CredentialFailureValue, CredentialGroupClass, CredentialIdentityClass, CredentialPrefix,
    CredentialRecord, CredentialRecordKind, CredentialRole, CredentialSelfState, CredentialStep,
    PeerObservation, ProbeBootstrap, ProbeErrorCategory, ProbeResult, ProbeStage, READY_RECORD,
    RESULT_RECORD_BYTES, ROOT_FD, RuntimeFault, RuntimeWorkerFailure, WORKER_ACTIVATION,
};
use bangbang_session::{GRANT_FD, ObjectIdentity, SESSION_ENV_KEY, SESSION_ENV_VALUE, SESSION_FD};

use bangbang_session::macos::grant_transport::GrantTransportError;
use bangbang_session::macos::runtime::{
    ExplicitRuntimeRoot, RuntimeError, ValidatedWorkerNamespace, WorkerNamespace,
};
use bangbang_session::macos::runtime_authority::receive_runtime_session_authority;

const CREDENTIAL_TIMEOUT: Duration = Duration::from_secs(5);
const CREDENTIAL_WORKER_ARTIFACT: &str =
    "bangbang-elevated-credential-worker-v1-credential-drop-BBC1-BBG1-restore-groups";
const RUNTIME_WORKER_ARTIFACT: &str = "bangbang-elevated-runtime-worker-v2-runtime-drop-runtime-retain-root-runtime-unmapped-BBA1-BBN1-adopted-session---bangbang-internal-grant-probe-v1-target-runtime";
const RUNTIME_WORKER_BOUNDARY_ARTIFACT: &str = "bangbang-elevated-runtime-worker-boundaries-v2-pre-ack-post-ack-session-create-session-open-authority-send-authority-receive-authority-validate-session-lock-session-enter-prepared-namespace-grant-transfer-proceed-terminal-continuation-ack-lifecycle-hello-runtime-session-create-runtime-session-open-runtime-authority-send-runtime-authority-receive-runtime-authority-validate-runtime-session-lock-runtime-session-enter-lifecycle-prepared-runtime-namespace-grant-accepted-lifecycle-proceed-lifecycle-terminal-runtime-cleanup-complete-continuation-boundary-identity-boundary-explicit-root-boundary-namespace-boundary-grant-boundary-lifecycle-boundary";
const GUEST_WORKER_BOUNDARY_ARTIFACT: &str = "bangbang-elevated-guest-worker-boundaries-v1-guest-no-api-drop-guest-no-api-retain-root-guest-no-api-unmapped-guest-api-drop-guest-api-retain-root-guest-api-unmapped-BBW1-guest-resource-witness-guest-grant-accepted-guest-transport-contamination-guest-hvf-witness-guest-terminal-evidence-bangbang-grant:evidence-guest-kernel-bangbang-grant:evidence-guest-serial";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProbeError {
    stage: ProbeStage,
    kind: io::ErrorKind,
}

/// Result of the feature-only process-entry probe.
pub(crate) enum ProbeEntry {
    /// A historical mode or a failed continuation terminates at the probe boundary.
    Terminal(ExitCode),
    /// The same process and transports continue into the ordinary lifecycle.
    Continue(RuntimeContinuation),
}

/// Single-use authority carried from credential transition into contained bootstrap.
pub(crate) struct RuntimeContinuation {
    pub(crate) stream: UnixStream,
    pub(crate) grants: UnixDatagram,
    pub(crate) namespace: WorkerNamespace,
    pub(crate) expected_session: bangbang_session::SessionId,
    pub(crate) bootstrap: ProbeBootstrap,
    pub(crate) parent: libc::pid_t,
    worker_args: Option<Vec<OsString>>,
}

impl std::fmt::Debug for RuntimeContinuation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RuntimeContinuation(<redacted>)")
    }
}

impl RuntimeContinuation {
    pub(crate) fn take_worker_args(&mut self) -> Result<Vec<OsString>, ()> {
        self.worker_args.take().ok_or(())
    }

    pub(crate) fn verify_witness(&self) -> Result<(), ()> {
        verify_runtime_witness(&self.stream, &self.grants, self.bootstrap, self.parent)
    }
}

pub(crate) fn is_requested() -> bool {
    env::args_os()
        .nth(1)
        .is_some_and(|argument| argument == OsStr::new(WORKER_ACTIVATION))
}

pub(crate) fn run() -> ProbeEntry {
    let (mut stream, bootstrap, parent, worker_args) = match probe_session() {
        Ok(session) => session,
        Err(_) => return ProbeEntry::Terminal(ExitCode::FAILURE),
    };
    if bootstrap.mode().is_credential_pair() {
        return run_credential_pair(stream, bootstrap, parent, worker_args);
    }
    let outcome = execute(bootstrap);
    let result = match outcome {
        Ok(()) => ProbeResult::success(bootstrap.mode(), bootstrap.nonce()),
        Err(error) => ProbeResult::failure(
            bootstrap.mode(),
            bootstrap.nonce(),
            error.stage,
            ProbeErrorCategory::from_io_kind(error.kind),
        ),
    };
    let Ok(result) = result else {
        return ProbeEntry::Terminal(ExitCode::FAILURE);
    };
    let encoded = result.encode();
    debug_assert_eq!(encoded.len(), RESULT_RECORD_BYTES);
    if stream.write_all(&encoded).is_err() {
        return ProbeEntry::Terminal(ExitCode::FAILURE);
    }
    ProbeEntry::Terminal(if outcome.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn probe_session() -> Result<(UnixStream, ProbeBootstrap, libc::pid_t, Vec<OsString>), ProbeError> {
    let arguments = env::args_os().skip(1).collect::<Vec<_>>();
    if arguments
        .first()
        .is_none_or(|arg| arg != OsStr::new(WORKER_ACTIVATION))
    {
        return Err(invalid(ProbeStage::InitialIdentity));
    }
    let value = env::var_os(SESSION_ENV_KEY).ok_or_else(|| invalid(ProbeStage::InitialIdentity))?;
    // SAFETY: This runs at process entry before application threads exist and
    // consumes the private marker before any later child could inherit it.
    unsafe { env::remove_var(SESSION_ENV_KEY) };
    if value != OsStr::new(SESSION_ENV_VALUE) {
        return Err(invalid(ProbeStage::InitialIdentity));
    }
    bangbang_session::macos::set_cloexec(SESSION_FD)
        .map_err(|error| with_kind(ProbeStage::InitialIdentity, error.kind()))?;
    // SAFETY: The validated production spawn contract transfers fixed fd 3
    // exactly once to this process.
    let owned = unsafe { OwnedFd::from_raw_fd(SESSION_FD) };
    let mut stream = UnixStream::from(owned);
    // SAFETY: `getppid` has no pointer or ownership contract.
    let parent = unsafe { libc::getppid() };
    bangbang_session::macos::verify_peer(stream.as_raw_fd(), parent)
        .map_err(|_| permission(ProbeStage::InitialIdentity))?;
    stream
        .write_all(&READY_RECORD)
        .map_err(|error| with_kind(ProbeStage::InitialIdentity, error.kind()))?;
    let mut encoded = [0_u8; BOOTSTRAP_RECORD_BYTES];
    stream
        .read_exact(&mut encoded)
        .map_err(|error| with_kind(ProbeStage::InitialIdentity, error.kind()))?;
    let bootstrap =
        ProbeBootstrap::decode(&encoded).map_err(|_| invalid(ProbeStage::InitialIdentity))?;
    let worker_args = arguments.into_iter().skip(1).collect::<Vec<_>>();
    if !bootstrap.mode().continues_runtime() && !worker_args.is_empty() {
        return Err(invalid(ProbeStage::InitialIdentity));
    }
    Ok((stream, bootstrap, parent, worker_args))
}

fn run_credential_pair(
    mut stream: UnixStream,
    bootstrap: ProbeBootstrap,
    parent: libc::pid_t,
    worker_args: Vec<OsString>,
) -> ProbeEntry {
    std::hint::black_box(CREDENTIAL_WORKER_ARTIFACT);
    std::hint::black_box(RUNTIME_WORKER_ARTIFACT);
    std::hint::black_box(RUNTIME_WORKER_BOUNDARY_ARTIFACT);
    std::hint::black_box(GUEST_WORKER_BOUNDARY_ARTIFACT);
    if stream.set_read_timeout(Some(CREDENTIAL_TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(CREDENTIAL_TIMEOUT)).is_err()
    {
        return terminal(ExitCode::FAILURE);
    }
    if let Err(error) = validate_initial_identity() {
        return terminal(write_credential_failure(
            &mut stream,
            bootstrap,
            CredentialFailureValue::new(
                CredentialStep::InitialIdentity,
                ProbeErrorCategory::from_io_kind(error.kind),
                CredentialPrefix::None,
                initial_credential_state(bootstrap),
            ),
            PeerObservation::NONE,
        ));
    }
    let runtime_root_descriptor = match take_credential_root(bootstrap) {
        Ok(root) => root,
        Err(error) => {
            return terminal(write_credential_failure(
                &mut stream,
                bootstrap,
                CredentialFailureValue::new(
                    CredentialStep::InitialIdentity,
                    ProbeErrorCategory::from_io_kind(error.kind),
                    CredentialPrefix::None,
                    initial_credential_state(bootstrap),
                ),
                PeerObservation::NONE,
            ));
        }
    };
    let grants = match take_grants() {
        Ok(grants) => grants,
        Err(category) => {
            return terminal(write_credential_failure(
                &mut stream,
                bootstrap,
                credential_failure(
                    CredentialStep::Protocol,
                    category,
                    CredentialPrefix::None,
                    initial_credential_state(bootstrap),
                ),
                PeerObservation::NONE,
            ));
        }
    };
    if grants.set_read_timeout(Some(CREDENTIAL_TIMEOUT)).is_err()
        || grants.set_write_timeout(Some(CREDENTIAL_TIMEOUT)).is_err()
    {
        return terminal(write_credential_failure(
            &mut stream,
            bootstrap,
            credential_failure(
                CredentialStep::Protocol,
                ProbeErrorCategory::Other,
                CredentialPrefix::None,
                initial_credential_state(bootstrap),
            ),
            PeerObservation::NONE,
        ));
    }
    match receive_credential_datagram(&grants) {
        Ok(proof)
            if proof.matches_expected(
                bootstrap.mode(),
                CredentialDatagramPhase::Challenge,
                CredentialRole::Launcher,
                bootstrap.nonce(),
            ) => {}
        Ok(_) | Err(_) => {
            return terminal(write_credential_failure(
                &mut stream,
                bootstrap,
                credential_failure(
                    CredentialStep::Protocol,
                    ProbeErrorCategory::InvalidInput,
                    CredentialPrefix::None,
                    initial_credential_state(bootstrap),
                ),
                PeerObservation::NONE,
            ));
        }
    }
    let Ok(worker_ready) =
        CredentialDatagramProof::worker_ready(bootstrap.mode(), bootstrap.nonce())
    else {
        return terminal(ExitCode::FAILURE);
    };
    if send_credential_datagram(&grants, worker_ready).is_err() {
        return terminal(ExitCode::FAILURE);
    }
    match receive_credential_datagram(&grants) {
        Ok(proof)
            if proof.matches_expected(
                bootstrap.mode(),
                CredentialDatagramPhase::LauncherRelease,
                CredentialRole::Launcher,
                bootstrap.nonce(),
            ) => {}
        Ok(_) | Err(_) => {
            return terminal(write_credential_failure(
                &mut stream,
                bootstrap,
                credential_failure(
                    CredentialStep::Protocol,
                    ProbeErrorCategory::InvalidInput,
                    CredentialPrefix::None,
                    initial_credential_state(bootstrap),
                ),
                PeerObservation::NONE,
            ));
        }
    }
    let runtime_root = match runtime_root_descriptor
        .map(|root| {
            ExplicitRuntimeRoot::from_owned_fd(
                root,
                bootstrap.root(),
                bootstrap.target_uid(),
                bootstrap.target_gid(),
                true,
            )
            .map_err(|_| permission(ProbeStage::ValidateRoot))
        })
        .transpose()
    {
        Ok(root) => root,
        Err(error) => {
            return terminal(write_credential_failure(
                &mut stream,
                bootstrap,
                CredentialFailureValue::new(
                    CredentialStep::InitialIdentity,
                    ProbeErrorCategory::from_io_kind(error.kind),
                    CredentialPrefix::None,
                    initial_credential_state(bootstrap),
                ),
                PeerObservation::NONE,
            ));
        }
    };
    let (initial, baseline) = match bangbang_session::elevated_credential::observe_initial_peer(
        stream.as_raw_fd(),
        grants.as_raw_fd(),
        parent,
        parent,
        bootstrap.target_uid(),
        bootstrap.target_gid(),
    ) {
        Ok(observation) => observation,
        Err(category) => {
            return terminal(write_credential_failure(
                &mut stream,
                bootstrap,
                credential_failure(
                    CredentialStep::PeerObservation,
                    category,
                    CredentialPrefix::None,
                    initial_credential_state(bootstrap),
                ),
                PeerObservation::NONE,
            ));
        }
    };
    let transition = match bangbang_session::elevated_credential::transition_process(
        bootstrap.mode(),
        bootstrap.target_uid(),
        bootstrap.target_gid(),
    ) {
        Ok(transition) => transition,
        Err(failure) => {
            return terminal(write_credential_failure(
                &mut stream,
                bootstrap,
                failure,
                initial,
            ));
        }
    };
    let after_worker = match bangbang_session::elevated_credential::observe_later_peer(
        stream.as_raw_fd(),
        grants.as_raw_fd(),
        parent,
        parent,
        bootstrap.target_uid(),
        bootstrap.target_gid(),
        &baseline,
    ) {
        Ok(observation) => observation,
        Err(category) => {
            return terminal(write_credential_failure(
                &mut stream,
                bootstrap,
                credential_failure(
                    CredentialStep::PeerObservation,
                    category,
                    transition.prefix(),
                    transition.state(),
                ),
                initial,
            ));
        }
    };
    let Ok(record) = CredentialRecord::worker_transitioned(
        bootstrap.mode(),
        transition.state(),
        initial,
        after_worker,
        bootstrap.nonce(),
    ) else {
        return terminal(ExitCode::FAILURE);
    };
    if stream.write_all(&record.encode()).is_err() {
        return terminal(ExitCode::FAILURE);
    }

    let launcher = match read_credential_record(&mut stream) {
        Ok(record)
            if record.matches_exchange(
                bootstrap.mode(),
                CredentialRole::Launcher,
                bootstrap.nonce(),
            ) =>
        {
            record
        }
        Ok(_) | Err(_) => {
            return terminal(write_credential_failure(
                &mut stream,
                bootstrap,
                credential_failure(
                    CredentialStep::Protocol,
                    ProbeErrorCategory::InvalidInput,
                    transition.prefix(),
                    transition.state(),
                ),
                initial,
            ));
        }
    };
    match launcher.kind() {
        CredentialRecordKind::Failure => return terminal(ExitCode::FAILURE),
        CredentialRecordKind::LauncherTransitioned => {}
        CredentialRecordKind::WorkerTransitioned | CredentialRecordKind::WorkerFinal => {
            return terminal(write_credential_failure(
                &mut stream,
                bootstrap,
                credential_failure(
                    CredentialStep::Protocol,
                    ProbeErrorCategory::InvalidInput,
                    transition.prefix(),
                    transition.state(),
                ),
                initial,
            ));
        }
    }
    let after_both = match bangbang_session::elevated_credential::observe_later_peer(
        stream.as_raw_fd(),
        grants.as_raw_fd(),
        parent,
        parent,
        bootstrap.target_uid(),
        bootstrap.target_gid(),
        &baseline,
    ) {
        Ok(observation) => observation,
        Err(category) => {
            return terminal(write_credential_failure(
                &mut stream,
                bootstrap,
                credential_failure(
                    CredentialStep::PeerObservation,
                    category,
                    transition.prefix(),
                    transition.state(),
                ),
                initial,
            ));
        }
    };
    let Ok(final_record) = CredentialRecord::worker_final(
        bootstrap.mode(),
        transition.state(),
        [initial, after_worker, after_both],
        bootstrap.nonce(),
    ) else {
        return terminal(ExitCode::FAILURE);
    };
    if stream.write_all(&final_record.encode()).is_err() {
        return terminal(ExitCode::FAILURE);
    }
    if !bootstrap.mode().continues_runtime() {
        return terminal(ExitCode::SUCCESS);
    }
    let Some(root) = runtime_root else {
        return terminal(ExitCode::FAILURE);
    };
    if bootstrap.fault() == RuntimeFault::PreAck {
        return terminal(ExitCode::FAILURE);
    }
    let mut encoded = [0_u8; CONTINUATION_ACK_BYTES];
    if stream.read_exact(&mut encoded).is_err() {
        return terminal(ExitCode::FAILURE);
    }
    let Ok(ack) = ContinuationAck::decode(&encoded) else {
        return terminal(ExitCode::FAILURE);
    };
    if !ack.matches_expected(bootstrap.mode(), bootstrap.nonce())
        || stream.set_read_timeout(None).is_err()
        || stream.set_write_timeout(None).is_err()
        || grants.set_read_timeout(None).is_err()
        || grants.set_write_timeout(None).is_err()
        || !transport_is_empty(stream.as_raw_fd())
    {
        return terminal(ExitCode::FAILURE);
    }
    if bootstrap.fault() == RuntimeFault::PostAck {
        return terminal(ExitCode::FAILURE);
    }
    if verify_runtime_witness(&stream, &grants, bootstrap, parent).is_err() {
        return terminal_runtime_failure(
            ProbeStage::RuntimeAuthorityReceive,
            ProbeErrorCategory::PermissionDenied,
        );
    }
    if grants.set_read_timeout(Some(CREDENTIAL_TIMEOUT)).is_err() {
        return terminal_runtime_failure(
            ProbeStage::RuntimeAuthorityReceive,
            ProbeErrorCategory::Other,
        );
    }
    let received = match receive_runtime_session_authority(&grants) {
        Ok(received) => received,
        Err(error) => {
            return terminal_runtime_failure(
                ProbeStage::RuntimeAuthorityReceive,
                runtime_transport_category(error),
            );
        }
    };
    if bootstrap.fault() == RuntimeFault::AuthorityReceive {
        return terminal_runtime_failure(
            ProbeStage::RuntimeAuthorityReceive,
            ProbeErrorCategory::Other,
        );
    }
    if bootstrap.fault() == RuntimeFault::AuthorityValidate {
        return terminal_runtime_failure(
            ProbeStage::RuntimeAuthorityValidate,
            ProbeErrorCategory::Other,
        );
    }
    let authority = received.authority;
    let expected_session = authority.session();
    if !authority.matches_expected(bootstrap, expected_session, authority.session_identity()) {
        return terminal_runtime_failure(
            ProbeStage::RuntimeAuthorityValidate,
            ProbeErrorCategory::InvalidInput,
        );
    }
    let validated = match ValidatedWorkerNamespace::from_explicit_root(
        root,
        received.descriptor,
        expected_session,
        authority.session_identity(),
    ) {
        Ok(validated) => validated,
        Err(error) => {
            return terminal_runtime_failure(
                ProbeStage::RuntimeAuthorityValidate,
                runtime_namespace_category(error),
            );
        }
    };
    if bootstrap.fault() == RuntimeFault::SessionLock {
        return terminal_runtime_failure(ProbeStage::RuntimeSessionLock, ProbeErrorCategory::Other);
    }
    let namespace = match validated.lock() {
        Ok(namespace) => namespace,
        Err(error) => {
            return terminal_runtime_failure(
                ProbeStage::RuntimeSessionLock,
                runtime_namespace_category(error),
            );
        }
    };
    if grants.set_read_timeout(None).is_err() || !transport_is_empty(grants.as_raw_fd()) {
        return terminal_runtime_failure(
            ProbeStage::RuntimeAuthorityValidate,
            ProbeErrorCategory::InvalidInput,
        );
    }
    let continuation = RuntimeContinuation {
        stream,
        grants,
        namespace,
        expected_session,
        bootstrap,
        parent,
        worker_args: Some(worker_args),
    };
    if continuation.verify_witness().is_err() {
        terminal_runtime_failure(
            ProbeStage::RuntimeAuthorityValidate,
            ProbeErrorCategory::PermissionDenied,
        )
    } else {
        ProbeEntry::Continue(continuation)
    }
}

const fn terminal(code: ExitCode) -> ProbeEntry {
    ProbeEntry::Terminal(code)
}

fn terminal_runtime_failure(stage: ProbeStage, category: ProbeErrorCategory) -> ProbeEntry {
    let code = RuntimeWorkerFailure::new(stage, category).map_or(ExitCode::FAILURE, |failure| {
        ExitCode::from(failure.exit_code())
    });
    terminal(code)
}

fn runtime_transport_category(error: GrantTransportError) -> ProbeErrorCategory {
    match error {
        GrantTransportError::Io(kind) => ProbeErrorCategory::from_io_kind(kind),
        GrantTransportError::Invalid => ProbeErrorCategory::InvalidInput,
    }
}

fn runtime_namespace_category(error: RuntimeError) -> ProbeErrorCategory {
    match error {
        RuntimeError::Filesystem(kind) | RuntimeError::NamespaceCreate(kind) => {
            ProbeErrorCategory::from_io_kind(kind)
        }
        RuntimeError::InvalidRoot | RuntimeError::InvalidEntry | RuntimeError::Collision => {
            ProbeErrorCategory::InvalidInput
        }
    }
}

fn verify_runtime_witness(
    stream: &UnixStream,
    grants: &UnixDatagram,
    bootstrap: ProbeBootstrap,
    parent: libc::pid_t,
) -> Result<(), ()> {
    // SAFETY: `getppid` has no pointer or ownership contract.
    let live_parent = unsafe { libc::getppid() };
    if live_parent != parent
        || bangbang_session::macos::verify_peer_pid(stream.as_raw_fd(), parent).is_err()
        || bangbang_session::macos::verify_peer_pid(grants.as_raw_fd(), parent).is_err()
        || bangbang_session::elevated_credential::attest_current_process(
            bootstrap.mode(),
            bootstrap.target_uid(),
            bootstrap.target_gid(),
        )
        .is_err()
    {
        return Err(());
    }
    Ok(())
}

fn take_credential_root(bootstrap: ProbeBootstrap) -> Result<Option<OwnedFd>, ProbeError> {
    let root = take_root_descriptor()?;
    if bootstrap.mode().continues_runtime() {
        return Ok(Some(root));
    }
    validate_root(root.as_raw_fd(), bootstrap.root())?;
    Ok(None)
}

fn take_root(bootstrap: ProbeBootstrap) -> Result<OwnedFd, ProbeError> {
    let root = take_root_descriptor()?;
    validate_root(root.as_raw_fd(), bootstrap.root())?;
    Ok(root)
}

fn take_root_descriptor() -> Result<OwnedFd, ProbeError> {
    bangbang_session::macos::set_cloexec(ROOT_FD)
        .map_err(|error| with_kind(ProbeStage::TakeRoot, error.kind()))?;
    // SAFETY: The feature-gated production spawn contract transfers fixed fd 8
    // exactly once to this process.
    let root = unsafe { OwnedFd::from_raw_fd(ROOT_FD) };
    Ok(root)
}

fn transport_is_empty(fd: libc::c_int) -> bool {
    let mut byte = 0_u8;
    // SAFETY: `byte` is one writable byte, `fd` is a live connected local
    // socket, and PEEK never consumes application bytes.
    let result = unsafe {
        libc::recv(
            fd,
            (&raw mut byte).cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    if result < 0 {
        let error = io::Error::last_os_error();
        return error
            .raw_os_error()
            .is_some_and(|code| code == libc::EAGAIN || code == libc::EWOULDBLOCK);
    }
    false
}

fn take_grants() -> Result<UnixDatagram, ProbeErrorCategory> {
    bangbang_session::macos::set_cloexec(GRANT_FD)
        .map_err(|error| ProbeErrorCategory::from_io_kind(error.kind()))?;
    // SAFETY: The production spawn contract transfers fixed fd 4 exactly once
    // to this process, and credential mode consumes it instead of normal grants.
    let owned = unsafe { OwnedFd::from_raw_fd(GRANT_FD) };
    Ok(UnixDatagram::from(owned))
}

fn receive_credential_datagram(
    grants: &UnixDatagram,
) -> Result<CredentialDatagramProof, ProbeErrorCategory> {
    let mut encoded = [0_u8; CREDENTIAL_DATAGRAM_BYTES + 1];
    let length = grants
        .recv(&mut encoded)
        .map_err(|error| ProbeErrorCategory::from_io_kind(error.kind()))?;
    if length != CREDENTIAL_DATAGRAM_BYTES {
        return Err(ProbeErrorCategory::InvalidInput);
    }
    let exact = encoded[..CREDENTIAL_DATAGRAM_BYTES]
        .try_into()
        .map_err(|_| ProbeErrorCategory::InvalidInput)?;
    CredentialDatagramProof::decode(exact).map_err(|_| ProbeErrorCategory::InvalidInput)
}

fn send_credential_datagram(
    grants: &UnixDatagram,
    proof: CredentialDatagramProof,
) -> Result<(), ProbeErrorCategory> {
    let encoded = proof.encode();
    let length = grants
        .send(&encoded)
        .map_err(|error| ProbeErrorCategory::from_io_kind(error.kind()))?;
    if length == encoded.len() {
        Ok(())
    } else {
        Err(ProbeErrorCategory::InvalidInput)
    }
}

fn read_credential_record(stream: &mut UnixStream) -> Result<CredentialRecord, ()> {
    let mut encoded = [0_u8; CREDENTIAL_RECORD_BYTES];
    stream.read_exact(&mut encoded).map_err(|_| ())?;
    CredentialRecord::decode(&encoded).map_err(|_| ())
}

fn write_credential_failure(
    stream: &mut UnixStream,
    bootstrap: ProbeBootstrap,
    failure: CredentialFailureValue,
    initial: PeerObservation,
) -> ExitCode {
    let Ok(record) = CredentialRecord::failure(
        bootstrap.mode(),
        CredentialRole::Worker,
        failure,
        initial,
        bootstrap.nonce(),
    ) else {
        return ExitCode::FAILURE;
    };
    let _ = stream.write_all(&record.encode());
    ExitCode::FAILURE
}

const fn credential_failure(
    step: CredentialStep,
    category: ProbeErrorCategory,
    prefix: CredentialPrefix,
    state: CredentialSelfState,
) -> CredentialFailureValue {
    CredentialFailureValue::new(step, category, prefix, state)
}

const fn initial_credential_state(bootstrap: ProbeBootstrap) -> CredentialSelfState {
    let identity = if bootstrap.mode().retains_root() {
        CredentialIdentityClass::InitialAndTarget
    } else {
        CredentialIdentityClass::InitialRoot
    };
    CredentialSelfState::new(identity, CredentialGroupClass::Other)
}

fn execute(config: ProbeBootstrap) -> Result<(), ProbeError> {
    validate_initial_identity()?;
    let root = take_root(config)?;
    match config.mode() {
        bangbang_session::elevated_probe::ProbeMode::HvfControl => {
            drop(root);
            return run_hvf_control();
        }
        bangbang_session::elevated_probe::ProbeMode::InheritedRoot => {
            validate_inherited_root(config.root())?;
            drop(root);
            validate_sandbox_chroot_control()?;
            return run_hvf_control();
        }
        bangbang_session::elevated_probe::ProbeMode::Drop
        | bangbang_session::elevated_probe::ProbeMode::RetainRoot
        | bangbang_session::elevated_probe::ProbeMode::UnmappedSyscall => {}
        bangbang_session::elevated_probe::ProbeMode::Control
        | bangbang_session::elevated_probe::ProbeMode::CredentialDrop
        | bangbang_session::elevated_probe::ProbeMode::CredentialRetainRoot
        | bangbang_session::elevated_probe::ProbeMode::CredentialUnmapped
        | bangbang_session::elevated_probe::ProbeMode::CredentialControl
        | bangbang_session::elevated_probe::ProbeMode::RuntimeDrop
        | bangbang_session::elevated_probe::ProbeMode::RuntimeRetainRoot
        | bangbang_session::elevated_probe::ProbeMode::RuntimeUnmapped
        | bangbang_session::elevated_probe::ProbeMode::GuestNoApiDrop
        | bangbang_session::elevated_probe::ProbeMode::GuestNoApiRetainRoot
        | bangbang_session::elevated_probe::ProbeMode::GuestNoApiUnmapped
        | bangbang_session::elevated_probe::ProbeMode::GuestApiDrop
        | bangbang_session::elevated_probe::ProbeMode::GuestApiRetainRoot
        | bangbang_session::elevated_probe::ProbeMode::GuestApiUnmapped => {
            return Err(invalid(ProbeStage::InitialIdentity));
        }
    }
    // SAFETY: `root` is the live, validated private directory descriptor.
    syscall(ProbeStage::EnterRoot, unsafe {
        libc::fchdir(root.as_raw_fd())
    })?;
    // SAFETY: The current directory is the retained exact root and the fixed
    // relative path contains no attacker-controlled bytes.
    syscall(ProbeStage::Chroot, unsafe { libc::chroot(c".".as_ptr()) })?;
    // SAFETY: The process has entered the private root and the fixed absolute
    // path is NUL-terminated.
    syscall(ProbeStage::ChangeDirectory, unsafe {
        libc::chdir(c"/".as_ptr())
    })?;
    drop(root);
    Err(ProbeError {
        stage: ProbeStage::UnexpectedContinuation,
        kind: io::ErrorKind::Other,
    })
}

fn validate_inherited_root(expected: ObjectIdentity) -> Result<(), ProbeError> {
    let slash = open_directory(c"/")?;
    validate_root(slash.as_raw_fd(), expected).map_err(|error| ProbeError {
        stage: ProbeStage::InheritedRoot,
        kind: error.kind,
    })?;
    let cwd = open_directory(c".")?;
    validate_root(cwd.as_raw_fd(), expected).map_err(|error| ProbeError {
        stage: ProbeStage::InheritedRoot,
        kind: error.kind,
    })
}

fn validate_sandbox_chroot_control() -> Result<(), ProbeError> {
    validate_sandbox_chroot_control_with(|| {
        // SAFETY: Cwd is the already inherited exact root and the fixed
        // relative string is NUL-terminated. Success would retain the same
        // root but violate the expected App Sandbox denial established by the
        // signed control.
        if unsafe { libc::chroot(c".".as_ptr()) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error().kind())
        }
    })
}

fn validate_sandbox_chroot_control_with<F>(chroot: F) -> Result<(), ProbeError>
where
    F: FnOnce() -> Result<(), io::ErrorKind>,
{
    match chroot() {
        Ok(()) => Err(ProbeError {
            stage: ProbeStage::UnexpectedContinuation,
            kind: io::ErrorKind::Other,
        }),
        Err(io::ErrorKind::PermissionDenied) => Ok(()),
        Err(kind) => Err(with_kind(ProbeStage::SandboxChrootControl, kind)),
    }
}

fn run_hvf_control() -> Result<(), ProbeError> {
    let mut backend = HvfBackend::new();
    run_hvf_control_with(&mut backend)
}

trait HvfControl {
    fn create(&mut self) -> Result<(), ()>;
    fn destroy(&mut self) -> Result<(), ()>;
}

impl HvfControl for HvfBackend {
    fn create(&mut self) -> Result<(), ()> {
        self.create_vm().map_err(|_| ())
    }

    fn destroy(&mut self) -> Result<(), ()> {
        self.destroy_vm().map_err(|_| ())
    }
}

fn run_hvf_control_with<B: HvfControl>(backend: &mut B) -> Result<(), ProbeError> {
    backend.create().map_err(|()| ProbeError {
        stage: ProbeStage::HvfCreate,
        kind: io::ErrorKind::Other,
    })?;
    backend.destroy().map_err(|()| ProbeError {
        stage: ProbeStage::HvfDestroy,
        kind: io::ErrorKind::Other,
    })
}

fn open_directory(path: &std::ffi::CStr) -> Result<OwnedFd, ProbeError> {
    // SAFETY: `path` is NUL-terminated, fixed by the caller, and no pointer is
    // retained by `open`.
    let descriptor = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if descriptor < 0 {
        Err(last(ProbeStage::InheritedRoot))
    } else {
        // SAFETY: `descriptor` is a fresh successful result owned by this scope.
        Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
    }
}

fn validate_initial_identity() -> Result<(), ProbeError> {
    // SAFETY: Credential getters have no pointer or ownership contract.
    let identities = unsafe {
        (
            libc::getuid(),
            libc::geteuid(),
            libc::getgid(),
            libc::getegid(),
        )
    };
    if identities == (0, 0, 0, 0) {
        Ok(())
    } else {
        Err(permission(ProbeStage::InitialIdentity))
    }
}

fn validate_root(descriptor: libc::c_int, expected: ObjectIdentity) -> Result<(), ProbeError> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `descriptor` is live and `stat` is writable for one result.
    if unsafe { libc::fstat(descriptor, stat.as_mut_ptr()) } != 0 {
        return Err(last(ProbeStage::ValidateRoot));
    }
    // SAFETY: Successful `fstat` initialized the complete value.
    let stat = unsafe { stat.assume_init() };
    validate_root_stat(&stat, expected)
}

fn validate_root_stat(stat: &libc::stat, expected: ObjectIdentity) -> Result<(), ProbeError> {
    let actual = ObjectIdentity {
        device: u64::from(u32::from_ne_bytes(stat.st_dev.to_ne_bytes())),
        inode: stat.st_ino,
    };
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR
        || stat.st_mode & 0o7777 != 0o700
        || stat.st_uid != 0
        || stat.st_gid != 0
        || stat.st_nlink < 2
        || actual != expected
    {
        return Err(permission(ProbeStage::ValidateRoot));
    }
    Ok(())
}

fn syscall(stage: ProbeStage, status: libc::c_int) -> Result<(), ProbeError> {
    if status == 0 {
        Ok(())
    } else {
        Err(last(stage))
    }
}

fn last(stage: ProbeStage) -> ProbeError {
    with_kind(stage, io::Error::last_os_error().kind())
}

const fn with_kind(stage: ProbeStage, kind: io::ErrorKind) -> ProbeError {
    ProbeError { stage, kind }
}

const fn invalid(stage: ProbeStage) -> ProbeError {
    ProbeError {
        stage,
        kind: io::ErrorKind::InvalidInput,
    }
}

const fn permission(stage: ProbeStage) -> ProbeError {
    ProbeError {
        stage,
        kind: io::ErrorKind::PermissionDenied,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeHvfControl {
        calls: Vec<&'static str>,
        create_result: Result<(), ()>,
        destroy_result: Result<(), ()>,
    }

    impl HvfControl for FakeHvfControl {
        fn create(&mut self) -> Result<(), ()> {
            self.calls.push("create");
            self.create_result
        }

        fn destroy(&mut self) -> Result<(), ()> {
            self.calls.push("destroy");
            self.destroy_result
        }
    }

    #[test]
    fn error_mapping_is_value_free() {
        for (kind, expected) in [
            (
                io::ErrorKind::PermissionDenied,
                ProbeErrorCategory::PermissionDenied,
            ),
            (
                io::ErrorKind::InvalidInput,
                ProbeErrorCategory::InvalidInput,
            ),
            (io::ErrorKind::NotFound, ProbeErrorCategory::Other),
        ] {
            assert_eq!(ProbeErrorCategory::from_io_kind(kind), expected);
        }
    }

    #[test]
    fn sandbox_chroot_control_accepts_only_permission_denied() {
        validate_sandbox_chroot_control_with(|| Err(io::ErrorKind::PermissionDenied))
            .expect("the signed denial control should pass");

        let continuation = validate_sandbox_chroot_control_with(|| Ok(()))
            .expect_err("unexpected chroot success should fail closed");
        assert_eq!(continuation.stage, ProbeStage::UnexpectedContinuation);
        assert_eq!(continuation.kind, io::ErrorKind::Other);

        let other = validate_sandbox_chroot_control_with(|| Err(io::ErrorKind::NotFound))
            .expect_err("a different failure class should remain distinct");
        assert_eq!(other.stage, ProbeStage::SandboxChrootControl);
        assert_eq!(other.kind, io::ErrorKind::NotFound);
    }

    #[test]
    fn inherited_root_stat_requires_the_exact_closed_identity_shape() {
        let root = std::fs::File::open("/").expect("test root descriptor should open");
        let mut stat = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `root` is live and `stat` is writable for one result.
        let status = unsafe { libc::fstat(root.as_raw_fd(), stat.as_mut_ptr()) };
        assert_eq!(status, 0);
        // SAFETY: Successful `fstat` initialized the complete value.
        let mut stat = unsafe { stat.assume_init() };
        stat.st_mode = libc::S_IFDIR | 0o700;
        stat.st_uid = 0;
        stat.st_gid = 0;
        stat.st_nlink = 2;
        let expected = ObjectIdentity {
            device: u64::from(u32::from_ne_bytes(stat.st_dev.to_ne_bytes())),
            inode: stat.st_ino,
        };
        validate_root_stat(&stat, expected).expect("the exact inherited root should pass");

        let mut wrong_type = stat;
        wrong_type.st_mode = libc::S_IFREG | 0o700;
        let mut wrong_mode = stat;
        wrong_mode.st_mode = libc::S_IFDIR | 0o755;
        let mut wrong_uid = stat;
        wrong_uid.st_uid = 1;
        let mut wrong_gid = stat;
        wrong_gid.st_gid = 1;
        let mut wrong_links = stat;
        wrong_links.st_nlink = 1;
        for invalid in [wrong_type, wrong_mode, wrong_uid, wrong_gid, wrong_links] {
            assert_eq!(
                validate_root_stat(&invalid, expected),
                Err(permission(ProbeStage::ValidateRoot))
            );
        }
        assert_eq!(
            validate_root_stat(
                &stat,
                ObjectIdentity {
                    device: expected.device,
                    inode: expected.inode ^ 1,
                }
            ),
            Err(permission(ProbeStage::ValidateRoot))
        );
    }

    #[test]
    fn hvf_control_destroys_exactly_after_successful_create() {
        let mut success = FakeHvfControl {
            calls: Vec::new(),
            create_result: Ok(()),
            destroy_result: Ok(()),
        };
        run_hvf_control_with(&mut success).expect("create and destroy should succeed");
        assert_eq!(success.calls, ["create", "destroy"]);

        let mut create_failure = FakeHvfControl {
            calls: Vec::new(),
            create_result: Err(()),
            destroy_result: Ok(()),
        };
        let failure = run_hvf_control_with(&mut create_failure)
            .expect_err("create failure should stop the sequence");
        assert_eq!(failure.stage, ProbeStage::HvfCreate);
        assert_eq!(create_failure.calls, ["create"]);

        let mut destroy_failure = FakeHvfControl {
            calls: Vec::new(),
            create_result: Ok(()),
            destroy_result: Err(()),
        };
        let failure = run_hvf_control_with(&mut destroy_failure)
            .expect_err("destroy failure should be reported");
        assert_eq!(failure.stage, ProbeStage::HvfDestroy);
        assert_eq!(destroy_failure.calls, ["create", "destroy"]);
    }

    #[test]
    fn credential_transport_rejects_disconnect_timeout_and_wrong_length() {
        let (mut stream, peer) = UnixStream::pair().expect("stream pair should construct");
        drop(peer);
        assert!(
            read_credential_record(&mut stream).is_err(),
            "stream endpoint death must not produce a phase record"
        );

        let (datagram, peer) = UnixDatagram::pair().expect("datagram pair should construct");
        peer.send(&[0; CREDENTIAL_DATAGRAM_BYTES - 1])
            .expect("short datagram should send");
        assert_eq!(
            receive_credential_datagram(&datagram),
            Err(ProbeErrorCategory::InvalidInput)
        );

        let (datagram, peer) = UnixDatagram::pair().expect("datagram pair should construct");
        datagram
            .set_read_timeout(Some(Duration::from_millis(10)))
            .expect("datagram timeout should configure");
        drop(peer);
        assert!(
            receive_credential_datagram(&datagram).is_err(),
            "a dead datagram endpoint must fail or time out without advancing"
        );
    }

    #[test]
    fn continuation_requires_empty_live_stream_and_datagram_transports() {
        let (mut stream, mut stream_peer) =
            UnixStream::pair().expect("stream pair should construct");
        assert!(transport_is_empty(stream.as_raw_fd()));
        stream_peer
            .write_all(&[0x41, 0x42])
            .expect("replay bytes should send");
        assert!(!transport_is_empty(stream.as_raw_fd()));
        let mut byte = [0_u8; 1];
        stream
            .read_exact(&mut byte)
            .expect("first replay byte should consume");
        assert!(!transport_is_empty(stream.as_raw_fd()));
        stream
            .read_exact(&mut byte)
            .expect("second replay byte should consume");
        assert!(transport_is_empty(stream.as_raw_fd()));
        drop(stream_peer);
        assert!(
            !transport_is_empty(stream.as_raw_fd()),
            "EOF must not be mistaken for an empty live transport"
        );

        let (datagram, datagram_peer) =
            UnixDatagram::pair().expect("datagram pair should construct");
        assert!(transport_is_empty(datagram.as_raw_fd()));
        datagram_peer
            .send(&[0x51])
            .expect("first datagram should send");
        datagram_peer
            .send(&[0x52])
            .expect("second datagram should send");
        assert!(!transport_is_empty(datagram.as_raw_fd()));
        datagram
            .recv(&mut byte)
            .expect("first datagram should consume");
        assert!(!transport_is_empty(datagram.as_raw_fd()));
        datagram
            .recv(&mut byte)
            .expect("second datagram should consume");
        assert!(transport_is_empty(datagram.as_raw_fd()));
        drop(datagram_peer);
        assert!(
            !transport_is_empty(datagram.as_raw_fd()),
            "dead datagram transport must fail closed"
        );
    }
}
