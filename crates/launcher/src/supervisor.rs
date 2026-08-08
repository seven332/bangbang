use std::ffi::OsString;
#[cfg(any(target_os = "macos", test))]
use std::os::unix::process::ExitStatusExt;

#[cfg(target_os = "macos")]
use crate::BundleLayout;
use crate::LauncherError;

/// Final process result returned by the production launcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LauncherExit(u8);

impl LauncherExit {
    /// Returns the exact launcher process exit value.
    #[must_use]
    pub const fn code(self) -> u8 {
        self.0
    }
}

/// Validates and launches the one embedded worker with the supplied argument bytes.
pub fn launch_embedded_worker<I>(args: I) -> Result<LauncherExit, LauncherError>
where
    I: IntoIterator<Item = OsString>,
{
    #[cfg(target_os = "macos")]
    {
        let args = args.into_iter().collect::<Vec<_>>();
        #[cfg(feature = "elevated-bootstrap-probe")]
        let (elevated_probe, args) = crate::elevated_probe::Config::parse(args)?;
        let child_bootstrap = crate::macos::daemon::child_bootstrap()?;
        let timing = child_bootstrap
            .as_ref()
            .map_or_else(crate::launch_policy::LaunchTiming::sample, |bootstrap| {
                Ok(bootstrap.timing)
            })?;
        let command = crate::launch_policy::LaunchCommand::parse(args)?;
        let request = match command {
            crate::launch_policy::LaunchCommand::Help => {
                print!("{}", crate::launch_policy::help());
                return Ok(LauncherExit(0));
            }
            crate::launch_policy::LaunchCommand::Version => {
                println!("Jailer v{}", env!("CARGO_PKG_VERSION"));
                return Ok(LauncherExit(0));
            }
            crate::launch_policy::LaunchCommand::Run(request) => request,
        };
        let executable = std::env::current_exe().map_err(|_| LauncherError::InvalidBundleLayout)?;
        let layout = BundleLayout::from_launcher_executable(&executable)?;
        let worker_profile = crate::macos::code_sign::validate_bundle(&layout)?;
        #[cfg(feature = "elevated-bootstrap-probe")]
        if elevated_probe.is_some() && (child_bootstrap.is_some() || request.requests_daemonize()) {
            return Err(LauncherError::InvalidLaunchPolicy);
        }
        #[cfg(feature = "elevated-bootstrap-probe")]
        if elevated_probe.as_ref().is_some_and(|probe| {
            probe.mode() == bangbang_session::elevated_probe::ProbeMode::Control
        }) {
            let probe = elevated_probe
                .as_ref()
                .ok_or(LauncherError::InvalidLaunchPolicy)?;
            probe.run_control()?;
            println!("status: elevated bootstrap control complete");
            return Ok(LauncherExit(0));
        }
        #[cfg(feature = "elevated-bootstrap-probe")]
        if elevated_probe.as_ref().is_some_and(|probe| {
            probe.mode() == bangbang_session::elevated_probe::ProbeMode::CredentialControl
        }) {
            let probe = elevated_probe
                .as_ref()
                .ok_or(LauncherError::InvalidLaunchPolicy)?;
            return match probe.run_credential_control() {
                Ok(transition) => {
                    println!(
                        "status: elevated credential credential-control complete prefix={} identity={} groups={}",
                        transition.prefix().name(),
                        transition.state().identity().name(),
                        transition.state().groups().name()
                    );
                    Ok(LauncherExit(0))
                }
                Err(failure) => Ok(credential_blocked_value(
                    bangbang_session::elevated_probe::CredentialRole::Launcher,
                    failure,
                )),
            };
        }
        if let Some(mut bootstrap) = child_bootstrap {
            let result = (|| {
                if !request.requests_daemonize()
                    || bootstrap.notifier.check_parent()?
                        != crate::macos::daemon::NotifierEvent::Pending
                {
                    return Err(LauncherError::DaemonHandoff);
                }
                let launch =
                    request.prepare(layout.worker_executable(), timing, true, worker_profile)?;
                if bootstrap.notifier.check_parent()?
                    != crate::macos::daemon::NotifierEvent::Pending
                {
                    return Err(LauncherError::DaemonHandoff);
                }
                launch_prepared(&layout, launch, Some(&mut bootstrap.notifier))
            })();
            if let Err(error) = result {
                bootstrap.notifier.notify_failure(error);
            }
            return result;
        }
        if request.requests_daemonize() {
            crate::macos::daemon::launch_parent(&request, timing, &executable, &layout)?;
            return Ok(LauncherExit(0));
        }
        let launch = request.prepare(layout.worker_executable(), timing, false, worker_profile)?;
        #[cfg(feature = "elevated-bootstrap-probe")]
        let mut launch = launch;
        #[cfg(feature = "elevated-bootstrap-probe")]
        if let Some(elevated_probe) = elevated_probe {
            elevated_probe.prepend_worker_activation(&mut launch.worker_args)?;
            return launch_elevated_prepared(&layout, launch, elevated_probe);
        }
        launch_prepared(&layout, launch, None)
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = args;
        Err(LauncherError::UnsupportedPlatform)
    }
}

#[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
fn launch_elevated_prepared(
    layout: &BundleLayout,
    launch: crate::launch_policy::PreparedLaunch,
    elevated_probe: crate::elevated_probe::Config,
) -> Result<LauncherExit, LauncherError> {
    let bootstrap = elevated_probe.bootstrap()?;
    if elevated_probe.mode() == bangbang_session::elevated_probe::ProbeMode::InheritedRoot {
        return launch_inherited_prepared(launch, elevated_probe, bootstrap);
    }
    let spawned = crate::macos::spawn::prepare_suspended_elevated(
        layout.worker_executable(),
        launch.worker_args,
        elevated_probe.root_fd(),
    )?
    .spawn()?;
    finish_elevated_exchange(spawned, launch.worker_profile, bootstrap, false)
}

