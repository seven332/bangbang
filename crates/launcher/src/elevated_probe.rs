use std::ffi::{CStr, OsStr, OsString};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use bangbang_session::elevated_probe::{
    LAUNCHER_ACTIVATION, ProbeBootstrap, ProbeErrorCategory, ProbeMode, ProbeStage, RuntimeFault,
    RuntimeWorkload, WORKER_ACTIVATION,
};
use bangbang_session::macos::runtime::ExplicitRuntimeRoot;
use bangbang_session::{ObjectIdentity, SessionId};

use crate::grant_manifest::{ElevatedGuestContract, PreparedGrantBatch};

use crate::{BundleLayout, LauncherError};

const ROOT_OPTION: &str = "--root";
const TARGET_UID_OPTION: &str = "--target-uid";
const TARGET_GID_OPTION: &str = "--target-gid";
const MODE_OPTION: &str = "--mode";
const FAULT_OPTION: &str = "--fault";
const ADOPTION_BARRIER_OPTION: &str = "--bangbang-internal-post-adoption-stop-v1";
const DAEMON_BARRIER_OPTION: &str = "--bangbang-internal-daemon-barrier-v1";
const DAEMON_FAULT_OPTION: &str = "--bangbang-internal-daemon-fault-v1";
const DELIMITER: &str = "--";
const MAX_ROOT_PATH_BYTES: usize = 1024;
const ROOT_CHILD_PREFIX: &str = "bangbang-elevated-probe.";
const ROOT_CHILD_SUFFIX_BYTES: usize = 8;
const STAGED_LAUNCHER_PATH: &str = "Bangbang.app/Contents/MacOS/bangbang";
const IN_ROOT_WORKER_PATH: &str =
    "/Bangbang.app/Contents/Helpers/BangbangWorker.app/Contents/MacOS/bangbang-worker";
const MAX_STAGED_DYLD_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProbeFailure {
    pub(crate) stage: ProbeStage,
    pub(crate) category: ProbeErrorCategory,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum DaemonBarrier {
    #[default]
    None,
    ParentBeforeTransitionAck,
    ChildAfterTransitionAck,
    ParentAfterReady,
    PostAckWatch,
    ParentAfterAck,
    NamespaceRetirement,
}

impl DaemonBarrier {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "parent-before-transition-ack" => Some(Self::ParentBeforeTransitionAck),
            "child-after-transition-ack" => Some(Self::ChildAfterTransitionAck),
            "parent-after-ready" => Some(Self::ParentAfterReady),
            "post-ack-watch" => Some(Self::PostAckWatch),
            "parent-after-ack" => Some(Self::ParentAfterAck),
            "daemon-namespace-retirement" => Some(Self::NamespaceRetirement),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum DaemonFault {
    #[default]
    None,
    RootAdoption,
    TransitionWitness,
    TransitionAck,
    SessionBind,
    ReadySend,
    ReadyAck,
}

impl DaemonFault {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "root-adoption" => Some(Self::RootAdoption),
            "transition-witness" => Some(Self::TransitionWitness),
            "transition-ack" => Some(Self::TransitionAck),
            "session-bind" => Some(Self::SessionBind),
            "ready-send" => Some(Self::ReadySend),
            "ready-ack" => Some(Self::ReadyAck),
            _ => None,
        }
    }
}

pub(crate) struct Config {
    activation: Vec<OsString>,
    root: OwnedFd,
    root_path: PathBuf,
    root_identity: ObjectIdentity,
    runtime_root: Option<ExplicitRuntimeRoot>,
    staged_loader_identity: Option<ObjectIdentity>,
    target_uid: u32,
    target_gid: u32,
    mode: ProbeMode,
    fault: RuntimeFault,
    adoption_barrier: bool,
    daemon_barrier: DaemonBarrier,
    daemon_fault: DaemonFault,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ElevatedProbeConfig(<redacted>)")
    }
}

impl Config {
    pub(crate) fn parse(
        args: Vec<OsString>,
    ) -> Result<(Option<Self>, Vec<OsString>), LauncherError> {
        Self::parse_inner(args, None)
    }

    pub(crate) fn parse_daemon(
        args: Vec<OsString>,
        root: OwnedFd,
    ) -> Result<(Option<Self>, Vec<OsString>), LauncherError> {
        Self::parse_inner(args, Some(root))
    }

