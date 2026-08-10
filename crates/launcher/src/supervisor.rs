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
            let mut guest_contract = match elevated_probe
                .validate_runtime_contract(&launch.worker_args, &launch.grants)
            {
                Ok(contract) => contract,
                Err(error) if elevated_probe.mode().continues_runtime() => {
                    return Err(error);
                }
                Err(_) => None,
            };
            if elevated_probe.stop_after_adoption()? {
                guest_contract = elevated_probe
                    .validate_runtime_contract(&launch.worker_args, &launch.grants)?;
            }
            elevated_probe.prepend_worker_activation(&mut launch.worker_args)?;
            return launch_elevated_prepared(&layout, launch, elevated_probe, guest_contract);
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
    mut launch: crate::launch_policy::PreparedLaunch,
    mut elevated_probe: crate::elevated_probe::Config,
    guest_contract: Option<crate::grant_manifest::ElevatedGuestContract>,
) -> Result<LauncherExit, LauncherError> {
    let bootstrap = elevated_probe.bootstrap()?;
    if elevated_probe.mode() == bangbang_session::elevated_probe::ProbeMode::InheritedRoot {
        return launch_inherited_prepared(launch, elevated_probe, bootstrap);
    }
    let runtime = if bootstrap.mode().continues_runtime() {
        launch.worker_policy = launch
            .worker_policy
            .with_identity(bootstrap.target_uid(), bootstrap.target_gid());
        Some(ElevatedRuntimeLaunch {
            root: elevated_probe.take_runtime_root()?,
            guest_contract,
        })
    } else {
        None
    };
    let worker_args = std::mem::take(&mut launch.worker_args);
    let spawned = crate::macos::spawn::prepare_suspended_elevated(
        layout.worker_executable(),
        worker_args,
        elevated_probe.root_fd(),
    )?
    .spawn()?;
    // The completed spawn copied the fixed root descriptor into the worker.
    // Close the launcher-owned copy before any credential transition begins.
    drop(elevated_probe);
    finish_elevated_exchange(spawned, launch, bootstrap, false, runtime)
}

#[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
struct ElevatedRuntimeLaunch {
    root: bangbang_session::macos::runtime::ExplicitRuntimeRoot,
    guest_contract: Option<crate::grant_manifest::ElevatedGuestContract>,
}

