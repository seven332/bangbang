use std::env;
use std::ffi::{CString, OsStr, OsString, c_char};
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{UnixDatagram, UnixStream};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::ExitStatus;

use bangbang_session::{GRANT_FD, SESSION_ENV_KEY, SESSION_ENV_VALUE, SESSION_FD};

use crate::LauncherError;

const MIN_TRANSPORT_FD: RawFd = 10;

unsafe extern "C" {
    fn posix_spawn_file_actions_addinherit_np(
        actions: *mut libc::posix_spawn_file_actions_t,
        fd: libc::c_int,
    ) -> libc::c_int;
}

/// One suspended, owned, unreaped worker PID.
pub(crate) struct OwnedWorker {
    pid: libc::pid_t,
    status: Option<ExitStatus>,
}

impl std::fmt::Debug for OwnedWorker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OwnedWorker")
            .field("pid", &self.pid)
            .field("reaped", &self.status.is_some())
            .finish()
    }
}

impl OwnedWorker {
    pub(crate) fn pid(&self) -> libc::pid_t {
        self.pid
    }

    pub(crate) fn resume(&self) -> Result<(), LauncherError> {
        self.signal(libc::SIGCONT)
            .map_err(LauncherError::WorkerSpawn)
    }

    pub(crate) fn signal(&self, signal: libc::c_int) -> Result<(), io::ErrorKind> {
        if self.status.is_some() {
            return Err(io::ErrorKind::NotFound);
        }
        // SAFETY: `pid` remains an owned unreaped child and the signal value is
        // supplied by the fixed launcher lifecycle.
        if unsafe { libc::kill(self.pid, signal) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error.kind())
        }
    }

    pub(crate) fn try_wait(&mut self) -> Result<Option<ExitStatus>, LauncherError> {
        if let Some(status) = self.status {
            return Ok(Some(status));
        }
        let mut raw_status = 0;
        // SAFETY: `pid` is this object's unreaped child and `raw_status` is writable.
        let result = unsafe { libc::waitpid(self.pid, &raw mut raw_status, libc::WNOHANG) };
        if result == 0 {
            return Ok(None);
        }
        if result == self.pid {
            let status = ExitStatus::from_raw(raw_status);
            self.status = Some(status);
            return Ok(Some(status));
        }
        Err(LauncherError::WorkerWait(io::Error::last_os_error().kind()))
    }

    pub(crate) fn wait(&mut self) -> Result<ExitStatus, LauncherError> {
        if let Some(status) = self.status {
            return Ok(status);
        }
        loop {
            let mut raw_status = 0;
            // SAFETY: `pid` is this object's unreaped child and `raw_status` is writable.
            let result = unsafe { libc::waitpid(self.pid, &raw mut raw_status, 0) };
            if result == self.pid {
                let status = ExitStatus::from_raw(raw_status);
                self.status = Some(status);
                return Ok(status);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(LauncherError::WorkerWait(error.kind()));
            }
        }
    }

    pub(crate) fn terminate_and_reap(&mut self) {
        if !matches!(self.try_wait(), Ok(Some(_))) {
            let _ = self.signal(libc::SIGKILL);
        }
        let _ = self.wait();
    }
}

impl Drop for OwnedWorker {
    fn drop(&mut self) {
        self.terminate_and_reap();
    }
}

/// Result of the default-close initially suspended spawn.
#[derive(Debug)]
pub(crate) struct SuspendedWorker {
    pub(crate) worker: OwnedWorker,
    pub(crate) session: UnixStream,
    pub(crate) grants: UnixDatagram,
}

pub(crate) fn spawn_suspended(
    executable: &Path,
    args: Vec<OsString>,
) -> Result<SuspendedWorker, LauncherError> {
    let (parent, child) =
        UnixStream::pair().map_err(|error| LauncherError::SessionSetup(error.kind()))?;
    let parent = duplicate_stream_at_or_above(parent, MIN_TRANSPORT_FD)?;
    let child = duplicate_stream_at_or_above(child, MIN_TRANSPORT_FD)?;
    let (grant_parent, grant_child) =
        UnixDatagram::pair().map_err(|error| LauncherError::SessionSetup(error.kind()))?;
    let grant_parent = duplicate_datagram_at_or_above(grant_parent, MIN_TRANSPORT_FD)?;
    let grant_child = duplicate_datagram_at_or_above(grant_child, MIN_TRANSPORT_FD)?;

    let executable = cstring(executable.as_os_str())
        .map_err(|_| LauncherError::WorkerSpawn(io::ErrorKind::InvalidInput))?;
    let argv = argv(&executable, args)?;
    let env = environment()?;
    let argv_pointers = pointer_array(&argv);
    let env_pointers = pointer_array(&env);

    let mut attributes = SpawnAttributes::new()?;
    attributes.configure()?;
    let mut actions = SpawnFileActions::new()?;
    for fd in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO] {
        if descriptor_is_open(fd)? {
            actions.inherit(fd)?;
        }
    }
    actions.duplicate(child.as_raw_fd(), SESSION_FD)?;
    if child.as_raw_fd() != SESSION_FD {
        actions.close(child.as_raw_fd())?;
    }
    actions.duplicate(grant_child.as_raw_fd(), GRANT_FD)?;
    if grant_child.as_raw_fd() != GRANT_FD {
        actions.close(grant_child.as_raw_fd())?;
    }

    let mut pid = 0;
    // SAFETY: All C strings and null-terminated pointer arrays remain live for
    // the synchronous spawn. Attribute/action wrappers own initialized Darwin
    // objects and `pid` is writable for one result.
    let result = unsafe {
        libc::posix_spawn(
            &raw mut pid,
            executable.as_ptr(),
            actions.as_ptr(),
            attributes.as_ptr(),
            argv_pointers.as_ptr(),
            env_pointers.as_ptr(),
        )
    };
    if result != 0 {
        return Err(LauncherError::WorkerSpawn(
            io::Error::from_raw_os_error(result).kind(),
        ));
    }
    drop(child);
    drop(grant_child);
    Ok(SuspendedWorker {
        worker: OwnedWorker { pid, status: None },
        session: parent,
        grants: grant_parent,
    })
}