    fn parse_inner(
        args: Vec<OsString>,
        inherited_root: Option<OwnedFd>,
    ) -> Result<(Option<Self>, Vec<OsString>), LauncherError> {
        if args
            .first()
            .is_none_or(|arg| arg != OsStr::new(LAUNCHER_ACTIVATION))
        {
            if inherited_root.is_some() {
                return Err(LauncherError::InvalidLaunchPolicy);
            }
            return Ok((None, args));
        }
        if args.len() < 10
            || args.get(1) != Some(&OsString::from(ROOT_OPTION))
            || args.get(3) != Some(&OsString::from(TARGET_UID_OPTION))
            || args.get(5) != Some(&OsString::from(TARGET_GID_OPTION))
            || args.get(7) != Some(&OsString::from(MODE_OPTION))
        {
            return Err(LauncherError::InvalidLaunchPolicy);
        }
        let root_path = args
            .get(2)
            .filter(|value| {
                !value.is_empty()
                    && value.as_bytes().len() <= MAX_ROOT_PATH_BYTES
                    && !value.as_bytes().contains(&0)
                    && value.to_str().is_some()
            })
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or(LauncherError::InvalidLaunchPolicy)?;
        let target_uid = parse_u32(args.get(4))?;
        let target_gid = parse_u32(args.get(6))?;
        let mode = args
            .get(8)
            .and_then(|value| value.to_str())
            .and_then(|value| ProbeMode::parse(value, target_uid, target_gid))
            .ok_or(LauncherError::InvalidLaunchPolicy)?;
        let (fault, adoption_barrier, daemon_barrier, daemon_fault, tail_delimiter) =
            parse_runtime_options(
                args.get(9..).ok_or(LauncherError::InvalidLaunchPolicy)?,
                mode,
            )?;
        let delimiter = 9_usize
            .checked_add(tail_delimiter)
            .ok_or(LauncherError::InvalidLaunchPolicy)?;
        if !mode.continues_runtime() && fault != RuntimeFault::None {
            return Err(LauncherError::InvalidLaunchPolicy);
        }
        if !mode.continues_runtime() && delimiter + 1 != args.len() {
            return Err(LauncherError::InvalidLaunchPolicy);
        }
        if inherited_root.is_some() && !mode.continues_runtime() {
            return Err(LauncherError::InvalidLaunchPolicy);
        }
        validate_initial_root()?;
        let (linked_root, root_identity) = open_private_root(
            &root_path,
            if mode.continues_runtime() {
                (target_uid, target_gid)
            } else {
                (0, 0)
            },
        )?;
        let root = match inherited_root {
            Some(root) => {
                validate_inherited_root(
                    &root,
                    root_identity,
                    if mode.continues_runtime() {
                        (target_uid, target_gid)
                    } else {
                        (0, 0)
                    },
                )?;
                drop(linked_root);
                root
            }
            None => linked_root,
        };
        let runtime_root = if mode.continues_runtime() {
            let independent = openat_directory(root.as_raw_fd(), c".")?;
            Some(
                ExplicitRuntimeRoot::from_owned_fd(
                    independent,
                    root_identity,
                    target_uid,
                    target_gid,
                    true,
                )
                .map_err(|_| LauncherError::InvalidLaunchPolicy)?,
            )
        } else {
            None
        };
        let staged_loader_identity = if mode == ProbeMode::InheritedRoot {
            Some(validate_staged_loader(root.as_raw_fd(), None)?)
        } else {
            None
        };
        Ok((
            Some(Self {
                activation: args
                    .get(..=delimiter)
                    .ok_or(LauncherError::InvalidLaunchPolicy)?
                    .to_vec(),
                root,
                root_path,
                root_identity,
                runtime_root,
                staged_loader_identity,
                target_uid,
                target_gid,
                mode,
                fault,
                adoption_barrier,
                daemon_barrier,
                daemon_fault,
            }),
            args.into_iter().skip(delimiter + 1).collect(),
        ))
    }

    pub(crate) fn daemon_args(&self, request: &[OsString]) -> Result<Vec<OsString>, LauncherError> {
        if !self.mode.continues_runtime() || self.activation.is_empty() {
            return Err(LauncherError::InvalidLaunchPolicy);
        }
        let mut args = self.activation.clone();
        args.extend_from_slice(request);
        Ok(args)
    }

    pub(crate) fn validate_daemon(&self) -> Result<(), LauncherError> {
        if self.mode.continues_runtime() && !self.adoption_barrier {
            Ok(())
        } else {
            Err(LauncherError::InvalidLaunchPolicy)
        }
    }

    pub(crate) fn validate_foreground(&self) -> Result<(), LauncherError> {
        if self.daemon_barrier == DaemonBarrier::None
            && self.daemon_fault == DaemonFault::None
            && !matches!(
                self.fault,
                RuntimeFault::NamespaceRetireBeforeUnlink
                    | RuntimeFault::NamespaceRetireAfterUnlink
                    | RuntimeFault::NamespaceRetireObserve
                    | RuntimeFault::NamespaceRecordWrite
            )
        {
            Ok(())
        } else {
            Err(LauncherError::InvalidLaunchPolicy)
        }
    }

    pub(crate) fn prepend_worker_activation(
        &self,
        worker_args: &mut Vec<OsString>,
    ) -> Result<(), LauncherError> {
        if !self.mode.continues_runtime() && !worker_args.is_empty() {
            return Err(LauncherError::InvalidLaunchPolicy);
        }
        worker_args.insert(0, OsString::from(WORKER_ACTIVATION));
        Ok(())
    }