#[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
fn launch_inherited_prepared(
    launch: crate::launch_policy::PreparedLaunch,
    elevated_probe: crate::elevated_probe::Config,
    bootstrap: bangbang_session::elevated_probe::ProbeBootstrap,
) -> Result<LauncherExit, LauncherError> {
    use bangbang_session::elevated_probe::{ProbeErrorCategory, ProbeStage};

    let staged_layout = match elevated_probe.staged_layout() {
        Ok(layout) => layout,
        Err(_) => {
            return Ok(elevated_blocked(
                ProbeStage::ValidateStagedBundle,
                ProbeErrorCategory::InvalidInput,
            ));
        }
    };
    let staged_profile = match crate::macos::code_sign::validate_bundle(&staged_layout) {
        Ok(profile) => profile,
        Err(_) => {
            return Ok(elevated_blocked(
                ProbeStage::ValidateStagedBundle,
                ProbeErrorCategory::Other,
            ));
        }
    };
    if staged_profile != launch.worker_profile {
        return Ok(elevated_blocked(
            ProbeStage::ValidateStagedBundle,
            ProbeErrorCategory::InvalidInput,
        ));
    }
    if elevated_probe.validate_staged_loader().is_err() {
        return Ok(elevated_blocked(
            ProbeStage::ValidateStagedLoader,
            ProbeErrorCategory::InvalidInput,
        ));
    }
    let worker = match elevated_probe.in_root_worker() {
        Ok(worker) => worker,
        Err(_) => {
            return Ok(elevated_blocked(
                ProbeStage::SpawnWorker,
                ProbeErrorCategory::InvalidInput,
            ));
        }
    };
    let prepared = match crate::macos::spawn::prepare_suspended_elevated(
        worker,
        launch.worker_args,
        elevated_probe.root_fd(),
    ) {
        Ok(prepared) => prepared,
        Err(error) => {
            return Ok(elevated_blocked(
                ProbeStage::SpawnWorker,
                probe_error_category(&error),
            ));
        }
    };
    if let Err(failure) = elevated_probe.enter_inherited_root() {
        return Ok(elevated_blocked(failure.stage, failure.category));
    }
    let spawned = match prepared.spawn() {
        Ok(spawned) => spawned,
        Err(error) => {
            return Ok(elevated_blocked(
                ProbeStage::SpawnWorker,
                probe_error_category(&error),
            ));
        }
    };
    finish_elevated_exchange(spawned, launch.worker_profile, bootstrap, true)
}