fn duplicate_stream_at_or_above(
    stream: UnixStream,
    minimum: RawFd,
) -> Result<UnixStream, LauncherError> {
    // SAFETY: The source remains live during `fcntl`; a successful result is an
    // independent close-on-exec descriptor.
    let fd = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_DUPFD_CLOEXEC, minimum) };
    if fd < 0 {
        return Err(LauncherError::SessionSetup(
            io::Error::last_os_error().kind(),
        ));
    }
    // SAFETY: `fd` is a fresh connected stream descriptor and ownership moves
    // into `OwnedFd`, then `UnixStream`.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    Ok(UnixStream::from(owned))
}

fn duplicate_datagram_at_or_above(
    datagram: UnixDatagram,
    minimum: RawFd,
) -> Result<UnixDatagram, LauncherError> {
    // SAFETY: The source remains live during fcntl; a successful result is an
    // independent close-on-exec descriptor for the same connected socket.
    let fd = unsafe { libc::fcntl(datagram.as_raw_fd(), libc::F_DUPFD_CLOEXEC, minimum) };
    if fd < 0 {
        return Err(LauncherError::SessionSetup(
            io::Error::last_os_error().kind(),
        ));
    }
    // SAFETY: fd is a fresh connected datagram descriptor and ownership moves
    // into OwnedFd, then UnixDatagram.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    Ok(UnixDatagram::from(owned))
}

fn descriptor_is_open(fd: RawFd) -> Result<bool, LauncherError> {
    // SAFETY: `F_GETFD` only reads flags from the supplied integer descriptor.
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EBADF) {
        Ok(false)
    } else {
        Err(LauncherError::SessionSetup(error.kind()))
    }
}

fn argv(executable: &CString, args: Vec<OsString>) -> Result<Vec<CString>, LauncherError> {
    std::iter::once(Ok(executable.clone()))
        .chain(args.into_iter().map(|argument| {
            cstring(&argument).map_err(|_| LauncherError::WorkerSpawn(io::ErrorKind::InvalidInput))
        }))
        .collect()
}

fn environment() -> Result<Vec<CString>, LauncherError> {
    environment_from(env::vars_os())
}

fn environment_from<I>(variables: I) -> Result<Vec<CString>, LauncherError>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    let key = OsStr::new(SESSION_ENV_KEY);
    let mut entries = variables
        .into_iter()
        .filter(|(name, _)| name != key)
        .map(|(name, value)| {
            let mut entry = Vec::with_capacity(name.len().saturating_add(value.len()) + 1);
            entry.extend_from_slice(name.as_bytes());
            entry.push(b'=');
            entry.extend_from_slice(value.as_bytes());
            CString::new(entry).map_err(|_| LauncherError::WorkerSpawn(io::ErrorKind::InvalidInput))
        })
        .collect::<Result<Vec<_>, _>>()?;
    entries.push(
        CString::new(format!("{SESSION_ENV_KEY}={SESSION_ENV_VALUE}"))
            .map_err(|_| LauncherError::WorkerSpawn(io::ErrorKind::InvalidInput))?,
    );
    Ok(entries)
}

fn pointer_array(values: &[CString]) -> Vec<*mut c_char> {
    values
        .iter()
        .map(|value| value.as_ptr().cast_mut())
        .chain(std::iter::once(std::ptr::null_mut()))
        .collect()
}

fn cstring(value: &OsStr) -> Result<CString, std::ffi::NulError> {
    CString::new(value.as_bytes())
}

struct SpawnAttributes {
    value: MaybeUninit<libc::posix_spawnattr_t>,
    initialized: bool,
}