    pub(crate) fn validate_runtime_contract(
        &self,
        worker_args: &[OsString],
        grants: &PreparedGrantBatch,
        daemonized: bool,
    ) -> Result<Option<ElevatedGuestContract>, LauncherError> {
        use bangbang_session::elevated_probe::{
            GUEST_API_SOCKET_REFERENCE, GUEST_CONFIG_REFERENCE, RuntimeWorkload,
        };

        let workload = self
            .mode
            .runtime_workload()
            .ok_or(LauncherError::InvalidLaunchPolicy)?;
        let exact_args: &[&str] = match workload {
            RuntimeWorkload::RepresentativeGrants => {
                &["--bangbang-internal-grant-probe-v1", "target-runtime"]
            }
            RuntimeWorkload::GuestNoApi => &["--config-file", GUEST_CONFIG_REFERENCE, "--no-api"],
            RuntimeWorkload::GuestApi => &["--api-sock", GUEST_API_SOCKET_REFERENCE],
        };
        let worker_args = if daemonized {
            let prefix = worker_args
                .get(..8)
                .ok_or(LauncherError::InvalidLaunchPolicy)?;
            let [
                id_option,
                id,
                start_option,
                start,
                cpu_option,
                cpu,
                parent_option,
                parent_cpu,
            ] = prefix
            else {
                return Err(LauncherError::InvalidLaunchPolicy);
            };
            if id_option != OsStr::new("--id")
                || id.is_empty()
                || id.as_bytes().contains(&0)
                || start_option != OsStr::new("--start-time-us")
                || !canonical_decimal(start)
                || cpu_option != OsStr::new("--start-time-cpu-us")
                || cpu != OsStr::new("0")
                || parent_option != OsStr::new("--parent-cpu-time-us")
                || !canonical_decimal(parent_cpu)
            {
                return Err(LauncherError::InvalidLaunchPolicy);
            }
            worker_args
                .get(8..)
                .ok_or(LauncherError::InvalidLaunchPolicy)?
        } else {
            worker_args
        };
        if worker_args.len() != exact_args.len()
            || worker_args
                .iter()
                .zip(exact_args)
                .any(|(actual, expected)| actual != OsStr::new(expected))
        {
            return Err(LauncherError::InvalidLaunchPolicy);
        }
        grants.validate_elevated_runtime_contract(workload, self.target_uid, self.target_gid)
    }

    pub(crate) fn stop_after_adoption(&self) -> Result<bool, LauncherError> {
        if !self.adoption_barrier {
            return Ok(false);
        }
        if self.fault != RuntimeFault::None
            || self
                .mode
                .runtime_workload()
                .is_none_or(|workload| !workload.is_guest())
        {
            return Err(LauncherError::InvalidLaunchPolicy);
        }
        // SAFETY: `SIGSTOP` has fixed kernel semantics and takes no pointer.
        // This feature-only launcher is still single-threaded and has not
        // spawned its worker. The exact-root certifier resumes this same PID
        // only after adversarially replacing the already adopted pathnames.
        if unsafe { libc::raise(libc::SIGSTOP) } != 0 {
            return Err(LauncherError::SessionProtocol);
        }
        Ok(true)
    }

    pub(crate) fn root_fd(&self) -> RawFd {
        self.root.as_raw_fd()
    }

    pub(crate) const fn root_identity(&self) -> ObjectIdentity {
        self.root_identity
    }

    pub(crate) const fn target_uid(&self) -> u32 {
        self.target_uid
    }

    pub(crate) const fn target_gid(&self) -> u32 {
        self.target_gid
    }

    pub(crate) const fn daemon_barrier(&self) -> DaemonBarrier {
        self.daemon_barrier
    }

    pub(crate) const fn daemon_fault(&self) -> DaemonFault {
        self.daemon_fault
    }

    pub(crate) const fn mode(&self) -> ProbeMode {
        self.mode
    }

    pub(crate) fn take_runtime_root(&mut self) -> Result<ExplicitRuntimeRoot, LauncherError> {
        self.runtime_root
            .take()
            .ok_or(LauncherError::InvalidLaunchPolicy)
    }

    pub(crate) fn staged_layout(&self) -> Result<BundleLayout, LauncherError> {
        if self.mode != ProbeMode::InheritedRoot {
            return Err(LauncherError::InvalidLaunchPolicy);
        }
        BundleLayout::from_launcher_executable(&self.root_path.join(STAGED_LAUNCHER_PATH))
    }

    pub(crate) fn validate_staged_loader(&self) -> Result<(), LauncherError> {
        let expected = self
            .staged_loader_identity
            .ok_or(LauncherError::InvalidLaunchPolicy)?;
        validate_staged_loader(self.root.as_raw_fd(), Some(expected)).map(|_| ())
    }