#[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
fn launch_inherited_prepared(
    mut launch: crate::launch_policy::PreparedLaunch,
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
    let worker_args = std::mem::take(&mut launch.worker_args);
    let prepared = match crate::macos::spawn::prepare_suspended_elevated(
        worker,
        worker_args,
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
    finish_elevated_exchange(spawned, launch, bootstrap, true, None)
}

#[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
fn finish_elevated_exchange(
    mut spawned: crate::macos::spawn::SuspendedWorker,
    launch: crate::launch_policy::PreparedLaunch,
    bootstrap: bangbang_session::elevated_probe::ProbeBootstrap,
    inherited_root: bool,
    runtime: Option<ElevatedRuntimeLaunch>,
) -> Result<LauncherExit, LauncherError> {
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::time::Duration;

    use bangbang_session::elevated_probe::{
        ProbeErrorCategory, ProbeResult, ProbeStage, READY_RECORD, RESULT_RECORD_BYTES,
    };

    const TIMEOUT: Duration = Duration::from_secs(5);
    let expected_profile = launch.worker_profile.clone();

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
        return finish_credential_exchange(spawned, launch, bootstrap, initial, baseline, runtime);
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
    launch: crate::launch_policy::PreparedLaunch,
    bootstrap: bangbang_session::elevated_probe::ProbeBootstrap,
    initial: bangbang_session::elevated_probe::PeerObservation,
    baseline: bangbang_session::elevated_credential::PeerBaseline,
    runtime: Option<ElevatedRuntimeLaunch>,
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
                    let _ = credential_wait_for_exit(&mut spawned, 1);
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
    let launcher_record = match CredentialRecord::launcher_transitioned(
        bootstrap.mode(),
        transition.state(),
        launcher_observations,
        bootstrap.nonce(),
    ) {
        Ok(record) => record,
        Err(_) => {
            return Ok(credential_protocol_blocked_after_reap(
                &mut spawned,
                transition.prefix(),
                transition.state(),
            ));
        }
    };
    if spawned
        .session
        .write_all(&launcher_record.encode())
        .is_err()
    {
        return Ok(credential_protocol_blocked_after_reap(
            &mut spawned,
            transition.prefix(),
            transition.state(),
        ));
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
                    let _ = credential_wait_for_exit(&mut spawned, 1);
                    return credential_blocked_record(record);
                }
                CredentialRecordKind::WorkerTransitioned
                | CredentialRecordKind::LauncherTransitioned => {
                    return Ok(credential_protocol_blocked_after_reap(
                        &mut spawned,
                        transition.prefix(),
                        transition.state(),
                    ));
                }
            }
        }
        Ok(_) | Err(_) => {
            return Ok(credential_protocol_blocked_after_reap(
                &mut spawned,
                transition.prefix(),
                transition.state(),
            ));
        }
    };
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
    if bootstrap.mode().continues_runtime() {
        let Some(runtime) = runtime else {
            return Ok(credential_protocol_blocked_after_reap(
                &mut spawned,
                transition.prefix(),
                transition.state(),
            ));
        };
        return continue_runtime_session(spawned, launch, runtime, bootstrap, semantics);
    }
    if runtime.is_some() || credential_wait_for_exit(&mut spawned, 0).is_err() {
        return Ok(credential_protocol_blocked_after_reap(
            &mut spawned,
            transition.prefix(),
            transition.state(),
        ));
    }
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
        let record = match CredentialRecord::failure(
            bootstrap.mode(),
            CredentialRole::Launcher,
            failure,
            initial,
            bootstrap.nonce(),
        ) {
            Ok(record) => record,
            Err(_) => {
                spawned.worker.terminate_and_reap();
                return Ok(credential_blocked_value(CredentialRole::Launcher, failure));
            }
        };
        let _ = spawned.session.write_all(&record.encode());
        let _ = credential_wait_for_exit(spawned, 1);
        Ok(credential_blocked_value(CredentialRole::Launcher, failure))
    }
}