#[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
fn finish_elevated_exchange(
    mut spawned: crate::macos::spawn::SuspendedWorker,
    expected_profile: crate::macos::code_sign::WorkerProfile,
    bootstrap: bangbang_session::elevated_probe::ProbeBootstrap,
    inherited_root: bool,
) -> Result<LauncherExit, LauncherError> {
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::time::Duration;

    use bangbang_session::elevated_probe::{
        ProbeErrorCategory, ProbeResult, ProbeStage, READY_RECORD, RESULT_RECORD_BYTES,
    };

    const TIMEOUT: Duration = Duration::from_secs(5);

    match crate::macos::code_sign::validate_worker_process(spawned.worker.pid()) {
        Ok(profile) if profile == expected_profile => {}
        Ok(_) if inherited_root => {
            return Ok(elevated_blocked(
                ProbeStage::SuspendedIdentity,
                ProbeErrorCategory::InvalidInput,
            ));
        }
        Ok(_) => return Err(LauncherError::InvalidWorkerIdentity),
        Err(_) if inherited_root => {
            return Ok(elevated_blocked(
                ProbeStage::WorkerBootstrap,
                ProbeErrorCategory::Other,
            ));
        }
        Err(error) => return Err(error),
    }
    if let Err(error) = spawned.session.set_read_timeout(Some(TIMEOUT)) {
        if inherited_root {
            return Ok(elevated_blocked(
                ProbeStage::WorkerBootstrap,
                ProbeErrorCategory::from_io_kind(error.kind()),
            ));
        }
        return Err(LauncherError::SessionSetup(error.kind()));
    }
    if let Err(error) = spawned.session.set_write_timeout(Some(TIMEOUT)) {
        if inherited_root {
            return Ok(elevated_blocked(
                ProbeStage::WorkerBootstrap,
                ProbeErrorCategory::from_io_kind(error.kind()),
            ));
        }
        return Err(LauncherError::SessionSetup(error.kind()));
    }
    if let Err(error) = spawned.worker.resume() {
        if inherited_root {
            return Ok(elevated_blocked(
                ProbeStage::WorkerBootstrap,
                probe_error_category(&error),
            ));
        }
        return Err(error);
    }
    let mut ready = [0_u8; READY_RECORD.len()];
    if let Err(error) = spawned.session.read_exact(&mut ready) {
        if inherited_root {
            return Ok(elevated_blocked(
                ProbeStage::WorkerBootstrap,
                ProbeErrorCategory::from_io_kind(error.kind()),
            ));
        }
        return Err(LauncherError::SessionProtocol);
    }
    if ready != READY_RECORD {
        if inherited_root {
            return Ok(elevated_blocked(
                ProbeStage::WorkerBootstrap,
                ProbeErrorCategory::InvalidInput,
            ));
        }
        return Err(LauncherError::SessionProtocol);
    }
    if bangbang_session::macos::verify_peer(spawned.session.as_raw_fd(), spawned.worker.pid())
        .is_err()
    {
        if inherited_root {
            return Ok(elevated_blocked(
                ProbeStage::LiveIdentity,
                ProbeErrorCategory::Other,
            ));
        }
        return Err(LauncherError::InvalidWorkerIdentity);
    }
    match crate::macos::code_sign::validate_worker_process(spawned.worker.pid()) {
        Ok(profile) if profile == expected_profile => {}
        Ok(_) if inherited_root => {
            return Ok(elevated_blocked(
                ProbeStage::LiveIdentity,
                ProbeErrorCategory::InvalidInput,
            ));
        }
        Ok(_) => return Err(LauncherError::InvalidWorkerIdentity),
        Err(_) if inherited_root => {
            return Ok(elevated_blocked(
                ProbeStage::LiveIdentity,
                ProbeErrorCategory::Other,
            ));
        }
        Err(error) => return Err(error),
    }
    if spawned.session.write_all(&bootstrap.encode()).is_err() {
        if inherited_root {
            return Ok(elevated_blocked(
                ProbeStage::WorkerBootstrap,
                ProbeErrorCategory::Other,
            ));
        }
        return Err(LauncherError::SessionProtocol);
    }
    if bootstrap.mode().is_credential_pair() {
        let (initial, baseline) = match begin_credential_exchange(&mut spawned, bootstrap) {
            Ok(initial) => initial,
            Err(failure) => {
                return Ok(credential_blocked_value(
                    bangbang_session::elevated_probe::CredentialRole::Launcher,
                    failure,
                ));
            }
        };
        return finish_credential_exchange(spawned, bootstrap, initial, baseline);
    }
    let mut encoded_result = [0_u8; RESULT_RECORD_BYTES];
    if spawned.session.read_exact(&mut encoded_result).is_err() {
        if inherited_root {
            return Ok(elevated_blocked(
                ProbeStage::WorkerBootstrap,
                ProbeErrorCategory::Other,
            ));
        }
        return Err(LauncherError::SessionProtocol);
    }
    let result = match ProbeResult::decode(&encoded_result) {
        Ok(result) => result,
        Err(_) if inherited_root => {
            return Ok(elevated_blocked(
                ProbeStage::WorkerBootstrap,
                ProbeErrorCategory::InvalidInput,
            ));
        }
        Err(_) => return Err(LauncherError::SessionProtocol),
    };
    if result.mode() != bootstrap.mode() || result.nonce() != bootstrap.nonce() {
        if inherited_root {
            return Ok(elevated_blocked(
                ProbeStage::WorkerBootstrap,
                ProbeErrorCategory::InvalidInput,
            ));
        }
        return Err(LauncherError::SessionProtocol);
    }
    let status = match spawned.worker.wait() {
        Ok(status) => status,
        Err(error) if inherited_root => {
            return Ok(elevated_blocked(
                ProbeStage::WorkerBootstrap,
                probe_error_category(&error),
            ));
        }
        Err(error) => return Err(error),
    };
    let exit = match map_exit_status(status) {
        Ok(exit) => exit,
        Err(_) if inherited_root => {
            return Ok(elevated_blocked(
                ProbeStage::WorkerBootstrap,
                ProbeErrorCategory::Other,
            ));
        }
        Err(error) => return Err(error),
    };
    match result.outcome() {
        Ok(()) if exit.code() == 0 => {
            println!(
                "status: elevated bootstrap {} complete",
                result.mode().name()
            );
            Ok(exit)
        }
        Err((stage, category)) if exit.code() == 1 => {
            println!(
                "status: elevated bootstrap blocked stage={} error={}",
                stage.name(),
                category.name()
            );
            Ok(LauncherExit(3))
        }
        Ok(()) | Err(_) if inherited_root => Ok(elevated_blocked(
            ProbeStage::WorkerBootstrap,
            ProbeErrorCategory::InvalidInput,
        )),
        Ok(()) | Err(_) => Err(LauncherError::SessionProtocol),
    }
}

#[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
const fn credential_initial_state(
    bootstrap: bangbang_session::elevated_probe::ProbeBootstrap,
) -> bangbang_session::elevated_probe::CredentialSelfState {
    use bangbang_session::elevated_probe::{
        CredentialGroupClass, CredentialIdentityClass, CredentialSelfState,
    };

    CredentialSelfState::new(
        if bootstrap.mode().retains_root() {
            CredentialIdentityClass::InitialAndTarget
        } else {
            CredentialIdentityClass::InitialRoot
        },
        CredentialGroupClass::Other,
    )
}

#[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
const fn credential_protocol_failure(
    prefix: bangbang_session::elevated_probe::CredentialPrefix,
    state: bangbang_session::elevated_probe::CredentialSelfState,
) -> bangbang_session::elevated_probe::CredentialFailureValue {
    use bangbang_session::elevated_probe::{
        CredentialFailureValue, CredentialStep, ProbeErrorCategory,
    };

    CredentialFailureValue::new(
        CredentialStep::Protocol,
        ProbeErrorCategory::InvalidInput,
        prefix,
        state,
    )
}

#[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
fn begin_credential_exchange(
    spawned: &mut crate::macos::spawn::SuspendedWorker,
    bootstrap: bangbang_session::elevated_probe::ProbeBootstrap,
) -> Result<
    (
        bangbang_session::elevated_probe::PeerObservation,
        bangbang_session::elevated_credential::PeerBaseline,
    ),
    bangbang_session::elevated_probe::CredentialFailureValue,