    pub(crate) fn in_root_worker(&self) -> Result<&'static Path, LauncherError> {
        if self.mode == ProbeMode::InheritedRoot {
            Ok(Path::new(IN_ROOT_WORKER_PATH))
        } else {
            Err(LauncherError::InvalidLaunchPolicy)
        }
    }

    pub(crate) fn bootstrap(&self) -> Result<ProbeBootstrap, LauncherError> {
        let nonce = SessionId::generate().map_err(|_| LauncherError::SessionProtocol)?;
        ProbeBootstrap::new_with_fault(
            self.mode,
            self.fault,
            self.target_uid,
            self.target_gid,
            self.root_identity,
            nonce,
        )
        .map_err(|_| LauncherError::SessionProtocol)
    }

    pub(crate) fn run_control(&self) -> Result<(), LauncherError> {
        if self.mode != ProbeMode::Control {
            return Err(LauncherError::InvalidLaunchPolicy);
        }
        // SAFETY: `root` is the exact retained private directory validated by
        // descriptor and this root-only control process is single-threaded.
        if unsafe { libc::fchdir(self.root.as_raw_fd()) } != 0 {
            return Err(LauncherError::SessionProtocol);
        }
        // SAFETY: Cwd is the retained root and the fixed relative string is
        // NUL-terminated. This signed launcher intentionally exits afterward.
        if unsafe { libc::chroot(c".".as_ptr()) } != 0 {
            return Err(LauncherError::SessionProtocol);
        }
        // SAFETY: The process is inside the private root and the fixed absolute
        // path is NUL-terminated.
        if unsafe { libc::chdir(c"/".as_ptr()) } != 0 {
            return Err(LauncherError::SessionProtocol);
        }
        Ok(())
    }

    pub(crate) fn run_credential_control(
        &self,
    ) -> Result<
        bangbang_session::elevated_credential::CredentialTransition,
        bangbang_session::elevated_probe::CredentialFailureValue,
    > {
        use bangbang_session::elevated_probe::{
            CredentialFailureValue, CredentialGroupClass, CredentialIdentityClass,
            CredentialPrefix, CredentialSelfState, CredentialStep,
        };

        if self.mode != ProbeMode::CredentialControl {
            return Err(CredentialFailureValue::new(
                CredentialStep::InitialIdentity,
                ProbeErrorCategory::InvalidInput,
                CredentialPrefix::None,
                CredentialSelfState::new(
                    CredentialIdentityClass::Other,
                    CredentialGroupClass::Other,
                ),
            ));
        }
        bangbang_session::elevated_credential::transition_process(
            self.mode,
            self.target_uid,
            self.target_gid,
        )
    }

    pub(crate) fn enter_inherited_root(&self) -> Result<(), ProbeFailure> {
        if self.mode != ProbeMode::InheritedRoot {
            return Err(ProbeFailure {
                stage: ProbeStage::EnterRoot,
                category: ProbeErrorCategory::InvalidInput,
            });
        }
        self.validate_staged_loader().map_err(|_| ProbeFailure {
            stage: ProbeStage::ValidateStagedLoader,
            category: ProbeErrorCategory::InvalidInput,
        })?;
        ordered_root_transition(
            || {
                // SAFETY: `root` is the exact retained private directory
                // validated by descriptor and this feature-gated launcher has
                // not started workers.
                cvt_root_syscall(unsafe { libc::fchdir(self.root.as_raw_fd()) })
            },
            || {
                // SAFETY: Cwd is the retained exact root and the fixed relative
                // string is NUL-terminated. The process intentionally never
                // escapes this root.
                cvt_root_syscall(unsafe { libc::chroot(c".".as_ptr()) })
            },
            || {
                // SAFETY: The process is inside the private root and the fixed
                // path is NUL-terminated.
                cvt_root_syscall(unsafe { libc::chdir(c"/".as_ptr()) })
            },
            || {
                let slash = open_directory(c"/").map_err(|_| ProbeErrorCategory::Other)?;
                let stat =
                    descriptor_stat(slash.as_raw_fd()).map_err(|_| ProbeErrorCategory::Other)?;
                if stat.st_mode & libc::S_IFMT != libc::S_IFDIR
                    || stat.st_mode & 0o7777 != 0o700
                    || stat.st_uid != 0
                    || stat.st_gid != 0
                    || stat_identity(&stat) != self.root_identity
                {
                    return Err(ProbeErrorCategory::InvalidInput);
                }
                Ok(())
            },
        )
    }
}

fn canonical_decimal(value: &OsStr) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.iter().all(u8::is_ascii_digit)
        && (bytes.len() == 1 || bytes.first() != Some(&b'0'))
}

fn cvt_root_syscall(result: libc::c_int) -> Result<(), ProbeErrorCategory> {
    if result == 0 {
        Ok(())
    } else {
        Err(ProbeErrorCategory::from_io_kind(
            std::io::Error::last_os_error().kind(),
        ))
    }
}

fn ordered_root_transition<F, C, D, V>(
    fchdir: F,
    chroot: C,
    chdir: D,
    validate: V,
) -> Result<(), ProbeFailure>
where
    F: FnOnce() -> Result<(), ProbeErrorCategory>,
    C: FnOnce() -> Result<(), ProbeErrorCategory>,
    D: FnOnce() -> Result<(), ProbeErrorCategory>,
    V: FnOnce() -> Result<(), ProbeErrorCategory>,
{
    fchdir().map_err(|category| ProbeFailure {
        stage: ProbeStage::EnterRoot,
        category,
    })?;
    chroot().map_err(|category| ProbeFailure {
        stage: ProbeStage::Chroot,
        category,
    })?;
    chdir().map_err(|category| ProbeFailure {
        stage: ProbeStage::ChangeDirectory,
        category,
    })?;
    validate().map_err(|category| ProbeFailure {
        stage: ProbeStage::InheritedRoot,
        category,
    })?;
    Ok(())
}

fn validate_staged_loader(
    root: RawFd,
    expected: Option<ObjectIdentity>,
) -> Result<ObjectIdentity, LauncherError> {
    let usr = openat_directory(root, c"usr")?;
    let lib = openat_directory(usr.as_raw_fd(), c"lib")?;
    let loader = openat_plain_file(lib.as_raw_fd(), c"dyld")?;
    let stat = descriptor_stat(loader.as_raw_fd())?;
    validate_staged_loader_stat(&stat, expected)
}