#[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
fn continue_runtime_session(
    mut spawned: crate::macos::spawn::SuspendedWorker,
    launch: crate::launch_policy::PreparedLaunch,
    runtime: ElevatedRuntimeLaunch,
    bootstrap: bangbang_session::elevated_probe::ProbeBootstrap,
    semantics: CredentialSemantics,
) -> Result<LauncherExit, LauncherError> {
    const RUNTIME_LAUNCHER_ARTIFACT: &str = "bangbang-elevated-runtime-launcher-v2-runtime-drop-runtime-retain-root-runtime-unmapped-status: elevated runtime-BBA1-BBN1-launcher-created-session";
    const RUNTIME_LAUNCHER_BOUNDARY_ARTIFACT: &str = "bangbang-elevated-runtime-launcher-boundaries-v2-pre-ack-post-ack-session-create-session-open-authority-send-authority-receive-authority-validate-session-lock-session-enter-prepared-namespace-grant-transfer-proceed-terminal-continuation-ack-lifecycle-hello-runtime-session-create-runtime-session-open-runtime-authority-send-runtime-authority-receive-runtime-authority-validate-runtime-session-lock-runtime-session-enter-lifecycle-prepared-runtime-namespace-grant-accepted-lifecycle-proceed-lifecycle-terminal-runtime-cleanup-complete-continuation-boundary-identity-boundary-explicit-root-boundary-namespace-boundary-grant-boundary-lifecycle-boundary";
    const GUEST_LAUNCHER_BOUNDARY_ARTIFACT: &str = "bangbang-elevated-guest-launcher-boundaries-v1-guest-no-api-drop-guest-no-api-retain-root-guest-no-api-unmapped-guest-api-drop-guest-api-retain-root-guest-api-unmapped-BBW1-guest-resource-witness-guest-grant-accepted-guest-transport-contamination-guest-hvf-witness-guest-terminal-evidence-api-instance-start-bangbang-grant:evidence-guest-kernel-bangbang-grant:evidence-guest-serial";
    const API_LISTENER_LAUNCHER_BOUNDARY_ARTIFACT: &str = "bangbang-elevated-api-listener-launcher-v1-BBL1-request-bind-transfer-adoption-final-child-one-right";

    use std::io::Write;
    use std::os::fd::AsRawFd;

    use bangbang_session::elevated_probe::{
        ContinuationAck, ProbeErrorCategory, ProbeStage, RuntimeFault, RuntimeSessionAuthority,
        RuntimeWorkerFailure, RuntimeWorkload,
    };
    use bangbang_session::macos::runtime::{PreparedLauncherSession, RuntimeError};
    use bangbang_session::macos::runtime_authority::send_runtime_session_authority;
    use bangbang_session::{LauncherLifecycle, SessionId};

    std::hint::black_box(RUNTIME_LAUNCHER_ARTIFACT);
    std::hint::black_box(RUNTIME_LAUNCHER_BOUNDARY_ARTIFACT);
    std::hint::black_box(GUEST_LAUNCHER_BOUNDARY_ARTIFACT);
    std::hint::black_box(API_LISTENER_LAUNCHER_BOUNDARY_ARTIFACT);
    let blocked = |stage, category| {
        Ok(elevated_runtime_blocked(
            bootstrap, &semantics, stage, category,
        ))
    };
    let contract_valid = match (bootstrap.mode().runtime_workload(), runtime.guest_contract) {
        (Some(RuntimeWorkload::RepresentativeGrants), None) => true,
        (Some(RuntimeWorkload::GuestNoApi), Some(contract)) => {
            contract.workload() == RuntimeWorkload::GuestNoApi && contract.api_anchor().is_none()
        }
        (Some(RuntimeWorkload::GuestApi), Some(contract)) => {
            contract.workload() == RuntimeWorkload::GuestApi && contract.api_anchor().is_some()
        }
        _ => false,
    };
    if !contract_valid {
        let _ = credential_wait_for_exit(&mut spawned, 1);
        return blocked(
            ProbeStage::GuestGrantContract,
            ProbeErrorCategory::InvalidInput,
        );
    }
    if bootstrap.fault() == RuntimeFault::GuestGrantContract {
        let _ = credential_wait_for_exit(&mut spawned, 1);
        return blocked(ProbeStage::GuestGrantContract, ProbeErrorCategory::Other);
    }
    if bootstrap.fault() == RuntimeFault::PreAck {
        let _ = credential_wait_for_exit(&mut spawned, 1);
        return blocked(ProbeStage::ContinuationAck, ProbeErrorCategory::Other);
    }
    let ack = ContinuationAck::launcher(bootstrap.mode(), bootstrap.nonce())
        .map_err(|_| LauncherError::SessionProtocol)?;
    if spawned.session.write_all(&ack.encode()).is_err() {
        return blocked(ProbeStage::ContinuationAck, ProbeErrorCategory::Other);
    }
    if spawned.session.set_read_timeout(None).is_err()
        || spawned.session.set_write_timeout(None).is_err()
        || spawned.grants.set_read_timeout(None).is_err()
        || spawned.grants.set_write_timeout(None).is_err()
    {
        return blocked(ProbeStage::ContinuationAck, ProbeErrorCategory::Other);
    }
    if bootstrap.fault() == RuntimeFault::PostAck {
        let _ = credential_wait_for_exit(&mut spawned, 1);
        return blocked(ProbeStage::LifecycleHello, ProbeErrorCategory::Other);
    }
    if bangbang_session::elevated_credential::attest_current_process(
        bootstrap.mode(),
        bootstrap.target_uid(),
        bootstrap.target_gid(),
    )
    .is_err()
        || bangbang_session::macos::verify_peer_pid(
            spawned.session.as_raw_fd(),
            spawned.worker.pid(),
        )
        .is_err()
    {
        return blocked(
            ProbeStage::LiveIdentity,
            ProbeErrorCategory::PermissionDenied,
        );
    }
    if let Err(category) = validate_runtime_worker(spawned.worker.pid(), &launch.worker_profile) {
        return blocked(ProbeStage::LiveIdentity, category);
    }

    let wakeups = match crate::macos::supervise::SignalWakeups::install() {
        Ok(wakeups) => wakeups,
        Err(_) => return blocked(ProbeStage::LifecycleHello, ProbeErrorCategory::Other),
    };
    let session_id = SessionId::generate().map_err(|_| LauncherError::SessionProtocol)?;
    let mut lifecycle = LauncherLifecycle::new(session_id);
    if bootstrap.fault() == RuntimeFault::SessionCreate {
        spawned.worker.terminate_and_reap();
        return blocked(ProbeStage::RuntimeSessionCreate, ProbeErrorCategory::Other);
    }
    let prepared = match PreparedLauncherSession::create(runtime.root, session_id) {
        Ok(prepared) => prepared,
        Err(error) => {
            let stage = match error {
                RuntimeError::NamespaceCreate(_) | RuntimeError::Collision => {
                    ProbeStage::RuntimeSessionCreate
                }
                RuntimeError::Filesystem(_)
                | RuntimeError::InvalidRoot
                | RuntimeError::InvalidEntry => ProbeStage::RuntimeSessionOpen,
            };
            return blocked(stage, runtime_namespace_error_category(error));
        }
    };
    if bootstrap.fault() == RuntimeFault::SessionOpen {
        if prepared.cleanup_unpublished().is_err() {
            spawned.worker.terminate_and_reap();
            return blocked(ProbeStage::RuntimeCleanup, ProbeErrorCategory::Other);
        }
        spawned.worker.terminate_and_reap();
        return blocked(ProbeStage::RuntimeSessionOpen, ProbeErrorCategory::Other);
    }
    let authority = match RuntimeSessionAuthority::launcher(
        bootstrap.mode(),
        bootstrap.target_uid(),
        bootstrap.target_gid(),
        bootstrap.root(),
        prepared.identity().object_identity(),
        bootstrap.nonce(),
        session_id,
    ) {
        Ok(authority) => authority,
        Err(_) => {
            if prepared.cleanup_unpublished().is_err() {
                spawned.worker.terminate_and_reap();
                return blocked(ProbeStage::RuntimeCleanup, ProbeErrorCategory::Other);
            }
            spawned.worker.terminate_and_reap();
            return blocked(
                ProbeStage::RuntimeAuthoritySend,
                ProbeErrorCategory::InvalidInput,
            );
        }
    };
    if bootstrap.fault() == RuntimeFault::AuthoritySend {
        if prepared.cleanup_unpublished().is_err() {
            spawned.worker.terminate_and_reap();
            return blocked(ProbeStage::RuntimeCleanup, ProbeErrorCategory::Other);
        }
        spawned.worker.terminate_and_reap();
        return blocked(ProbeStage::RuntimeAuthoritySend, ProbeErrorCategory::Other);
    }
    let (transfer, handles) = prepared.into_publication();
    if let Err(error) = send_runtime_session_authority(&spawned.grants, authority, transfer) {
        if finish_published_runtime_session(&mut spawned, handles).is_err() {
            return blocked(ProbeStage::RuntimeCleanup, ProbeErrorCategory::Other);
        }
        return blocked(
            ProbeStage::RuntimeAuthoritySend,
            runtime_transport_error_category(error),
        );
    }
    let mut handles = Some(handles);
    if crate::macos::supervise::read_bootstrap_hello(&mut spawned.session, &mut lifecycle).is_err()
    {
        let outcome = finish_published_runtime_session(
            &mut spawned,
            handles.take().ok_or(LauncherError::RuntimeNamespace)?,
        );
        return match outcome {
            Ok(Some(failure)) => blocked(failure.stage(), failure.category()),
            Ok(None) => {
                let stage = match bootstrap.fault() {
                    RuntimeFault::AuthorityReceive => ProbeStage::RuntimeAuthorityReceive,
                    RuntimeFault::AuthorityValidate => ProbeStage::RuntimeAuthorityValidate,
                    RuntimeFault::SessionLock => ProbeStage::RuntimeSessionLock,
                    _ => ProbeStage::LifecycleHello,
                };
                blocked(stage, ProbeErrorCategory::Other)
            }
            Err(_) => blocked(ProbeStage::RuntimeCleanup, ProbeErrorCategory::Other),
        };
    }
    if !runtime_transport_is_empty(spawned.grants.as_raw_fd()) {
        if finish_published_runtime_session(
            &mut spawned,
            handles.take().ok_or(LauncherError::RuntimeNamespace)?,
        )
        .is_err()
        {
            return blocked(ProbeStage::RuntimeCleanup, ProbeErrorCategory::Other);
        }
        return blocked(
            ProbeStage::RuntimeAuthorityValidate,
            ProbeErrorCategory::InvalidInput,
        );
    }
    if bangbang_session::macos::verify_peer_pid(spawned.session.as_raw_fd(), spawned.worker.pid())
        .is_err()
        || bangbang_session::elevated_credential::attest_current_process(
            bootstrap.mode(),
            bootstrap.target_uid(),
            bootstrap.target_gid(),
        )
        .is_err()
    {
        if finish_published_runtime_session(
            &mut spawned,
            handles.take().ok_or(LauncherError::RuntimeNamespace)?,
        )
        .is_err()
        {
            return blocked(ProbeStage::RuntimeCleanup, ProbeErrorCategory::Other);
        }
        return blocked(
            ProbeStage::LiveIdentity,
            ProbeErrorCategory::PermissionDenied,
        );
    }
    if let Err(category) = validate_runtime_worker(spawned.worker.pid(), &launch.worker_profile) {
        if finish_published_runtime_session(
            &mut spawned,
            handles.take().ok_or(LauncherError::RuntimeNamespace)?,
        )
        .is_err()
        {
            return blocked(ProbeStage::RuntimeCleanup, ProbeErrorCategory::Other);
        }
        return blocked(ProbeStage::LiveIdentity, category);
    }
    let start = match lifecycle.start(launch.worker_policy) {
        Ok(start) => start,
        Err(_) => {
            if finish_published_runtime_session(
                &mut spawned,
                handles.take().ok_or(LauncherError::RuntimeNamespace)?,
            )
            .is_err()
            {
                return blocked(ProbeStage::RuntimeCleanup, ProbeErrorCategory::Other);
            }
            return blocked(ProbeStage::LifecycleHello, ProbeErrorCategory::InvalidInput);
        }
    };
    let (guest, guest_failure) = match runtime.guest_contract {
        Some(contract) => {
            let (guest, failure) = crate::macos::elevated_guest::ElevatedGuestSupervisor::new(
                bootstrap,
                contract,
                launch.worker_profile.clone(),
                session_id,
            )?;
            (Some(guest), Some(failure))
        }
        None => (None, None),
    };
    let status = match crate::macos::supervise::wait_session_with_preopened_namespace(
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
        crate::macos::supervise::PreopenedSessionStart::new(
            handles.take().ok_or(LauncherError::RuntimeNamespace)?,
            start,
            guest,
        ),
    ) {
        Ok(status) => status,
        Err(error) => {
            if let Some(failure) = guest_failure
                .as_ref()
                .and_then(crate::macos::elevated_guest::ElevatedGuestFailureHandle::failure)
                && (bootstrap.fault() == RuntimeFault::None
                    || bootstrap.fault().stage() == Some(failure.stage()))
            {
                return blocked(failure.stage(), failure.category());
            }
            let stage = bootstrap.fault().stage().unwrap_or(match error {
                LauncherError::RuntimeNamespace => ProbeStage::RuntimeNamespace,
                LauncherError::GrantProtocol | LauncherError::GrantPreparation => {
                    ProbeStage::GrantAccepted
                }
                _ => ProbeStage::LifecycleTerminal,
            });
            return blocked(stage, probe_error_category(&error));
        }
    };
    let exit = map_exit_status(status)?;
    if let Ok(failure) = RuntimeWorkerFailure::from_exit_code(exit.code()) {
        return blocked(failure.stage(), failure.category());
    }
    if exit.code() == bangbang_session::elevated_probe::RUNTIME_NAMESPACE_PERMISSION_EXIT_CODE {
        return blocked(
            ProbeStage::RuntimeNamespace,
            ProbeErrorCategory::PermissionDenied,
        );
    }
    if bootstrap.fault() != RuntimeFault::None || exit.code() != 0 {
        let stage = bootstrap
            .fault()
            .stage()
            .unwrap_or(ProbeStage::LifecycleTerminal);
        return blocked(stage, ProbeErrorCategory::Other);
    }
    let guest_completion = match bootstrap.mode().runtime_workload() {
        Some(RuntimeWorkload::RepresentativeGrants) => "",
        Some(RuntimeWorkload::GuestNoApi) => {
            " resources=consumed workload=no-api api=absent hvf=created guest=oracle-poweroff"
        }
        Some(RuntimeWorkload::GuestApi) => {
            " resources=consumed workload=api api=complete hvf=created guest=oracle-poweroff"
        }
        None => return Err(LauncherError::InvalidLaunchPolicy),
    };
    println!(
        "status: elevated runtime {} complete result={} stream-eid={} stream-cred={} stream-pid=exact datagram-cred={} datagram-token={} datagram-pid={} namespace=launcher-created-target-owned authority=consumed lock=independent grants=committed{} lifecycle=terminal cleanup=complete",
        bootstrap.mode().name(),
        bangbang_session::elevated_probe::RuntimeResultClass::Complete.name(),
        semantics.stream_eid,
        semantics.stream_cred,
        semantics.datagram_cred,
        semantics.datagram_token,
        semantics.datagram_pid,
        guest_completion,
    );
    Ok(exit)
}