impl SpawnAttributes {
    fn new() -> Result<Self, LauncherError> {
        let mut attributes = Self {
            value: MaybeUninit::uninit(),
            initialized: false,
        };
        // SAFETY: `value` is writable storage for one Darwin spawn attribute.
        cvt_spawn(unsafe { libc::posix_spawnattr_init(attributes.value.as_mut_ptr()) })?;
        attributes.initialized = true;
        Ok(attributes)
    }

    fn configure(&mut self) -> Result<(), LauncherError> {
        let mut defaults = MaybeUninit::<libc::sigset_t>::uninit();
        // SAFETY: `defaults` is writable for a signal set.
        if unsafe { libc::sigemptyset(defaults.as_mut_ptr()) } != 0 {
            return Err(LauncherError::SessionSetup(
                io::Error::last_os_error().kind(),
            ));
        }
        // SAFETY: Successful `sigemptyset` initialized the set; SIGPIPE is fixed.
        if unsafe { libc::sigaddset(defaults.as_mut_ptr(), libc::SIGPIPE) } != 0 {
            return Err(LauncherError::SessionSetup(
                io::Error::last_os_error().kind(),
            ));
        }
        // SAFETY: Attribute and signal-set pointers remain live for this call.
        cvt_spawn(unsafe {
            libc::posix_spawnattr_setsigdefault(self.value.as_mut_ptr(), defaults.as_ptr())
        })?;
        let flags = libc::POSIX_SPAWN_CLOEXEC_DEFAULT
            | libc::POSIX_SPAWN_START_SUSPENDED
            | libc::POSIX_SPAWN_SETSIGDEF;
        let flags = libc::c_short::try_from(flags)
            .map_err(|_| LauncherError::SessionSetup(io::ErrorKind::InvalidInput))?;
        // SAFETY: This wrapper owns one initialized attribute object.
        cvt_spawn(unsafe { libc::posix_spawnattr_setflags(self.value.as_mut_ptr(), flags) })
    }

    fn as_ptr(&self) -> *const libc::posix_spawnattr_t {
        self.value.as_ptr()
    }
}

impl Drop for SpawnAttributes {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: This wrapper owns one initialized attribute object.
            let _ = unsafe { libc::posix_spawnattr_destroy(self.value.as_mut_ptr()) };
        }
    }
}

struct SpawnFileActions {
    value: MaybeUninit<libc::posix_spawn_file_actions_t>,
    initialized: bool,
}

impl SpawnFileActions {
    fn new() -> Result<Self, LauncherError> {
        let mut actions = Self {
            value: MaybeUninit::uninit(),
            initialized: false,
        };
        // SAFETY: `value` is writable storage for one Darwin action object.
        cvt_spawn(unsafe { libc::posix_spawn_file_actions_init(actions.value.as_mut_ptr()) })?;
        actions.initialized = true;
        Ok(actions)
    }

    fn inherit(&mut self, fd: RawFd) -> Result<(), LauncherError> {
        // SAFETY: This wrapper owns one initialized action object and `fd` is open.
        cvt_spawn(unsafe { posix_spawn_file_actions_addinherit_np(self.value.as_mut_ptr(), fd) })
    }

    fn duplicate(&mut self, source: RawFd, destination: RawFd) -> Result<(), LauncherError> {
        // SAFETY: This wrapper owns one initialized action object; descriptors are integers
        // interpreted by the child-side spawn implementation.
        cvt_spawn(unsafe {
            libc::posix_spawn_file_actions_adddup2(self.value.as_mut_ptr(), source, destination)
        })
    }

    fn close(&mut self, fd: RawFd) -> Result<(), LauncherError> {
        // SAFETY: This wrapper owns one initialized action object; `fd` is interpreted in child.
        cvt_spawn(unsafe { libc::posix_spawn_file_actions_addclose(self.value.as_mut_ptr(), fd) })
    }

    fn as_ptr(&self) -> *const libc::posix_spawn_file_actions_t {
        self.value.as_ptr()
    }
}

impl Drop for SpawnFileActions {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: This wrapper owns one initialized action object.
            let _ = unsafe { libc::posix_spawn_file_actions_destroy(self.value.as_mut_ptr()) };
        }
    }
}

fn cvt_spawn(result: libc::c_int) -> Result<(), LauncherError> {
    if result == 0 {
        Ok(())
    } else {
        Err(LauncherError::SessionSetup(
            io::Error::from_raw_os_error(result).kind(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_environment_value_replaces_inherited_copy() {
        let entries = environment_from([
            (OsString::from("ORDINARY"), OsString::from("value")),
            (OsString::from(SESSION_ENV_KEY), OsString::from("forged")),
        ])
        .expect("environment should encode");
        let matching = entries
            .iter()
            .filter(|entry| entry.as_bytes().starts_with(SESSION_ENV_KEY.as_bytes()))
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1);
        assert_eq!(
            matching[0].as_bytes(),
            format!("{SESSION_ENV_KEY}={SESSION_ENV_VALUE}").as_bytes()
        );
    }
}