fn validate_staged_loader_stat(
    stat: &libc::stat,
    expected: Option<ObjectIdentity>,
) -> Result<ObjectIdentity, LauncherError> {
    let identity = stat_identity(stat);
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG
        || stat.st_mode & 0o022 != 0
        || stat.st_mode & 0o111 == 0
        || stat.st_uid != 0
        || stat.st_gid != 0
        || stat.st_size <= 0
        || u64::try_from(stat.st_size)
            .ok()
            .is_none_or(|size| size > MAX_STAGED_DYLD_BYTES)
        || identity.device == 0
        || identity.inode == 0
        || expected.is_some_and(|expected| expected != identity)
    {
        return Err(LauncherError::InvalidLaunchPolicy);
    }
    Ok(identity)
}

fn parse_u32(value: Option<&OsString>) -> Result<u32, LauncherError> {
    value
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .filter(|value| value.len() == 1 || !value.starts_with('0'))
        .and_then(|value| value.parse().ok())
        .ok_or(LauncherError::InvalidLaunchPolicy)
}

fn parse_runtime_options(
    args: &[OsString],
    mode: ProbeMode,
) -> Result<(RuntimeFault, bool, DaemonBarrier, DaemonFault, usize), LauncherError> {
    if args.first() == Some(&OsString::from(DELIMITER)) {
        return Ok((
            RuntimeFault::None,
            false,
            DaemonBarrier::None,
            DaemonFault::None,
            0,
        ));
    }
    if mode.continues_runtime()
        && args.first() == Some(&OsString::from(FAULT_OPTION))
        && args.get(2) == Some(&OsString::from(DAEMON_BARRIER_OPTION))
        && args.get(4) == Some(&OsString::from(DELIMITER))
    {
        let fault = args
            .get(1)
            .and_then(|value| value.to_str())
            .and_then(RuntimeFault::parse)
            .filter(|fault| *fault == RuntimeFault::GuestEndpointDeath)
            .ok_or(LauncherError::InvalidLaunchPolicy)?;
        let barrier = args
            .get(3)
            .and_then(|value| value.to_str())
            .and_then(DaemonBarrier::parse)
            .filter(|barrier| *barrier == DaemonBarrier::PostAckWatch)
            .ok_or(LauncherError::InvalidLaunchPolicy)?;
        if mode
            .runtime_workload()
            .is_none_or(|workload| !workload.is_guest())
        {
            return Err(LauncherError::InvalidLaunchPolicy);
        }
        return Ok((fault, false, barrier, DaemonFault::None, 4));
    }
    if mode.continues_runtime()
        && args.first() == Some(&OsString::from(FAULT_OPTION))
        && args.get(2) == Some(&OsString::from(DELIMITER))
    {
        let fault = args
            .get(1)
            .and_then(|value| value.to_str())
            .and_then(RuntimeFault::parse)
            .ok_or(LauncherError::InvalidLaunchPolicy)?;
        return Ok((fault, false, DaemonBarrier::None, DaemonFault::None, 2));
    }
    if mode
        .runtime_workload()
        .is_some_and(RuntimeWorkload::is_guest)
        && args.first() == Some(&OsString::from(ADOPTION_BARRIER_OPTION))
        && args.get(1) == Some(&OsString::from(DELIMITER))
    {
        return Ok((
            RuntimeFault::None,
            true,
            DaemonBarrier::None,
            DaemonFault::None,
            1,
        ));
    }
    if mode.continues_runtime()
        && args.first() == Some(&OsString::from(DAEMON_BARRIER_OPTION))
        && args.get(2) == Some(&OsString::from(DELIMITER))
    {
        let barrier = args
            .get(1)
            .and_then(|value| value.to_str())
            .and_then(DaemonBarrier::parse)
            .ok_or(LauncherError::InvalidLaunchPolicy)?;
        return Ok((RuntimeFault::None, false, barrier, DaemonFault::None, 2));
    }
    if mode.continues_runtime()
        && args.first() == Some(&OsString::from(DAEMON_FAULT_OPTION))
        && args.get(2) == Some(&OsString::from(DELIMITER))
    {
        let fault = args
            .get(1)
            .and_then(|value| value.to_str())
            .and_then(DaemonFault::parse)
            .ok_or(LauncherError::InvalidLaunchPolicy)?;
        return Ok((RuntimeFault::None, false, DaemonBarrier::None, fault, 2));
    }
    Err(LauncherError::InvalidLaunchPolicy)
}

fn validate_initial_root() -> Result<(), LauncherError> {
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
        Err(LauncherError::InvalidLaunchPolicy)
    }
}

fn open_private_root(
    path: &Path,
    expected_owner: (u32, u32),
) -> Result<(OwnedFd, ObjectIdentity), LauncherError> {
    let child = private_root_child(path).ok_or(LauncherError::InvalidLaunchPolicy)?;
    let mut directory = open_directory(c"/")?;
    validate_ancestor(directory.as_raw_fd())?;
    for component in [c"private", c"var", c"root"] {
        directory = openat_directory(directory.as_raw_fd(), component)?;
        validate_ancestor(directory.as_raw_fd())?;
    }
    let child =
        std::ffi::CString::new(child.as_bytes()).map_err(|_| LauncherError::InvalidLaunchPolicy)?;
    let root = openat_directory(directory.as_raw_fd(), &child)?;
    let stat = descriptor_stat(root.as_raw_fd())?;
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR
        || stat.st_mode & 0o7777 != 0o700
        || stat.st_uid != expected_owner.0
        || stat.st_gid != expected_owner.1
        || stat.st_nlink < 2
    {
        return Err(LauncherError::InvalidLaunchPolicy);
    }
    let identity = stat_identity(&stat);
    if identity.device == 0 || identity.inode == 0 {
        return Err(LauncherError::InvalidLaunchPolicy);
    }
    Ok((root, identity))
}

