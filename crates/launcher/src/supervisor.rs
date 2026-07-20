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
        let child_bootstrap = crate::macos::daemon::child_bootstrap()?;
        let timing = child_bootstrap
            .as_ref()
            .map_or_else(crate::launch_policy::LaunchTiming::sample, |bootstrap| {
                Ok(bootstrap.timing)
            })?;
        let command = crate::launch_policy::LaunchCommand::parse(args.into_iter().collect())?;
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
        launch_prepared(&layout, launch, None)
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = args;
        Err(LauncherError::UnsupportedPlatform)
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
