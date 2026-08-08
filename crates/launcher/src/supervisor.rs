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
}