fn validate_inherited_root(
    root: &OwnedFd,
    expected_identity: ObjectIdentity,
    expected_owner: (u32, u32),
) -> Result<(), LauncherError> {
    let stat = descriptor_stat(root.as_raw_fd())?;
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR
        || stat.st_mode & 0o7777 != 0o700
        || stat.st_uid != expected_owner.0
        || stat.st_gid != expected_owner.1
        || stat.st_nlink < 2
        || stat_identity(&stat) != expected_identity
    {
        return Err(LauncherError::InvalidLaunchPolicy);
    }
    // SAFETY: `root` is live and `F_GETFD` only returns descriptor flags.
    let flags = unsafe { libc::fcntl(root.as_raw_fd(), libc::F_GETFD) };
    if flags < 0 || flags & libc::FD_CLOEXEC == 0 {
        return Err(LauncherError::InvalidLaunchPolicy);
    }
    Ok(())
}

fn private_root_child(path: &Path) -> Option<&OsStr> {
    let mut components = path.components();
    if components.next() != Some(Component::RootDir)
        || components.next() != Some(Component::Normal(OsStr::new("private")))
        || components.next() != Some(Component::Normal(OsStr::new("var")))
        || components.next() != Some(Component::Normal(OsStr::new("root")))
    {
        return None;
    }
    let child = match components.next()? {
        Component::Normal(child) => child,
        _ => return None,
    };
    if components.next().is_some() {
        return None;
    }
    let child = child.to_str()?;
    let suffix = child.strip_prefix(ROOT_CHILD_PREFIX)?;
    if suffix.len() != ROOT_CHILD_SUFFIX_BYTES
        || !suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return None;
    }
    Some(OsStr::new(child))
}

fn open_directory(path: &CStr) -> Result<OwnedFd, LauncherError> {
    // SAFETY: `path` is NUL-terminated and no pointer is retained.
    let descriptor = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    owned_descriptor(descriptor)
}

