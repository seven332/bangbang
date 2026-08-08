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
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::time::Duration;

    use bangbang_session::elevated_probe::{ProbeResult, READY_RECORD, RESULT_RECORD_BYTES};

    const TIMEOUT: Duration = Duration::from_secs(5);

    let bootstrap = elevated_probe.bootstrap()?;
    let mut spawned = crate::macos::spawn::spawn_suspended_elevated(
        layout.worker_executable(),
        launch.worker_args,
        elevated_probe.root_fd(),
    )?;
    if crate::macos::code_sign::validate_worker_process(spawned.worker.pid())?
        != launch.worker_profile
    {
        return Err(LauncherError::InvalidWorkerIdentity);
    }
    spawned
        .session
        .set_read_timeout(Some(TIMEOUT))
        .map_err(|error| LauncherError::SessionSetup(error.kind()))?;
    spawned
        .session
        .set_write_timeout(Some(TIMEOUT))
        .map_err(|error| LauncherError::SessionSetup(error.kind()))?;
    spawned.worker.resume()?;
    let mut ready = [0_u8; READY_RECORD.len()];
    spawned
        .session
        .read_exact(&mut ready)
        .map_err(|_| LauncherError::SessionProtocol)?;
    if ready != READY_RECORD {
        return Err(LauncherError::SessionProtocol);
    }
    bangbang_session::macos::verify_peer(spawned.session.as_raw_fd(), spawned.worker.pid())
        .map_err(|_| LauncherError::InvalidWorkerIdentity)?;
    if crate::macos::code_sign::validate_worker_process(spawned.worker.pid())?
        != launch.worker_profile
    {
        return Err(LauncherError::InvalidWorkerIdentity);
    }
    spawned
        .session
        .write_all(&bootstrap.encode())
        .map_err(|_| LauncherError::SessionProtocol)?;
    let mut encoded_result = [0_u8; RESULT_RECORD_BYTES];
    spawned
        .session
        .read_exact(&mut encoded_result)
        .map_err(|_| LauncherError::SessionProtocol)?;
    let result =
        ProbeResult::decode(&encoded_result).map_err(|_| LauncherError::SessionProtocol)?;
    if result.mode() != bootstrap.mode() || result.nonce() != bootstrap.nonce() {
        return Err(LauncherError::SessionProtocol);
    }
    let status = spawned.worker.wait()?;
    let exit = map_exit_status(status)?;
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
        Ok(()) | Err(_) => Err(LauncherError::SessionProtocol),
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