> {
    use std::os::fd::AsRawFd;
    use std::time::Duration;

    use bangbang_session::elevated_probe::{
        CredentialDatagramPhase, CredentialDatagramProof, CredentialFailureValue, CredentialPrefix,
        CredentialRole, CredentialStep, ProbeErrorCategory,
    };

    const TIMEOUT: Duration = Duration::from_secs(5);
    const CREDENTIAL_LAUNCHER_ARTIFACT: &str =
        "bangbang-elevated-credential-launcher-v1-credential-drop-BBC1-BBG1-restore-groups";

    std::hint::black_box(CREDENTIAL_LAUNCHER_ARTIFACT);

    let protocol_failure = |category| {
        CredentialFailureValue::new(
            CredentialStep::Protocol,
            category,
            CredentialPrefix::None,
            credential_initial_state(bootstrap),
        )
    };
    spawned
        .grants
        .set_read_timeout(Some(TIMEOUT))
        .map_err(|error| protocol_failure(ProbeErrorCategory::from_io_kind(error.kind())))?;
    spawned
        .grants
        .set_write_timeout(Some(TIMEOUT))
        .map_err(|error| protocol_failure(ProbeErrorCategory::from_io_kind(error.kind())))?;
    let challenge = CredentialDatagramProof::challenge(bootstrap.mode(), bootstrap.nonce())
        .map_err(|_| protocol_failure(ProbeErrorCategory::InvalidInput))?;
    send_credential_datagram(&spawned.grants, challenge).map_err(protocol_failure)?;
    let worker_ready = receive_credential_datagram(&spawned.grants).map_err(protocol_failure)?;
    if !worker_ready.matches_expected(
        bootstrap.mode(),
        CredentialDatagramPhase::WorkerReady,
        CredentialRole::Worker,
        bootstrap.nonce(),
    ) {
        return Err(protocol_failure(ProbeErrorCategory::InvalidInput));
    }
    // SAFETY: `getpid` has no pointer or ownership contract.
    let socket_creator = unsafe { libc::getpid() };
    let (initial, baseline) = bangbang_session::elevated_credential::observe_initial_peer(
        spawned.session.as_raw_fd(),
        spawned.grants.as_raw_fd(),
        spawned.worker.pid(),
        socket_creator,
        bootstrap.target_uid(),
        bootstrap.target_gid(),
    )
    .map_err(|category| {
        CredentialFailureValue::new(
            CredentialStep::PeerObservation,
            category,
            CredentialPrefix::None,
            credential_initial_state(bootstrap),
        )
    })?;
    let release = CredentialDatagramProof::launcher_release(bootstrap.mode(), bootstrap.nonce())
        .map_err(|_| protocol_failure(ProbeErrorCategory::InvalidInput))?;
    send_credential_datagram(&spawned.grants, release).map_err(protocol_failure)?;
    Ok((initial, baseline))
}

#[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
fn receive_credential_datagram(
    grants: &std::os::unix::net::UnixDatagram,
) -> Result<
    bangbang_session::elevated_probe::CredentialDatagramProof,
    bangbang_session::elevated_probe::ProbeErrorCategory,