fn openat_directory(parent: RawFd, child: &CStr) -> Result<OwnedFd, LauncherError> {
    // SAFETY: `parent` is a retained directory, `child` is NUL-terminated, and
    // no pointer is retained.
    let descriptor = unsafe {
        libc::openat(
            parent,
            child.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    owned_descriptor(descriptor)
}

fn openat_plain_file(parent: RawFd, child: &CStr) -> Result<OwnedFd, LauncherError> {
    // SAFETY: `parent` is a retained directory, `child` is NUL-terminated, and
    // no pointer is retained.
    let descriptor = unsafe {
        libc::openat(
            parent,
            child.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    owned_descriptor(descriptor)
}

fn owned_descriptor(descriptor: RawFd) -> Result<OwnedFd, LauncherError> {
    if descriptor < 0 {
        Err(LauncherError::InvalidLaunchPolicy)
    } else {
        // SAFETY: `descriptor` is a fresh successful result whose ownership is
        // transferred exactly once.
        Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
    }
}

fn validate_ancestor(descriptor: RawFd) -> Result<(), LauncherError> {
    let stat = descriptor_stat(descriptor)?;
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR
        || stat.st_mode & 0o022 != 0
        || stat.st_uid != 0
        || stat.st_gid != 0
        || stat.st_nlink < 2
    {
        Err(LauncherError::InvalidLaunchPolicy)
    } else {
        Ok(())
    }
}

fn descriptor_stat(descriptor: RawFd) -> Result<libc::stat, LauncherError> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `descriptor` is live and `stat` is writable for one result.
    if unsafe { libc::fstat(descriptor, stat.as_mut_ptr()) } != 0 {
        return Err(LauncherError::InvalidLaunchPolicy);
    }
    // SAFETY: Successful `fstat` initialized the complete value.
    Ok(unsafe { stat.assume_init() })
}

fn stat_identity(stat: &libc::stat) -> ObjectIdentity {
    ObjectIdentity {
        device: u64::from(u32::from_ne_bytes(stat.st_dev.to_ne_bytes())),
        inode: stat.st_ino,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    #[test]
    fn private_root_path_is_exact_and_symlink_free_by_shape() {
        assert_eq!(
            private_root_child(Path::new(
                "/private/var/root/bangbang-elevated-probe.A1b2C3d4"
            )),
            Some(OsStr::new("bangbang-elevated-probe.A1b2C3d4"))
        );
        for path in [
            "/tmp/bangbang-elevated-probe.A1b2C3d4",
            "/private/var/root/bangbang-elevated-probe.short",
            "/private/var/root/bangbang-elevated-probe.A1b2C3d4/child",
            "/private/var/root/not-the-probe.A1b2C3d4",
        ] {
            assert_eq!(private_root_child(Path::new(path)), None);
        }
    }

    #[test]
    fn debug_output_redacts_values() {
        let root = std::fs::File::open("/").expect("test root descriptor should open");
        let mut config = Config {
            activation: Vec::new(),
            root: root.into(),
            root_path: PathBuf::from("/private/var/root/bangbang-elevated-probe.A1b2C3d4"),
            root_identity: ObjectIdentity {
                device: 123,
                inode: 456,
            },
            runtime_root: None,
            staged_loader_identity: None,
            target_uid: 1_234_567_891,
            target_gid: 1_234_567_893,
            mode: ProbeMode::Drop,
            fault: RuntimeFault::None,
            adoption_barrier: false,
            daemon_barrier: DaemonBarrier::None,
            daemon_fault: DaemonFault::None,
        };
        let output = format!("{config:?}");
        assert_eq!(output, "ElevatedProbeConfig(<redacted>)");
        assert!(!output.contains("1234567891"));
        assert!(!output.contains("456"));

        let mut unexpected = vec![OsString::from("--version")];
        assert!(config.prepend_worker_activation(&mut unexpected).is_err());
        assert_eq!(unexpected, [OsString::from("--version")]);
        let mut empty = Vec::new();
        config
            .prepend_worker_activation(&mut empty)
            .expect("empty worker envelope should activate");
        assert_eq!(empty, [OsString::from(WORKER_ACTIVATION)]);
        assert_eq!(config.validate_foreground(), Ok(()));
        config.daemon_barrier = DaemonBarrier::PostAckWatch;
        assert_eq!(
            config.validate_foreground(),
            Err(LauncherError::InvalidLaunchPolicy)
        );
    }

    #[test]
    fn in_root_worker_path_is_fixed_to_the_nested_bundle() {
        assert_eq!(
            Path::new(STAGED_LAUNCHER_PATH),
            Path::new("Bangbang.app/Contents/MacOS/bangbang")
        );
        assert_eq!(
            Path::new(IN_ROOT_WORKER_PATH),
            Path::new(
                "/Bangbang.app/Contents/Helpers/BangbangWorker.app/Contents/MacOS/bangbang-worker"
            )
        );
    }

    #[test]
    fn numeric_parser_is_decimal_and_bounded() {
        assert_eq!(parse_u32(Some(&OsString::from("0"))), Ok(0));
        assert_eq!(
            parse_u32(Some(&OsString::from(u32::MAX.to_string()))),
            Ok(u32::MAX)
        );
        for invalid in ["", "00", "0501", "+1", "-1", "4294967296", "1_000", " 1"] {
            assert_eq!(
                parse_u32(Some(&OsString::from(invalid))),
                Err(LauncherError::InvalidLaunchPolicy)
            );
        }
    }

    #[test]
    fn post_adoption_stop_is_closed_to_unfaulted_guest_modes() {
        let barrier = vec![
            OsString::from(ADOPTION_BARRIER_OPTION),
            OsString::from(DELIMITER),
            OsString::from("worker-argument"),
        ];
        for mode in [ProbeMode::GuestNoApiDrop, ProbeMode::GuestApiDrop] {
            assert_eq!(
                parse_runtime_options(&barrier, mode),
                Ok((
                    RuntimeFault::None,
                    true,
                    DaemonBarrier::None,
                    DaemonFault::None,
                    1,
                ))
            );
        }
        assert_eq!(
            parse_runtime_options(&barrier, ProbeMode::RuntimeDrop),
            Err(LauncherError::InvalidLaunchPolicy)
        );
        let fault_then_barrier = vec![
            OsString::from(FAULT_OPTION),
            OsString::from("guest-oracle"),
            OsString::from(ADOPTION_BARRIER_OPTION),
            OsString::from(DELIMITER),
        ];
        assert_eq!(
            parse_runtime_options(&fault_then_barrier, ProbeMode::GuestNoApiDrop),
            Err(LauncherError::InvalidLaunchPolicy)
        );
    }

    #[test]
    fn daemon_barriers_and_faults_are_closed_and_exclusive() {
        let mode = ProbeMode::GuestNoApiDrop;
        for (name, expected) in [
            (
                "parent-before-transition-ack",
                DaemonBarrier::ParentBeforeTransitionAck,
            ),
            (
                "child-after-transition-ack",
                DaemonBarrier::ChildAfterTransitionAck,
            ),
            ("parent-after-ready", DaemonBarrier::ParentAfterReady),
            ("post-ack-watch", DaemonBarrier::PostAckWatch),
            ("parent-after-ack", DaemonBarrier::ParentAfterAck),
            (
                "daemon-namespace-retirement",
                DaemonBarrier::NamespaceRetirement,
            ),
        ] {
            let args = vec![
                OsString::from(DAEMON_BARRIER_OPTION),
                OsString::from(name),
                OsString::from(DELIMITER),
            ];
            assert_eq!(
                parse_runtime_options(&args, mode),
                Ok((RuntimeFault::None, false, expected, DaemonFault::None, 2,))
            );
        }
        for (name, expected) in [
            ("root-adoption", DaemonFault::RootAdoption),
            ("transition-witness", DaemonFault::TransitionWitness),
            ("transition-ack", DaemonFault::TransitionAck),
            ("session-bind", DaemonFault::SessionBind),
            ("ready-send", DaemonFault::ReadySend),
            ("ready-ack", DaemonFault::ReadyAck),
        ] {
            let args = vec![
                OsString::from(DAEMON_FAULT_OPTION),
                OsString::from(name),
                OsString::from(DELIMITER),
            ];
            assert_eq!(
                parse_runtime_options(&args, mode),
                Ok((RuntimeFault::None, false, DaemonBarrier::None, expected, 2,))
            );
        }

        for invalid in [
            vec![
                OsString::from(DAEMON_BARRIER_OPTION),
                OsString::from("unknown"),
                OsString::from(DELIMITER),
            ],
            vec![
                OsString::from(DAEMON_FAULT_OPTION),
                OsString::from("none"),
                OsString::from(DELIMITER),
            ],
            vec![
                OsString::from(DAEMON_BARRIER_OPTION),
                OsString::from("post-ack-watch"),
                OsString::from(DAEMON_FAULT_OPTION),
                OsString::from("ready-send"),
                OsString::from(DELIMITER),
            ],
        ] {
            assert_eq!(
                parse_runtime_options(&invalid, mode),
                Err(LauncherError::InvalidLaunchPolicy)
            );
        }
        let watched_death = vec![
            OsString::from(FAULT_OPTION),
            OsString::from("guest-endpoint-death"),
            OsString::from(DAEMON_BARRIER_OPTION),
            OsString::from("post-ack-watch"),
            OsString::from(DELIMITER),
        ];
        assert_eq!(
            parse_runtime_options(&watched_death, mode),
            Ok((
                RuntimeFault::GuestEndpointDeath,
                false,
                DaemonBarrier::PostAckWatch,
                DaemonFault::None,
                4,
            ))
        );
        let mut incompatible = watched_death;
        incompatible[1] = OsString::from("guest-oracle");
        assert_eq!(
            parse_runtime_options(&incompatible, mode),
            Err(LauncherError::InvalidLaunchPolicy)
        );
        let control = vec![
            OsString::from(DAEMON_BARRIER_OPTION),
            OsString::from("post-ack-watch"),
            OsString::from(DELIMITER),
        ];
        assert_eq!(
            parse_runtime_options(&control, ProbeMode::CredentialControl),
            Err(LauncherError::InvalidLaunchPolicy)
        );
    }

    #[test]
    fn inherited_root_transition_is_ordered_and_stops_on_first_failure() {
        let stages = [
            ProbeStage::EnterRoot,
            ProbeStage::Chroot,
            ProbeStage::ChangeDirectory,
            ProbeStage::InheritedRoot,
        ];
        for (fail_at, expected_stage) in stages.into_iter().enumerate() {
            let calls = RefCell::new(Vec::new());
            let operation = |index| {
                calls.borrow_mut().push(index);
                if index == fail_at {
                    Err(ProbeErrorCategory::PermissionDenied)
                } else {
                    Ok(())
                }
            };
            let failure = ordered_root_transition(
                || operation(0),
                || operation(1),
                || operation(2),
                || operation(3),
            )
            .expect_err("the selected transition operation should fail");
            assert_eq!(
                failure,
                ProbeFailure {
                    stage: expected_stage,
                    category: ProbeErrorCategory::PermissionDenied,
                }
            );
            assert_eq!(*calls.borrow(), (0..=fail_at).collect::<Vec<_>>());
        }

        let calls = RefCell::new(Vec::new());
        let operation = |index| {
            calls.borrow_mut().push(index);
            Ok(())
        };
        ordered_root_transition(
            || operation(0),
            || operation(1),
            || operation(2),
            || operation(3),
        )
        .expect("all transition operations should succeed");
        assert_eq!(*calls.borrow(), vec![0, 1, 2, 3]);
    }

    #[test]
    fn staged_loader_metadata_and_identity_are_closed() {
        let loader = std::fs::File::open("/usr/lib/dyld")
            .expect("the macOS dynamic loader should be present");
        let stat = descriptor_stat(loader.as_raw_fd()).expect("loader should stat");
        let identity = validate_staged_loader_stat(&stat, None)
            .expect("the system loader should satisfy the closed metadata shape");
        validate_staged_loader_stat(&stat, Some(identity))
            .expect("the recorded loader identity should match");

        let mut wrong_type = stat;
        wrong_type.st_mode = (wrong_type.st_mode & !libc::S_IFMT) | libc::S_IFDIR;
        let mut writable = stat;
        writable.st_mode |= 0o022;
        let mut wrong_uid = stat;
        wrong_uid.st_uid = 1;
        let mut wrong_gid = stat;
        wrong_gid.st_gid = 1;
        let mut empty = stat;
        empty.st_size = 0;
        let mut oversized = stat;
        oversized.st_size =
            i64::try_from(MAX_STAGED_DYLD_BYTES + 1).expect("loader bound should fit off_t");
        let mut zero_device = stat;
        zero_device.st_dev = 0;
        let mut zero_inode = stat;
        zero_inode.st_ino = 0;
        for invalid in [
            wrong_type,
            writable,
            wrong_uid,
            wrong_gid,
            empty,
            oversized,
            zero_device,
            zero_inode,
        ] {
            assert_eq!(
                validate_staged_loader_stat(&invalid, None),
                Err(LauncherError::InvalidLaunchPolicy)
            );
        }
        assert_eq!(
            validate_staged_loader_stat(
                &stat,
                Some(ObjectIdentity {
                    device: identity.device,
                    inode: identity.inode ^ 1,
                })
            ),
            Err(LauncherError::InvalidLaunchPolicy)
        );
    }
}