#[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
fn finish_published_runtime_session(
    spawned: &mut crate::macos::spawn::SuspendedWorker,
    mut handles: bangbang_session::macos::runtime::LauncherSessionHandles,
) -> Result<Option<bangbang_session::elevated_probe::RuntimeWorkerFailure>, LauncherError> {
    let status = match spawned.worker.try_wait()? {
        Some(status) => status,
        None => {
            spawned
                .worker
                .signal(libc::SIGKILL)
                .map_err(LauncherError::SignalForward)?;
            spawned.worker.wait()?
        }
    };
    let session = handles.session();
    if let Some(mut namespace) = handles
        .recover_after_worker_exit(session)
        .map_err(|_| LauncherError::RuntimeNamespace)?
    {
        namespace
            .cleanup()
            .map_err(|_| LauncherError::RuntimeNamespace)?;
    }
    let exit = map_exit_status(status)?;
    Ok(bangbang_session::elevated_probe::RuntimeWorkerFailure::from_exit_code(exit.code()).ok())
}

#[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
const fn runtime_namespace_error_category(
    error: bangbang_session::macos::runtime::RuntimeError,
) -> bangbang_session::elevated_probe::ProbeErrorCategory {
    use bangbang_session::elevated_probe::ProbeErrorCategory;
    use bangbang_session::macos::runtime::RuntimeError;

    match error {
        RuntimeError::Filesystem(kind) | RuntimeError::NamespaceCreate(kind) => {
            ProbeErrorCategory::from_io_kind(kind)
        }
        RuntimeError::InvalidRoot | RuntimeError::InvalidEntry | RuntimeError::Collision => {
            ProbeErrorCategory::InvalidInput
        }
    }
}