> {
    use bangbang_session::elevated_probe::{
        CREDENTIAL_DATAGRAM_BYTES, CredentialDatagramProof, ProbeErrorCategory,
    };

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

#[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
fn send_credential_datagram(
    grants: &std::os::unix::net::UnixDatagram,
    proof: bangbang_session::elevated_probe::CredentialDatagramProof,
) -> Result<(), bangbang_session::elevated_probe::ProbeErrorCategory> {
    use bangbang_session::elevated_probe::ProbeErrorCategory;

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

#[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
fn finish_credential_exchange(
    mut spawned: crate::macos::spawn::SuspendedWorker,
    bootstrap: bangbang_session::elevated_probe::ProbeBootstrap,
    initial: bangbang_session::elevated_probe::PeerObservation,
    baseline: bangbang_session::elevated_credential::PeerBaseline,
) -> Result<LauncherExit, LauncherError> {
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;

    use bangbang_session::elevated_probe::{
        CREDENTIAL_RECORD_BYTES, CredentialFailureValue, CredentialPrefix, CredentialRecord,
        CredentialRecordKind, CredentialRole, CredentialStep, PeerObservation,
    };

    // SAFETY: `getpid` has no pointer or ownership contract and remains stable
    // across this process's credential transition.
    let socket_creator = unsafe { libc::getpid() };

    let worker_transitioned = match read_credential_record(&mut spawned.session) {
        Ok(record)
            if record.matches_exchange(
                bootstrap.mode(),
                CredentialRole::Worker,
                bootstrap.nonce(),
            ) =>
        {
            match record.kind() {
                CredentialRecordKind::WorkerTransitioned => record,
                CredentialRecordKind::Failure => {
                    credential_wait_for_exit(&mut spawned, 1)?;
                    return credential_blocked_record(record);
                }
                CredentialRecordKind::LauncherTransitioned | CredentialRecordKind::WorkerFinal => {
                    return send_launcher_failure_and_wait(
                        &mut spawned,
                        bootstrap,
                        credential_protocol_failure(
                            CredentialPrefix::None,
                            credential_initial_state(bootstrap),
                        ),
                        initial,
                    );
                }
            }
        }
        Ok(_) | Err(_) => {
            return send_launcher_failure_and_wait(
                &mut spawned,
                bootstrap,
                credential_protocol_failure(
                    CredentialPrefix::None,
                    credential_initial_state(bootstrap),
                ),
                initial,
            );
        }
    };

    let after_worker = match bangbang_session::elevated_credential::observe_later_peer(
        spawned.session.as_raw_fd(),
        spawned.grants.as_raw_fd(),
        spawned.worker.pid(),
        socket_creator,
        bootstrap.target_uid(),
        bootstrap.target_gid(),
        &baseline,
    ) {
        Ok(observation) => observation,
        Err(category) => {
            return send_launcher_failure_and_wait(
                &mut spawned,
                bootstrap,
                CredentialFailureValue::new(
                    CredentialStep::PeerObservation,
                    category,
                    CredentialPrefix::None,
                    credential_initial_state(bootstrap),
                ),
                initial,
            );
        }
    };
    let transition = match bangbang_session::elevated_credential::transition_process(
        bootstrap.mode(),
        bootstrap.target_uid(),
        bootstrap.target_gid(),
    ) {
        Ok(transition) => transition,
        Err(failure) => {
            return send_launcher_failure_and_wait(&mut spawned, bootstrap, failure, initial);
        }
    };
    let after_both = match bangbang_session::elevated_credential::observe_later_peer(
        spawned.session.as_raw_fd(),
        spawned.grants.as_raw_fd(),
        spawned.worker.pid(),
        socket_creator,
        bootstrap.target_uid(),
        bootstrap.target_gid(),
        &baseline,
    ) {
        Ok(observation) => observation,
        Err(category) => {
            return send_launcher_failure_and_wait(
                &mut spawned,
                bootstrap,
                CredentialFailureValue::new(
                    CredentialStep::PeerObservation,
                    category,
                    transition.prefix(),
                    transition.state(),
                ),
                initial,
            );
        }
    };
    let launcher_observations = [initial, after_worker, after_both];
    let launcher_record = CredentialRecord::launcher_transitioned(
        bootstrap.mode(),
        transition.state(),
        launcher_observations,
        bootstrap.nonce(),
    )
    .map_err(|_| LauncherError::SessionProtocol)?;
    if spawned
        .session
        .write_all(&launcher_record.encode())
        .is_err()
    {
        return Err(LauncherError::SessionProtocol);
    }

    let worker_final = match read_credential_record(&mut spawned.session) {
        Ok(record)
            if record.matches_exchange(
                bootstrap.mode(),
                CredentialRole::Worker,
                bootstrap.nonce(),
            ) =>
        {
            match record.kind() {
                CredentialRecordKind::WorkerFinal => record,
                CredentialRecordKind::Failure => {
                    credential_wait_for_exit(&mut spawned, 1)?;
                    return credential_blocked_record(record);
                }
                CredentialRecordKind::WorkerTransitioned
                | CredentialRecordKind::LauncherTransitioned => {
                    credential_wait_for_exit(&mut spawned, 1)?;
                    return Ok(credential_blocked_value(
                        CredentialRole::Launcher,
                        credential_protocol_failure(transition.prefix(), transition.state()),
                    ));
                }
            }
        }
        Ok(_) | Err(_) => {
            credential_wait_for_exit(&mut spawned, 1)?;
            return Ok(credential_blocked_value(
                CredentialRole::Launcher,
                credential_protocol_failure(transition.prefix(), transition.state()),
            ));
        }
    };
    credential_wait_for_exit(&mut spawned, 0)?;

    let worker_initial = worker_transitioned.observations();
    let worker_observations = worker_final.observations();
    if worker_initial[0] != worker_observations[0]
        || worker_initial[1] != worker_observations[1]
        || !worker_initial[2].is_none()
    {
        return Ok(credential_blocked_value(
            CredentialRole::Launcher,
            credential_protocol_failure(transition.prefix(), transition.state()),
        ));
    }
    let Some(semantics) =
        credential_semantics(bootstrap.mode(), launcher_observations, worker_observations)
    else {
        return Ok(credential_blocked_value(
            CredentialRole::Launcher,
            credential_protocol_failure(transition.prefix(), transition.state()),
        ));
    };
    println!(
        "status: elevated credential {} complete stream-eid={} stream-cred={} stream-pid=exact datagram-cred={} datagram-token={} datagram-pid={}",
        bootstrap.mode().name(),
        semantics.stream_eid,
        semantics.stream_cred,
        semantics.datagram_cred,
        semantics.datagram_token,
        semantics.datagram_pid
    );
    return Ok(LauncherExit(0));

    fn read_credential_record(
        stream: &mut std::os::unix::net::UnixStream,
    ) -> Result<CredentialRecord, ()> {
        let mut encoded = [0_u8; CREDENTIAL_RECORD_BYTES];
        stream.read_exact(&mut encoded).map_err(|_| ())?;
        CredentialRecord::decode(&encoded).map_err(|_| ())
    }

    fn send_launcher_failure_and_wait(
        spawned: &mut crate::macos::spawn::SuspendedWorker,
        bootstrap: bangbang_session::elevated_probe::ProbeBootstrap,
        failure: CredentialFailureValue,
        initial: PeerObservation,
    ) -> Result<LauncherExit, LauncherError> {
        let record = CredentialRecord::failure(
            bootstrap.mode(),
            CredentialRole::Launcher,
            failure,
            initial,
            bootstrap.nonce(),
        )
        .map_err(|_| LauncherError::SessionProtocol)?;
        let _ = spawned.session.write_all(&record.encode());
        credential_wait_for_exit(spawned, 1)?;
        Ok(credential_blocked_value(CredentialRole::Launcher, failure))
    }
}

#[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
fn credential_wait_for_exit(
    spawned: &mut crate::macos::spawn::SuspendedWorker,
    expected: u8,
) -> Result<(), LauncherError> {
    use std::time::{Duration, Instant};

    const WAIT_TIMEOUT: Duration = Duration::from_secs(5);
    const POLL_INTERVAL: Duration = Duration::from_millis(10);

    let deadline = Instant::now()
        .checked_add(WAIT_TIMEOUT)
        .ok_or(LauncherError::SessionProtocol)?;
    loop {
        if let Some(status) = spawned.worker.try_wait()? {
            let exit = map_exit_status(status)?;
            return if exit.code() == expected {
                Ok(())
            } else {
                Err(LauncherError::SessionProtocol)
            };
        }
        if Instant::now() >= deadline {
            spawned.worker.terminate_and_reap();
            return Err(LauncherError::SessionProtocol);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
fn credential_blocked_record(
    record: bangbang_session::elevated_probe::CredentialRecord,
) -> Result<LauncherExit, LauncherError> {
    use bangbang_session::elevated_probe::{CredentialFailureValue, CredentialSelfState};

    let (step, category, prefix) = record
        .failure_value()
        .ok_or(LauncherError::SessionProtocol)?;
    Ok(credential_blocked_value(
        record.role(),
        CredentialFailureValue::new(
            step,
            category,
            prefix,
            CredentialSelfState::new(record.identity(), record.groups()),
        ),
    ))
}

#[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
fn credential_blocked_value(
    role: bangbang_session::elevated_probe::CredentialRole,
    failure: bangbang_session::elevated_probe::CredentialFailureValue,
) -> LauncherExit {
    println!(
        "status: elevated credential blocked role={} step={} error={} prefix={} identity={} groups={}",
        role.name(),
        failure.step().name(),
        failure.category().name(),
        failure.prefix().name(),
        failure.state().identity().name(),
        failure.state().groups().name()
    );
    LauncherExit(3)
}

#[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
struct CredentialSemantics {
    stream_eid: &'static str,
    stream_cred: &'static str,
    datagram_cred: &'static str,
    datagram_token: &'static str,
    datagram_pid: &'static str,
}

#[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
fn credential_semantics(
    mode: bangbang_session::elevated_probe::ProbeMode,
    launcher: [bangbang_session::elevated_probe::PeerObservation; 3],
    worker: [bangbang_session::elevated_probe::PeerObservation; 3],
) -> Option<CredentialSemantics> {
    use bangbang_session::elevated_probe::{PeerPidClass, PeerTokenClass};

    if launcher
        .iter()
        .chain(worker.iter())
        .any(|observation| observation.stream_pid() != PeerPidClass::Exact)
    {
        return None;
    }
    let datagram_pid = if launcher
        .iter()
        .chain(worker.iter())
        .all(|observation| observation.datagram_pid() == PeerPidClass::Unsupported)
    {
        "unsupported"
    } else if launcher
        .iter()
        .all(|observation| observation.datagram_pid() == PeerPidClass::Exact)
        && worker
            .iter()
            .all(|observation| observation.datagram_pid() == PeerPidClass::Exact)
    {
        "exact"
    } else if launcher
        .iter()
        .all(|observation| observation.datagram_pid() == PeerPidClass::SocketCreator)
        && worker
            .iter()
            .all(|observation| observation.datagram_pid() == PeerPidClass::Exact)
    {
        "creator-snapshot"
    } else {
        return None;
    };
    let stream_eid = identity_semantic(mode, launcher, worker, |value| value.stream_eid())?;
    let stream_cred = identity_semantic(mode, launcher, worker, |value| value.stream_cred())?;
    let datagram_cred = identity_semantic(mode, launcher, worker, |value| value.datagram_cred())?;
    let tokens = [
        launcher[0].datagram_token(),
        launcher[1].datagram_token(),
        launcher[2].datagram_token(),
        worker[0].datagram_token(),
        worker[1].datagram_token(),
        worker[2].datagram_token(),
    ];
    let datagram_token = if tokens
        .iter()
        .all(|value| *value == PeerTokenClass::Unsupported)
    {
        "unsupported"
    } else if tokens[0] == PeerTokenClass::Baseline
        && tokens[3] == PeerTokenClass::Baseline
        && tokens[1..3]
            .iter()
            .chain(tokens[4..6].iter())
            .all(|value| *value == PeerTokenClass::Unchanged)
    {
        "unchanged"
    } else if tokens[0] == PeerTokenClass::Baseline
        && tokens[3] == PeerTokenClass::Baseline
        && tokens[1..3]
            .iter()
            .chain(tokens[4..6].iter())
            .all(|value| matches!(value, PeerTokenClass::Unchanged | PeerTokenClass::Changed))
        && tokens.contains(&PeerTokenClass::Changed)
    {
        "changed"
    } else {
        return None;
    };
    Some(CredentialSemantics {
        stream_eid,
        stream_cred,
        datagram_cred,
        datagram_token,
        datagram_pid,
    })
}

#[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
fn identity_semantic<F>(
    mode: bangbang_session::elevated_probe::ProbeMode,
    launcher: [bangbang_session::elevated_probe::PeerObservation; 3],
    worker: [bangbang_session::elevated_probe::PeerObservation; 3],
    select: F,
) -> Option<&'static str>
where
    F: Fn(
        bangbang_session::elevated_probe::PeerObservation,
    ) -> bangbang_session::elevated_probe::CredentialIdentityClass,
{
    use bangbang_session::elevated_probe::CredentialIdentityClass::{
        InitialAndTarget, InitialRoot, Other, Target, Unsupported,
    };

    let launcher = launcher.map(&select);
    let worker = worker.map(&select);
    let values = [
        launcher[0],
        launcher[1],
        launcher[2],
        worker[0],
        worker[1],
        worker[2],
    ];
    if values.iter().all(|value| *value == Unsupported) {
        return Some("unsupported");
    }
    if values
        .iter()
        .any(|value| matches!(value, Other | Unsupported))
    {
        return None;
    }
    if mode.retains_root() {
        return values
            .iter()
            .all(|value| *value == InitialAndTarget)
            .then_some("stable-root");
    }
    if values.iter().all(|value| *value == InitialRoot) {
        return Some("snapshot");
    }
    if launcher == [InitialRoot, Target, Target] && worker == [InitialRoot, InitialRoot, Target] {
        return Some("dynamic");
    }
    values
        .iter()
        .all(|value| matches!(value, InitialRoot | Target))
        .then_some("mixed")
}

#[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
fn elevated_blocked(
    stage: bangbang_session::elevated_probe::ProbeStage,
    category: bangbang_session::elevated_probe::ProbeErrorCategory,
) -> LauncherExit {
    println!(
        "status: elevated bootstrap blocked stage={} error={}",
        stage.name(),
        category.name()
    );
    LauncherExit(3)
}

#[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
fn probe_error_category(
    error: &LauncherError,
) -> bangbang_session::elevated_probe::ProbeErrorCategory {
    use bangbang_session::elevated_probe::ProbeErrorCategory;

    match error {
        LauncherError::WorkerSpawn(kind)
        | LauncherError::SessionSetup(kind)
        | LauncherError::WorkerWait(kind)
        | LauncherError::SignalForward(kind) => ProbeErrorCategory::from_io_kind(*kind),
        LauncherError::InvalidBundleLayout
        | LauncherError::InvalidBundleEntry
        | LauncherError::InvalidWorkerIdentity
        | LauncherError::InvalidLaunchPolicy => ProbeErrorCategory::InvalidInput,
        _ => ProbeErrorCategory::Other,
    }
}

#[cfg(target_os = "macos")]
fn launch_prepared(
    layout: &BundleLayout,
    launch: crate::launch_policy::PreparedLaunch,
    notifier: Option<&mut crate::macos::daemon::DaemonNotifier>,
) -> Result<LauncherExit, LauncherError> {
    use std::os::fd::AsRawFd;

    use bangbang_session::{LauncherLifecycle, SessionId};

    let wakeups = crate::macos::supervise::SignalWakeups::install()?;
    let session_id = SessionId::generate().map_err(|_| LauncherError::SessionProtocol)?;
    let mut lifecycle = LauncherLifecycle::new(session_id);
    let mut spawned =
        crate::macos::spawn::spawn_suspended(layout.worker_executable(), launch.worker_args)?;
    if crate::macos::code_sign::validate_worker_process(spawned.worker.pid())?
        != launch.worker_profile
    {
        return Err(LauncherError::InvalidWorkerIdentity);
    }
    spawned.worker.resume()?;
    crate::macos::supervise::read_bootstrap_hello(&mut spawned.session, &mut lifecycle)?;
    bangbang_session::macos::verify_peer(spawned.session.as_raw_fd(), spawned.worker.pid())
        .map_err(|_| LauncherError::InvalidWorkerIdentity)?;
    if crate::macos::code_sign::validate_worker_process(spawned.worker.pid())?
        != launch.worker_profile
    {
        return Err(LauncherError::InvalidWorkerIdentity);
    }
    let start = lifecycle
        .start(launch.worker_policy)
        .map_err(|_| LauncherError::SessionProtocol)?;
    crate::macos::supervise::write_frame(&mut spawned.session, start)?;
    let status = crate::macos::supervise::wait_session(
        &mut spawned.worker,
        &mut spawned.session,
        crate::macos::supervise::AuxiliaryChannels::new(
            &mut spawned.grants,
            &mut spawned.socket_broker,
            &mut spawned.vhost_user_broker,
            &mut spawned.block_control_broker,
        ),
        lifecycle,
        wakeups,
        &launch.grants,
        notifier,
    )?;
    map_exit_status(status)
}

#[cfg(any(target_os = "macos", test))]
fn map_exit_status(status: std::process::ExitStatus) -> Result<LauncherExit, LauncherError> {
    if let Some(code) = status.code() {
        return u8::try_from(code)
            .map(LauncherExit)
            .map_err(|_| LauncherError::WorkerWait(std::io::ErrorKind::InvalidData));
    }
    if let Some(signal) = status.signal() {
        let code = 128_i32
            .checked_add(signal)
            .and_then(|value| u8::try_from(value).ok())
            .ok_or(LauncherError::WorkerWait(std::io::ErrorKind::InvalidData))?;
        return Ok(LauncherExit(code));
    }
    Err(LauncherError::WorkerWait(std::io::ErrorKind::InvalidData))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    #[test]
    fn preserves_ordinary_worker_exit_codes() {
        for code in [0, 1, 152, 157, 255] {
            let status = std::process::ExitStatus::from_raw(code << 8);
            assert_eq!(
                map_exit_status(status).expect("ordinary status should map"),
                LauncherExit(u8::try_from(code).expect("test code should fit"))
            );
        }
    }

    #[test]
    fn maps_signaled_worker_to_conventional_exit() {
        let status = std::process::ExitStatus::from_raw(libc::SIGTERM);
        assert_eq!(
            map_exit_status(status).expect("signal status should map"),
            LauncherExit(128 + u8::try_from(libc::SIGTERM).expect("signal should fit"))
        );
    }

    #[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
    fn peer_observation(
        identity: bangbang_session::elevated_probe::CredentialIdentityClass,
        datagram_pid: bangbang_session::elevated_probe::PeerPidClass,
        token: bangbang_session::elevated_probe::PeerTokenClass,
    ) -> bangbang_session::elevated_probe::PeerObservation {
        use bangbang_session::elevated_probe::{
            CredentialIdentityClass, PeerObservation, PeerPidClass,
        };

        PeerObservation::new(
            identity,
            identity,
            PeerPidClass::Exact,
            CredentialIdentityClass::Unsupported,
            datagram_pid,
            token,
        )
        .expect("complete peer observation")
    }

    #[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
    #[test]
    fn credential_protocol_failure_preserves_the_completed_transition_prefix() {
        use bangbang_session::elevated_probe::{
            CredentialGroupClass, CredentialIdentityClass, CredentialPrefix, CredentialSelfState,
        };

        let failure = credential_protocol_failure(
            CredentialPrefix::Irreversible,
            CredentialSelfState::new(
                CredentialIdentityClass::Target,
                CredentialGroupClass::EffectiveOnly,
            ),
        );

        assert_eq!(failure.prefix(), CredentialPrefix::Irreversible);
        assert_eq!(failure.state().identity(), CredentialIdentityClass::Target);
        assert_eq!(
            failure.state().groups(),
            CredentialGroupClass::EffectiveOnly
        );
    }

    #[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
    #[test]
    fn credential_semantics_distinguish_snapshot_dynamic_and_retained_root() {
        use bangbang_session::elevated_probe::{
            CredentialIdentityClass::{InitialAndTarget, InitialRoot, Other, Target},
            PeerPidClass, PeerTokenClass, ProbeMode,
        };

        let snapshot = [
            peer_observation(InitialRoot, PeerPidClass::Exact, PeerTokenClass::Baseline),
            peer_observation(InitialRoot, PeerPidClass::Exact, PeerTokenClass::Unchanged),
            peer_observation(InitialRoot, PeerPidClass::Exact, PeerTokenClass::Unchanged),
        ];
        assert_eq!(
            identity_semantic(ProbeMode::CredentialDrop, snapshot, snapshot, |value| value
                .stream_eid()),
            Some("snapshot")
        );

        let launcher = [
            peer_observation(InitialRoot, PeerPidClass::Exact, PeerTokenClass::Baseline),
            peer_observation(Target, PeerPidClass::Exact, PeerTokenClass::Changed),
            peer_observation(Target, PeerPidClass::Exact, PeerTokenClass::Changed),
        ];
        let worker = [
            peer_observation(InitialRoot, PeerPidClass::Exact, PeerTokenClass::Baseline),
            peer_observation(InitialRoot, PeerPidClass::Exact, PeerTokenClass::Unchanged),
            peer_observation(Target, PeerPidClass::Exact, PeerTokenClass::Changed),
        ];
        assert_eq!(
            identity_semantic(ProbeMode::CredentialDrop, launcher, worker, |value| value
                .stream_eid()),
            Some("dynamic")
        );

        let retained = [
            peer_observation(
                InitialAndTarget,
                PeerPidClass::Exact,
                PeerTokenClass::Baseline,
            ),
            peer_observation(
                InitialAndTarget,
                PeerPidClass::Exact,
                PeerTokenClass::Unchanged,
            ),
            peer_observation(
                InitialAndTarget,
                PeerPidClass::Exact,
                PeerTokenClass::Unchanged,
            ),
        ];
        assert_eq!(
            identity_semantic(
                ProbeMode::CredentialRetainRoot,
                retained,
                retained,
                |value| value.stream_eid(),
            ),
            Some("stable-root")
        );

        let invalid = [
            peer_observation(Other, PeerPidClass::Exact, PeerTokenClass::Baseline),
            peer_observation(Other, PeerPidClass::Exact, PeerTokenClass::Unchanged),
            peer_observation(Other, PeerPidClass::Exact, PeerTokenClass::Unchanged),
        ];
        assert_eq!(
            identity_semantic(ProbeMode::CredentialDrop, invalid, snapshot, |value| value
                .stream_eid()),
            None
        );
    }

    #[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
    #[test]
    fn credential_summary_requires_exact_stream_pid_and_bounded_datagram_creator_shape() {
        use bangbang_session::elevated_probe::{
            CredentialIdentityClass::InitialRoot, PeerPidClass, PeerTokenClass, ProbeMode,
        };

        let launcher = [
            peer_observation(
                InitialRoot,
                PeerPidClass::SocketCreator,
                PeerTokenClass::Baseline,
            ),
            peer_observation(
                InitialRoot,
                PeerPidClass::SocketCreator,
                PeerTokenClass::Changed,
            ),
            peer_observation(
                InitialRoot,
                PeerPidClass::SocketCreator,
                PeerTokenClass::Changed,
            ),
        ];
        let worker = [
            peer_observation(InitialRoot, PeerPidClass::Exact, PeerTokenClass::Baseline),
            peer_observation(InitialRoot, PeerPidClass::Exact, PeerTokenClass::Unchanged),
            peer_observation(InitialRoot, PeerPidClass::Exact, PeerTokenClass::Changed),
        ];
        let summary = credential_semantics(ProbeMode::CredentialDrop, launcher, worker)
            .expect("bounded creator snapshot should summarize");
        assert_eq!(summary.stream_eid, "snapshot");
        assert_eq!(summary.datagram_cred, "unsupported");
        assert_eq!(summary.datagram_token, "changed");
        assert_eq!(summary.datagram_pid, "creator-snapshot");

        let unsupported_pid = launcher.map(|value| {
            peer_observation(
                value.stream_eid(),
                PeerPidClass::Unsupported,
                value.datagram_token(),
            )
        });
        let unsupported_worker_pid = worker.map(|value| {
            peer_observation(
                value.stream_eid(),
                PeerPidClass::Unsupported,
                value.datagram_token(),
            )
        });
        assert_eq!(
            credential_semantics(
                ProbeMode::CredentialDrop,
                unsupported_pid,
                unsupported_worker_pid,
            )
            .expect("uniform unsupported datagram PID should summarize")
            .datagram_pid,
            "unsupported"
        );

        let mismatched = launcher.map(|value| {
            peer_observation(
                value.stream_eid(),
                PeerPidClass::Mismatch,
                value.datagram_token(),
            )
        });
        assert!(credential_semantics(ProbeMode::CredentialDrop, mismatched, worker).is_none());
    }
}
