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
        use std::os::fd::AsRawFd;

        use bangbang_session::{LauncherLifecycle, SessionId};

        let input = crate::grant_manifest::LaunchInput::parse(args.into_iter().collect())?;
        let executable = std::env::current_exe().map_err(|_| LauncherError::InvalidBundleLayout)?;
        let layout = BundleLayout::from_launcher_executable(&executable)?;
        crate::macos::code_sign::validate_bundle(&layout)?;
        let (worker_args, grants) = input.prepare()?;
        let wakeups = crate::macos::supervise::SignalWakeups::install()?;
        let session_id = SessionId::generate().map_err(|_| LauncherError::SessionProtocol)?;
        let mut lifecycle = LauncherLifecycle::new(session_id);
        let mut spawned =
            crate::macos::spawn::spawn_suspended(layout.worker_executable(), worker_args)?;
        crate::macos::code_sign::validate_worker_process(spawned.worker.pid())?;
        spawned.worker.resume()?;
        crate::macos::supervise::read_bootstrap_hello(&mut spawned.session, &mut lifecycle)?;
        bangbang_session::macos::verify_peer(spawned.session.as_raw_fd(), spawned.worker.pid())
            .map_err(|_| LauncherError::InvalidWorkerIdentity)?;
        crate::macos::code_sign::validate_worker_process(spawned.worker.pid())?;
        let start = lifecycle
            .start()
            .map_err(|_| LauncherError::SessionProtocol)?;
        crate::macos::supervise::write_frame(&mut spawned.session, start)?;
        let status = crate::macos::supervise::wait_session(
            &mut spawned.worker,
            &mut spawned.session,
            &mut spawned.grants,
            lifecycle,
            wakeups,
            &grants,
        )?;
        map_exit_status(status)
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = args;
        Err(LauncherError::UnsupportedPlatform)
    }
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