#[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
const fn runtime_transport_error_category(
    error: bangbang_session::macos::grant_transport::GrantTransportError,
) -> bangbang_session::elevated_probe::ProbeErrorCategory {
    use bangbang_session::elevated_probe::ProbeErrorCategory;
    use bangbang_session::macos::grant_transport::GrantTransportError;

    match error {
        GrantTransportError::Io(kind) => ProbeErrorCategory::from_io_kind(kind),
        GrantTransportError::Invalid => ProbeErrorCategory::InvalidInput,
    }
}

#[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
fn runtime_transport_is_empty(fd: libc::c_int) -> bool {
    let mut byte = 0_u8;
    // SAFETY: `byte` is writable for one non-consuming byte probe and `fd` is
    // the live connected evidence datagram endpoint.
    let result = unsafe {
        libc::recv(
            fd,
            (&raw mut byte).cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        return error
            .raw_os_error()
            .is_some_and(|code| code == libc::EAGAIN || code == libc::EWOULDBLOCK);
    }
    false
}

#[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
fn validate_runtime_worker(
    pid: libc::pid_t,
    expected: &crate::macos::code_sign::WorkerProfile,
) -> Result<(), bangbang_session::elevated_probe::ProbeErrorCategory> {
    use bangbang_session::elevated_probe::ProbeErrorCategory;

    match crate::macos::code_sign::validate_worker_process(pid) {
        Ok(profile) if &profile == expected => Ok(()),
        Ok(_) => Err(ProbeErrorCategory::InvalidInput),
        Err(_) => Err(ProbeErrorCategory::Other),
    }
}

#[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
fn credential_protocol_blocked_after_reap(
    spawned: &mut crate::macos::spawn::SuspendedWorker,
    prefix: bangbang_session::elevated_probe::CredentialPrefix,
    state: bangbang_session::elevated_probe::CredentialSelfState,
) -> LauncherExit {
    use bangbang_session::elevated_probe::CredentialRole;

    spawned.worker.terminate_and_reap();
    credential_blocked_value(
        CredentialRole::Launcher,
        credential_protocol_failure(prefix, state),
    )
}

#[cfg(all(target_os = "macos", feature = "elevated-bootstrap-probe"))]
fn credential_wait_for_exit(
    spawned: &mut crate::macos::spawn::SuspendedWorker,
    expected: u8,
) -> Result<(), LauncherError> {
    use std::time::{Duration, Instant};

    const WAIT_TIMEOUT: Duration = Duration::from_secs(5);
    const POLL_INTERVAL: Duration = Duration::from_millis(10);

    let result = (|| {
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
                return Err(LauncherError::SessionProtocol);
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    })();
    if result.is_err() {
        spawned.worker.terminate_and_reap();
    }
    result
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
#[derive(Clone, Copy)]
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
fn elevated_runtime_blocked(
    bootstrap: bangbang_session::elevated_probe::ProbeBootstrap,
    semantics: &CredentialSemantics,
    stage: bangbang_session::elevated_probe::ProbeStage,
    category: bangbang_session::elevated_probe::ProbeErrorCategory,
) -> LauncherExit {
    use bangbang_session::elevated_probe::{ProbeStage, RuntimeResultClass};

    let result = match stage {
        ProbeStage::ContinuationAck => RuntimeResultClass::ContinuationBoundary,
        ProbeStage::RuntimeNamespace
        | ProbeStage::RuntimeSessionCreate
        | ProbeStage::RuntimeSessionOpen
        | ProbeStage::RuntimeAuthoritySend
        | ProbeStage::RuntimeAuthorityReceive
        | ProbeStage::RuntimeAuthorityValidate
        | ProbeStage::RuntimeSessionLock
        | ProbeStage::RuntimeSessionEnter
        | ProbeStage::LifecyclePrepared => RuntimeResultClass::NamespaceBoundary,
        ProbeStage::GrantTransfer
        | ProbeStage::GrantAccepted
        | ProbeStage::GuestGrantContract
        | ProbeStage::GuestResourceWitness => RuntimeResultClass::GrantBoundary,
        ProbeStage::LifecycleHello
        | ProbeStage::LifecycleProceed
        | ProbeStage::LifecycleTerminal
        | ProbeStage::RuntimeCleanup => RuntimeResultClass::LifecycleBoundary,
        ProbeStage::ApiSocketPublication
        | ProbeStage::ApiListenerRequest
        | ProbeStage::ApiListenerBind
        | ProbeStage::ApiListenerTransfer
        | ProbeStage::ApiListenerAdoption
        | ProbeStage::ApiLoggerConfiguration
        | ProbeStage::ApiMetricsConfiguration
        | ProbeStage::ApiSerialConfiguration
        | ProbeStage::ApiMachineConfiguration
        | ProbeStage::ApiBootConfiguration
        | ProbeStage::ApiDriveConfiguration
        | ProbeStage::ApiInstanceStart => RuntimeResultClass::ApiBoundary,
        ProbeStage::GuestHvfWitness | ProbeStage::GuestHvfCreate => RuntimeResultClass::HvfBoundary,
        ProbeStage::NoApiStartup
        | ProbeStage::GuestExecution
        | ProbeStage::GuestOracle
        | ProbeStage::GuestPoweroff
        | ProbeStage::GuestTimeout
        | ProbeStage::GuestEndpointDeath
        | ProbeStage::GuestTerminalEvidence
        | ProbeStage::GuestCleanup => RuntimeResultClass::GuestBoundary,
        ProbeStage::InitialIdentity | ProbeStage::LiveIdentity => {
            RuntimeResultClass::IdentityBoundary
        }
        ProbeStage::TakeRoot
        | ProbeStage::ValidateRoot
        | ProbeStage::EnterRoot
        | ProbeStage::Chroot
        | ProbeStage::ChangeDirectory
        | ProbeStage::UnexpectedContinuation
        | ProbeStage::ValidateStagedBundle
        | ProbeStage::ValidateStagedLoader
        | ProbeStage::SpawnWorker
        | ProbeStage::SuspendedIdentity
        | ProbeStage::InheritedRoot
        | ProbeStage::SandboxChrootControl
        | ProbeStage::HvfCreate
        | ProbeStage::HvfDestroy
        | ProbeStage::WorkerBootstrap => RuntimeResultClass::ExplicitRootBoundary,
    };
    println!(
        "status: elevated runtime {} blocked stage={} error={} result={} stream-eid={} stream-cred={} stream-pid=exact datagram-cred={} datagram-token={} datagram-pid={}",
        bootstrap.mode().name(),
        stage.name(),
        category.name(),
        result.name(),
        semantics.stream_eid,
        semantics.stream_cred,
        semantics.datagram_cred,
        semantics.datagram_token,
        semantics.datagram_pid,
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
    fn credential_protocol_failure_preserves_each_completed_transition_prefix() {
        use bangbang_session::elevated_probe::{
            CredentialGroupClass, CredentialIdentityClass, CredentialPrefix, CredentialSelfState,
        };

        for (prefix, state) in [
            (
                CredentialPrefix::Irreversible,
                CredentialSelfState::new(
                    CredentialIdentityClass::Target,
                    CredentialGroupClass::EffectiveOnly,
                ),
            ),
            (
                CredentialPrefix::RetainedRoot,
                CredentialSelfState::new(
                    CredentialIdentityClass::InitialAndTarget,
                    CredentialGroupClass::Initial,
                ),
            ),
        ] {
            let failure = credential_protocol_failure(prefix, state);

            assert_eq!(failure.prefix(), prefix);
            assert_eq!(failure.state(), state);
        }
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
