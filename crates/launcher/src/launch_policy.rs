use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use bangbang_session::{
    MAX_VMNET_ACTIVE_INTERFACES, MAX_VMNET_BRIDGE_NAMES, VmnetAuthority, WorkerPolicy,
};

use crate::grant_manifest::{LaunchInput, PreparedGrantBatch};
use crate::{JailerIsolationArgument, LauncherError};

pub(crate) const JAILER_ACTIVATION: &str = "--bangbang-jailer-v1";
const DELIMITER: &str = "--";
const DEFAULT_NO_FILE: u64 = 2048;
const ID_OPTION: &str = "--id";
const EXEC_FILE_OPTION: &str = "--exec-file";
const UID_OPTION: &str = "--uid";
const GID_OPTION: &str = "--gid";
const RESOURCE_LIMIT_OPTION: &str = "--resource-limit";
const DAEMONIZE_OPTION: &str = "--daemonize";
const VMNET_ALLOW_OPTION: &str = "--vmnet-allow";
const VMNET_MAX_INTERFACES_OPTION: &str = "--vmnet-max-interfaces";
const FORWARDED_SINGLETONS: [&str; 4] = [
    "--id",
    "--start-time-us",
    "--start-time-cpu-us",
    "--parent-cpu-time-us",
];

/// Parsed public launcher command. Values remain private and have redacted debug output.
pub(crate) enum LaunchCommand {
    Run(LaunchRequest),
    Help,
    Version,
}

impl std::fmt::Debug for LaunchCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Run(_) => formatter.write_str("Run(<redacted>)"),
            Self::Help => formatter.write_str("Help"),
            Self::Version => formatter.write_str("Version"),
        }
    }
}

pub(crate) struct LaunchRequest {
    raw_args: Vec<OsString>,
    grants: LaunchInput,
    jailer: Option<Box<JailerOptions>>,
}

impl std::fmt::Debug for LaunchRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("LaunchRequest(<redacted>)")
    }
}

struct JailerOptions {
    id: String,
    exec_file: PathBuf,
    uid: u32,
    gid: u32,
    no_file: u64,
    file_size: Option<u64>,
    daemonize: bool,
    vmnet_authority: VmnetAuthority,
}

impl std::fmt::Debug for JailerOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("JailerOptions(<redacted>)")
    }
}

pub(crate) struct PreparedLaunch {
    pub(crate) worker_args: Vec<OsString>,
    pub(crate) grants: PreparedGrantBatch,
    pub(crate) worker_policy: WorkerPolicy,
    pub(crate) worker_profile: crate::macos::code_sign::WorkerProfile,
}

impl std::fmt::Debug for PreparedLaunch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PreparedLaunch(<redacted>)")
    }
}

#[derive(Clone, Copy)]
pub(crate) struct LaunchTiming {
    monotonic_us: u64,
    process_cpu_us: u64,
    prior_process_cpu_us: u64,
}

impl std::fmt::Debug for LaunchTiming {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("LaunchTiming(<redacted>)")
    }
}

impl LaunchTiming {
    pub(crate) fn sample() -> Result<Self, LauncherError> {
        Ok(Self {
            monotonic_us: clock_microseconds(libc::CLOCK_MONOTONIC)?,
            process_cpu_us: clock_microseconds(libc::CLOCK_PROCESS_CPUTIME_ID)?,
            prior_process_cpu_us: 0,
        })
    }

    pub(crate) fn from_daemon_handoff(
        monotonic_us: u64,
        prior_process_cpu_us: u64,
    ) -> Result<Self, LauncherError> {
        Ok(Self {
            monotonic_us,
            process_cpu_us: clock_microseconds(libc::CLOCK_PROCESS_CPUTIME_ID)?,
            prior_process_cpu_us,
        })
    }

    pub(crate) const fn monotonic_us(self) -> u64 {
        self.monotonic_us
    }

    pub(crate) fn elapsed_process_cpu_us(self) -> Result<u64, LauncherError> {
        let current = clock_microseconds(libc::CLOCK_PROCESS_CPUTIME_ID)?
            .checked_sub(self.process_cpu_us)
            .ok_or(LauncherError::InvalidLaunchPolicy)?;
        self.prior_process_cpu_us
            .checked_add(current)
            .ok_or(LauncherError::InvalidLaunchPolicy)
    }
}

impl LaunchCommand {
    pub(crate) fn parse(args: Vec<OsString>) -> Result<Self, LauncherError> {
        if args
            .first()
            .is_none_or(|arg| arg != OsStr::new(JAILER_ACTIVATION))
        {
            return Ok(Self::Run(LaunchRequest {
                raw_args: args.clone(),
                grants: LaunchInput::parse(args)?,
                jailer: None,
            }));
        }
        if args.len() == 2
            && args
                .get(1)
                .is_some_and(|argument| argument == OsStr::new("--help"))
        {
            return Ok(Self::Help);
        }
        if args.len() == 2
            && args
                .get(1)
                .is_some_and(|argument| argument == OsStr::new("--version"))
        {
            return Ok(Self::Version);
        }
        parse_jailer(args)
    }
}

impl LaunchRequest {
    pub(crate) fn raw_args(&self) -> &[OsString] {
        &self.raw_args
    }

    pub(crate) fn requests_daemonize(&self) -> bool {
        self.jailer.as_ref().is_some_and(|jailer| jailer.daemonize)
    }

    pub(crate) fn validate(
        &self,
        worker_executable: &Path,
        daemonized: bool,
    ) -> Result<(), LauncherError> {
        let (uid, gid) = current_credentials()?;
        match &self.jailer {
            Some(jailer)
                if jailer.exec_file == worker_executable
                    && jailer.exec_file.is_absolute()
                    && jailer.uid == uid
                    && jailer.gid == gid
                    && jailer.daemonize == daemonized =>
            {
                Ok(())
            }
            None if !daemonized => Ok(()),
            Some(_) | None => Err(LauncherError::InvalidLaunchPolicy),
        }
    }

    pub(crate) fn prepare(
        self,
        worker_executable: &Path,
        timing: LaunchTiming,
        daemonized: bool,
        worker_profile: crate::macos::code_sign::WorkerProfile,
    ) -> Result<PreparedLaunch, LauncherError> {
        self.validate(worker_executable, daemonized)?;
        let vmnet_authority = self
            .jailer
            .as_ref()
            .map_or_else(VmnetAuthority::denied, |jailer| jailer.vmnet_authority);
        if !worker_profile.admits(vmnet_authority) {
            return Err(LauncherError::InvalidLaunchPolicy);
        }
        let (mut worker_args, grants) = self.grants.prepare()?;
        let (uid, gid) = current_credentials()?;
        let (no_file, file_size) = if let Some(jailer) = self.jailer {
            let mut injected = vec![
                OsString::from(ID_OPTION),
                OsString::from(jailer.id),
                OsString::from("--start-time-us"),
                OsString::from(timing.monotonic_us.to_string()),
                OsString::from("--start-time-cpu-us"),
                OsString::from("0"),
                OsString::from("--parent-cpu-time-us"),
                OsString::from(timing.elapsed_process_cpu_us()?.to_string()),
            ];
            injected.append(&mut worker_args);
            worker_args = injected;
            (jailer.no_file, jailer.file_size)
        } else {
            (DEFAULT_NO_FILE, None)
        };
        Ok(PreparedLaunch {
            worker_args,
            grants,
            worker_policy: WorkerPolicy::new(uid, gid, no_file, file_size, daemonized)
                .with_vmnet_authority(vmnet_authority),
            worker_profile,
        })
    }
}

pub(crate) const fn help() -> &'static str {
    "Usage: bangbang-launcher --bangbang-jailer-v1 --id ID --exec-file PATH --uid UID --gid GID [--resource-limit fsize=U64] [--resource-limit no-file=U64] [--vmnet-allow host|shared|bridged:INTERFACE ... --vmnet-max-interfaces 1..=4] [--daemonize] -- [WORKER OPTIONS]\n"
}

fn parse_jailer(args: Vec<OsString>) -> Result<LaunchCommand, LauncherError> {
    let mut id = None;
    let mut exec_file = None;
    let mut uid = None;
    let mut gid = None;
    let mut no_file = DEFAULT_NO_FILE;
    let mut file_size = None;
    let mut daemonize = false;
    let mut allow_vmnet_host = false;
    let mut allow_vmnet_shared = false;
    let mut allowed_vmnet_bridges = Vec::new();
    let mut vmnet_max_interfaces = None;
    let mut index = 1;
    while index < args.len() {
        let argument = args.get(index).ok_or(LauncherError::InvalidLaunchPolicy)?;
        if argument == OsStr::new(DELIMITER) {
            index += 1;
            break;
        }
        if let Some(argument) = unsupported_jailer_isolation_argument(argument) {
            return Err(LauncherError::UnsupportedJailerIsolation(argument));
        }
        let argument = policy_text(argument)?;
        match argument {
            ID_OPTION => {
                if id.is_some() {
                    return Err(LauncherError::InvalidLaunchPolicy);
                }
                let value = next_policy_value(&args, &mut index)?;
                if value.is_empty()
                    || value.len() > 64
                    || !value
                        .chars()
                        .all(|character| character.is_alphanumeric() || character == '-')
                {
                    return Err(LauncherError::InvalidLaunchPolicy);
                }
                id = Some(value.to_owned());
            }
            EXEC_FILE_OPTION => {
                if exec_file.is_some() {
                    return Err(LauncherError::InvalidLaunchPolicy);
                }
                let value = next_policy_os_value(&args, &mut index)?;
                policy_text(value)?;
                let path = PathBuf::from(value);
                if !path.is_absolute() {
                    return Err(LauncherError::InvalidLaunchPolicy);
                }
                exec_file = Some(path);
            }
            UID_OPTION => {
                if uid.is_some() {
                    return Err(LauncherError::InvalidLaunchPolicy);
                }
                uid = Some(parse_u32(next_policy_value(&args, &mut index)?)?);
            }
            GID_OPTION => {
                if gid.is_some() {
                    return Err(LauncherError::InvalidLaunchPolicy);
                }
                gid = Some(parse_u32(next_policy_value(&args, &mut index)?)?);
            }
            RESOURCE_LIMIT_OPTION => {
                let value = next_policy_value(&args, &mut index)?;
                let (name, raw_limit) = value
                    .split_once('=')
                    .filter(|(name, value)| !name.is_empty() && !value.is_empty())
                    .ok_or(LauncherError::InvalidLaunchPolicy)?;
                if raw_limit.contains('=') {
                    return Err(LauncherError::InvalidLaunchPolicy);
                }
                let limit = raw_limit
                    .parse::<u64>()
                    .map_err(|_| LauncherError::InvalidLaunchPolicy)?;
                match name {
                    "fsize" => file_size = Some(limit),
                    "no-file" => no_file = limit,
                    _ => return Err(LauncherError::InvalidLaunchPolicy),
                }
            }
            DAEMONIZE_OPTION if !daemonize => daemonize = true,
            VMNET_ALLOW_OPTION => {
                let value = next_policy_value(&args, &mut index)?;
                match value {
                    "host" if !allow_vmnet_host => allow_vmnet_host = true,
                    "shared" if !allow_vmnet_shared => allow_vmnet_shared = true,
                    "host" | "shared" => return Err(LauncherError::InvalidLaunchPolicy),
                    value => {
                        let bridge = value
                            .strip_prefix("bridged:")
                            .ok_or(LauncherError::InvalidLaunchPolicy)?;
                        if allowed_vmnet_bridges.len() >= MAX_VMNET_BRIDGE_NAMES
                            || allowed_vmnet_bridges
                                .iter()
                                .any(|allowed| allowed == bridge)
                            || VmnetAuthority::try_new(false, false, 1, &[bridge]).is_err()
                        {
                            return Err(LauncherError::InvalidLaunchPolicy);
                        }
                        allowed_vmnet_bridges.push(bridge.to_owned());
                    }
                }
            }
            VMNET_MAX_INTERFACES_OPTION => {
                if vmnet_max_interfaces.is_some() {
                    return Err(LauncherError::InvalidLaunchPolicy);
                }
                vmnet_max_interfaces = Some(
                    next_policy_value(&args, &mut index)?
                        .parse::<u8>()
                        .map_err(|_| LauncherError::InvalidLaunchPolicy)?,
                );
            }
            _ => return Err(LauncherError::InvalidLaunchPolicy),
        }
        index += 1;
    }
    if index == 1
        || index > args.len()
        || args.get(index.saturating_sub(1)) != Some(&OsString::from(DELIMITER))
    {
        return Err(LauncherError::InvalidLaunchPolicy);
    }
    let worker_envelope = args
        .get(index..)
        .ok_or(LauncherError::InvalidLaunchPolicy)?
        .to_vec();
    let grants = LaunchInput::parse(worker_envelope)?;
    reject_forwarded_singletons(&grants.worker_args)?;
    let any_vmnet_allow =
        allow_vmnet_host || allow_vmnet_shared || !allowed_vmnet_bridges.is_empty();
    let vmnet_authority = match (any_vmnet_allow, vmnet_max_interfaces) {
        (false, None) => VmnetAuthority::denied(),
        (true, Some(maximum)) if maximum <= MAX_VMNET_ACTIVE_INTERFACES => {
            let bridges = allowed_vmnet_bridges
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            VmnetAuthority::try_new(allow_vmnet_host, allow_vmnet_shared, maximum, &bridges)
                .map_err(|_| LauncherError::InvalidLaunchPolicy)?
        }
        (false, Some(_)) | (true, None) | (true, Some(_)) => {
            return Err(LauncherError::InvalidLaunchPolicy);
        }
    };
    let jailer = JailerOptions {
        id: id.ok_or(LauncherError::InvalidLaunchPolicy)?,
        exec_file: exec_file.ok_or(LauncherError::InvalidLaunchPolicy)?,
        uid: uid.ok_or(LauncherError::InvalidLaunchPolicy)?,
        gid: gid.ok_or(LauncherError::InvalidLaunchPolicy)?,
        no_file,
        file_size,
        daemonize,
        vmnet_authority,
    };
    Ok(LaunchCommand::Run(LaunchRequest {
        raw_args: args,
        grants,
        jailer: Some(Box::new(jailer)),
    }))
}

fn unsupported_jailer_isolation_argument(argument: &OsStr) -> Option<JailerIsolationArgument> {
    let name = argument.as_bytes().strip_prefix(b"--")?;
    let name = name.split(|byte| *byte == b'=').next()?;
    JailerIsolationArgument::from_name(std::str::from_utf8(name).ok()?)
}

fn reject_forwarded_singletons(args: &[OsString]) -> Result<(), LauncherError> {
    for argument in args {
        if argument == OsStr::new(DELIMITER) {
            break;
        }
        let bytes = argument.as_bytes();
        if FORWARDED_SINGLETONS.iter().any(|option| {
            bytes == option.as_bytes()
                || bytes
                    .strip_prefix(option.as_bytes())
                    .is_some_and(|suffix| suffix.starts_with(b"="))
        }) {
            return Err(LauncherError::InvalidLaunchPolicy);
        }
    }
    Ok(())
}

fn policy_text(value: &OsStr) -> Result<&str, LauncherError> {
    value.to_str().ok_or(LauncherError::InvalidLaunchPolicy)
}

fn next_policy_os_value<'a>(
    args: &'a [OsString],
    index: &mut usize,
) -> Result<&'a OsStr, LauncherError> {
    *index = index
        .checked_add(1)
        .ok_or(LauncherError::InvalidLaunchPolicy)?;
    args.get(*index)
        .filter(|value| !value.is_empty() && value != &OsStr::new(DELIMITER))
        .map(OsString::as_os_str)
        .ok_or(LauncherError::InvalidLaunchPolicy)
}

fn next_policy_value<'a>(
    args: &'a [OsString],
    index: &mut usize,
) -> Result<&'a str, LauncherError> {
    policy_text(next_policy_os_value(args, index)?)
}

fn parse_u32(value: &str) -> Result<u32, LauncherError> {
    value
        .parse::<u32>()
        .map_err(|_| LauncherError::InvalidLaunchPolicy)
}

fn current_credentials() -> Result<(u32, u32), LauncherError> {
    // SAFETY: These credential getters take no pointers and have no failure mode.
    let (uid, effective_uid, gid, effective_gid) = unsafe {
        (
            libc::getuid(),
            libc::geteuid(),
            libc::getgid(),
            libc::getegid(),
        )
    };
    if uid != effective_uid || gid != effective_gid {
        return Err(LauncherError::InvalidLaunchPolicy);
    }
    Ok((uid, gid))
}

fn clock_microseconds(clock: libc::clockid_t) -> Result<u64, LauncherError> {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `value` is valid writable storage for one `timespec`.
    if unsafe { libc::clock_gettime(clock, &mut value) } != 0
        || value.tv_sec < 0
        || value.tv_nsec < 0
    {
        return Err(LauncherError::InvalidLaunchPolicy);
    }
    u64::try_from(value.tv_sec)
        .ok()
        .and_then(|seconds| seconds.checked_mul(1_000_000))
        .and_then(|micros| {
            u64::try_from(value.tv_nsec)
                .ok()
                .and_then(|nanos| micros.checked_add(nanos / 1_000))
        })
        .ok_or(LauncherError::InvalidLaunchPolicy)
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStringExt;

    use super::*;

    const fn networkless_profile() -> crate::macos::code_sign::WorkerProfile {
        crate::macos::code_sign::WorkerProfile::Networkless
    }

    fn vmnet_profile() -> crate::macos::code_sign::WorkerProfile {
        crate::macos::code_sign::WorkerProfile::Vmnet {
            application_identifier: "APPID12345.dev.bangbang.worker".to_owned(),
            team_identifier: "TEAM123456".to_owned(),
        }
    }

    fn base(worker: &Path) -> Vec<OsString> {
        let (uid, gid) = current_credentials().expect("test credentials should be ordinary");
        vec![
            JAILER_ACTIVATION.into(),
            ID_OPTION.into(),
            "vm-1".into(),
            EXEC_FILE_OPTION.into(),
            worker.as_os_str().to_owned(),
            UID_OPTION.into(),
            uid.to_string().into(),
            GID_OPTION.into(),
            gid.to_string().into(),
            DELIMITER.into(),
        ]
    }

    fn assert_invalid(args: Vec<OsString>) {
        assert!(matches!(
            LaunchCommand::parse(args),
            Err(LauncherError::InvalidLaunchPolicy)
        ));
    }

    fn assert_unsupported_isolation(
        args: Vec<OsString>,
        expected: JailerIsolationArgument,
        private_values: &[&str],
    ) {
        let error = LaunchCommand::parse(args).expect_err("Linux isolation input should fail");
        assert_eq!(error, LauncherError::UnsupportedJailerIsolation(expected));
        assert_eq!(
            error.to_string(),
            format!(
                "unsupported Firecracker jailer isolation argument on macOS: --{}",
                expected.name()
            )
        );
        let diagnostics = format!("{error:?}\n{error}");
        for private_value in private_values {
            assert!(!diagnostics.contains(private_value));
        }
    }

    #[test]
    fn rejects_named_linux_isolation_before_consuming_values() {
        let worker = Path::new("/fixed/BangbangWorker");
        let arguments = [
            JailerIsolationArgument::Cgroup,
            JailerIsolationArgument::CgroupVersion,
            JailerIsolationArgument::ParentCgroup,
            JailerIsolationArgument::NetworkNamespace,
            JailerIsolationArgument::PidNamespace,
        ];

        for argument in arguments {
            let mut exact = base(worker);
            exact.insert(exact.len() - 1, format!("--{}", argument.name()).into());
            assert_unsupported_isolation(exact, argument, &[]);

            let private_value = format!("private-{}-value", argument.name());
            let mut attached = base(worker);
            attached.insert(
                attached.len() - 1,
                format!("--{}={private_value}", argument.name()).into(),
            );
            assert_unsupported_isolation(attached, argument, &[&private_value]);
        }

        for argument in [
            JailerIsolationArgument::Cgroup,
            JailerIsolationArgument::CgroupVersion,
            JailerIsolationArgument::ParentCgroup,
            JailerIsolationArgument::NetworkNamespace,
        ] {
            let private_value = format!("private-separated-{}-value", argument.name());
            let mut separated = base(worker);
            separated.splice(
                separated.len() - 1..separated.len() - 1,
                [
                    OsString::from(format!("--{}", argument.name())),
                    OsString::from(&private_value),
                ],
            );
            assert_unsupported_isolation(separated, argument, &[&private_value]);
        }

        let mut non_utf8_attached = base(worker);
        non_utf8_attached.insert(
            non_utf8_attached.len() - 1,
            OsString::from_vec(b"--netns=private-\xff-path".to_vec()),
        );
        assert_unsupported_isolation(
            non_utf8_attached,
            JailerIsolationArgument::NetworkNamespace,
            &[],
        );
    }

    #[test]
    fn linux_isolation_names_are_exact_and_pre_delimiter_only() {
        let worker = Path::new("/fixed/BangbangWorker");
        for lookalike in [
            "--cgroups",
            "--cgroup-version-extra",
            "--parent-cgroup-child",
            "--netns-path",
            "--new-pid-ns-extra",
        ] {
            let mut args = base(worker);
            args.insert(args.len() - 1, lookalike.into());
            assert_invalid(args);
        }

        for argument in [
            JailerIsolationArgument::Cgroup,
            JailerIsolationArgument::CgroupVersion,
            JailerIsolationArgument::ParentCgroup,
            JailerIsolationArgument::NetworkNamespace,
            JailerIsolationArgument::PidNamespace,
        ] {
            let private_value = format!("opaque-{}-value", argument.name());
            let mut args = base(worker);
            args.push(format!("--{}={private_value}", argument.name()).into());
            let LaunchCommand::Run(request) =
                LaunchCommand::parse(args).expect("worker arguments should remain opaque")
            else {
                panic!("run command expected");
            };
            let prepared = request
                .prepare(
                    worker,
                    LaunchTiming::sample().expect("timing should sample"),
                    false,
                    networkless_profile(),
                )
                .expect("opaque worker argument should prepare");
            assert_eq!(
                prepared.worker_args.last(),
                Some(&OsString::from(format!(
                    "--{}={private_value}",
                    argument.name()
                )))
            );
        }
    }

    #[test]
    fn parses_exact_policy_and_injects_owned_arguments() {
        let worker = Path::new("/fixed/BangbangWorker");
        let mut args = base(worker);
        args.splice(
            args.len() - 1..args.len() - 1,
            [
                OsString::from(RESOURCE_LIMIT_OPTION),
                OsString::from("no-file=4096"),
                OsString::from(RESOURCE_LIMIT_OPTION),
                OsString::from("no-file=2048"),
                OsString::from(RESOURCE_LIMIT_OPTION),
                OsString::from("fsize=8192"),
            ],
        );
        args.push("--no-api".into());
        let LaunchCommand::Run(request) = LaunchCommand::parse(args).expect("policy should parse")
        else {
            panic!("run command expected");
        };
        let prepared = request
            .prepare(
                worker,
                LaunchTiming::sample().expect("timing should sample"),
                false,
                networkless_profile(),
            )
            .expect("policy should prepare");
        assert_eq!(prepared.worker_policy.no_file(), 2048);
        assert_eq!(prepared.worker_policy.file_size(), Some(8192));
        assert!(!prepared.worker_policy.is_daemonized());
        assert_eq!(prepared.worker_args[0], ID_OPTION);
        assert_eq!(prepared.worker_args[1], "vm-1");
        assert_eq!(
            prepared.worker_args.last(),
            Some(&OsString::from("--no-api"))
        );
    }

    #[test]
    fn legacy_launch_gets_default_policy_without_argument_changes() {
        let opaque = OsString::from_vec(vec![b'-', b'-', b'x', 0xff]);
        let LaunchCommand::Run(request) =
            LaunchCommand::parse(vec![opaque.clone()]).expect("legacy should parse")
        else {
            panic!("run command expected");
        };
        let prepared = request
            .prepare(
                Path::new("/fixed/worker"),
                LaunchTiming::sample().expect("timing"),
                false,
                networkless_profile(),
            )
            .expect("legacy should prepare");
        assert_eq!(prepared.worker_args, vec![opaque]);
        assert_eq!(prepared.worker_policy.no_file(), DEFAULT_NO_FILE);
        assert!(prepared.worker_policy.vmnet_authority().is_denied());
    }

    #[test]
    fn ordinary_worker_arguments_cannot_enable_vmnet_authority() {
        let worker_args = vec![
            OsString::from(VMNET_ALLOW_OPTION),
            OsString::from("shared"),
            OsString::from(VMNET_MAX_INTERFACES_OPTION),
            OsString::from("1"),
        ];
        let LaunchCommand::Run(request) =
            LaunchCommand::parse(worker_args.clone()).expect("ordinary arguments should parse")
        else {
            panic!("run command expected");
        };
        let prepared = request
            .prepare(
                Path::new("/fixed/worker"),
                LaunchTiming::sample().expect("timing"),
                false,
                networkless_profile(),
            )
            .expect("ordinary launch should prepare");

        assert_eq!(prepared.worker_args, worker_args);
        assert!(prepared.worker_policy.vmnet_authority().is_denied());
    }

    #[test]
    fn vmnet_profile_rejects_denied_policy_before_spawn() {
        let LaunchCommand::Run(request) =
            LaunchCommand::parse(Vec::new()).expect("legacy arguments should parse")
        else {
            panic!("run command expected");
        };
        assert!(matches!(
            request.prepare(
                Path::new("/fixed/worker"),
                LaunchTiming::sample().expect("timing should sample"),
                false,
                vmnet_profile(),
            ),
            Err(LauncherError::InvalidLaunchPolicy)
        ));
    }

    #[test]
    fn parses_exact_vmnet_authority_and_preserves_daemon_reparse_bytes() {
        let worker = Path::new("/fixed/BangbangWorker");
        let mut args = base(worker);
        args.splice(
            args.len() - 1..args.len() - 1,
            [
                OsString::from(VMNET_ALLOW_OPTION),
                OsString::from("host"),
                OsString::from(VMNET_ALLOW_OPTION),
                OsString::from("shared"),
                OsString::from(VMNET_ALLOW_OPTION),
                OsString::from("bridged:en0"),
                OsString::from(VMNET_ALLOW_OPTION),
                OsString::from("bridged:bridge_1"),
                OsString::from(VMNET_MAX_INTERFACES_OPTION),
                OsString::from("4"),
                OsString::from(DAEMONIZE_OPTION),
            ],
        );
        let LaunchCommand::Run(request) =
            LaunchCommand::parse(args.clone()).expect("vmnet policy should parse")
        else {
            panic!("run command expected");
        };
        let authority = request
            .jailer
            .as_ref()
            .expect("jailer policy should exist")
            .vmnet_authority;
        assert!(authority.allows_host());
        assert!(authority.allows_shared());
        assert!(authority.allows_bridge("en0"));
        assert!(authority.allows_bridge("bridge_1"));
        assert_eq!(authority.max_interfaces(), Some(4));
        assert_eq!(request.raw_args(), args);

        let LaunchCommand::Run(reparsed) =
            LaunchCommand::parse(request.raw_args().to_vec()).expect("daemon bytes should reparse")
        else {
            panic!("run command expected");
        };
        assert_eq!(
            reparsed
                .jailer
                .as_ref()
                .expect("reparsed jailer policy should exist")
                .vmnet_authority,
            authority
        );
        assert!(matches!(
            reparsed.prepare(
                worker,
                LaunchTiming::sample().expect("timing should sample"),
                true,
                networkless_profile(),
            ),
            Err(LauncherError::InvalidLaunchPolicy)
        ));

        let LaunchCommand::Run(vmnet_request) =
            LaunchCommand::parse(args).expect("vmnet policy should reparse")
        else {
            panic!("run command expected");
        };
        let prepared = vmnet_request
            .prepare(
                worker,
                LaunchTiming::sample().expect("timing should sample"),
                true,
                vmnet_profile(),
            )
            .expect("vmnet profile should admit nonempty authority");
        assert_eq!(prepared.worker_policy.vmnet_authority(), authority);
    }

    #[test]
    fn rejects_malformed_duplicate_and_unrelated_vmnet_options() {
        let worker = Path::new("/fixed/worker");
        let cases: &[&[&str]] = &[
            &[VMNET_ALLOW_OPTION, "host"],
            &[VMNET_MAX_INTERFACES_OPTION, "1"],
            &[
                VMNET_ALLOW_OPTION,
                "host",
                VMNET_ALLOW_OPTION,
                "host",
                VMNET_MAX_INTERFACES_OPTION,
                "1",
            ],
            &[
                VMNET_ALLOW_OPTION,
                "shared",
                VMNET_ALLOW_OPTION,
                "shared",
                VMNET_MAX_INTERFACES_OPTION,
                "1",
            ],
            &[
                VMNET_ALLOW_OPTION,
                "bridged:en0",
                VMNET_ALLOW_OPTION,
                "bridged:en0",
                VMNET_MAX_INTERFACES_OPTION,
                "1",
            ],
            &[
                VMNET_ALLOW_OPTION,
                "host",
                VMNET_MAX_INTERFACES_OPTION,
                "1",
                VMNET_MAX_INTERFACES_OPTION,
                "2",
            ],
            &[VMNET_ALLOW_OPTION, "host", VMNET_MAX_INTERFACES_OPTION, "0"],
            &[VMNET_ALLOW_OPTION, "host", VMNET_MAX_INTERFACES_OPTION, "5"],
            &[
                VMNET_ALLOW_OPTION,
                "host",
                VMNET_MAX_INTERFACES_OPTION,
                "not-a-number",
            ],
            &[
                VMNET_ALLOW_OPTION,
                "bridged:",
                VMNET_MAX_INTERFACES_OPTION,
                "1",
            ],
            &[
                VMNET_ALLOW_OPTION,
                "bridged:en$0",
                VMNET_MAX_INTERFACES_OPTION,
                "1",
            ],
            &[
                VMNET_ALLOW_OPTION,
                "bridged:abcdefghijklmnop",
                VMNET_MAX_INTERFACES_OPTION,
                "1",
            ],
            &[
                VMNET_ALLOW_OPTION,
                "bridged:a",
                VMNET_ALLOW_OPTION,
                "bridged:b",
                VMNET_ALLOW_OPTION,
                "bridged:c",
                VMNET_ALLOW_OPTION,
                "bridged:d",
                VMNET_ALLOW_OPTION,
                "bridged:e",
                VMNET_MAX_INTERFACES_OPTION,
                "4",
            ],
            &[
                VMNET_ALLOW_OPTION,
                "unknown",
                VMNET_MAX_INTERFACES_OPTION,
                "1",
            ],
        ];
        for values in cases {
            let mut args = base(worker);
            args.splice(
                args.len() - 1..args.len() - 1,
                values.iter().copied().map(OsString::from),
            );
            assert_invalid(args);
        }
    }

    #[test]
    fn rejects_missing_duplicate_unknown_and_forwarded_inputs() {
        let worker = Path::new("/fixed/worker");
        for mutation in [
            vec![JAILER_ACTIVATION.into()],
            {
                let mut value = base(worker);
                value.insert(3, ID_OPTION.into());
                value.insert(4, "second".into());
                value
            },
            {
                let mut value = base(worker);
                value.insert(value.len() - 1, "--unknown".into());
                value
            },
            {
                let mut value = base(worker);
                value.push("--id=forged".into());
                value
            },
        ] {
            assert_invalid(mutation);
        }
    }

    #[test]
    fn enforces_id_byte_boundaries_and_policy_text_encoding() {
        let worker = Path::new("/fixed/worker");
        for id in ["a".repeat(64), "界".repeat(21)] {
            let mut args = base(worker);
            args[2] = id.into();
            assert!(matches!(
                LaunchCommand::parse(args),
                Ok(LaunchCommand::Run(_))
            ));
        }

        for id in [
            String::new(),
            "a".repeat(65),
            "界".repeat(22),
            "bad_id".into(),
        ] {
            let mut args = base(worker);
            args[2] = id.into();
            assert_invalid(args);
        }

        let mut non_utf8_id = base(worker);
        non_utf8_id[2] = OsString::from_vec(vec![0xff]);
        assert_invalid(non_utf8_id);

        let mut non_utf8_executable = base(worker);
        non_utf8_executable[4] = OsString::from_vec(vec![b'/', 0xff]);
        assert_invalid(non_utf8_executable);
    }

    #[test]
    fn rejects_malformed_numbers_limits_flags_and_delimiters() {
        let worker = Path::new("/fixed/worker");
        let mut cases = Vec::new();

        let mut missing_delimiter = base(worker);
        missing_delimiter.pop();
        cases.push(missing_delimiter);

        let mut relative_executable = base(worker);
        relative_executable[4] = "relative/worker".into();
        cases.push(relative_executable);

        for (index, value) in [(6, "4294967296"), (8, "not-a-number")] {
            let mut args = base(worker);
            args[index] = value.into();
            cases.push(args);
        }

        for value in [
            "fsize",
            "=1",
            "fsize=",
            "fsize=1=2",
            "unknown=1",
            "no-file=18446744073709551616",
        ] {
            let mut args = base(worker);
            args.splice(
                args.len() - 1..args.len() - 1,
                [OsString::from(RESOURCE_LIMIT_OPTION), OsString::from(value)],
            );
            cases.push(args);
        }

        let mut duplicate_daemon = base(worker);
        duplicate_daemon.splice(
            duplicate_daemon.len() - 1..duplicate_daemon.len() - 1,
            [
                OsString::from(DAEMONIZE_OPTION),
                OsString::from(DAEMONIZE_OPTION),
            ],
        );
        cases.push(duplicate_daemon);

        for args in cases {
            assert_invalid(args);
        }
    }

    #[test]
    fn validation_binds_fixed_executable_current_credentials_and_daemon_state() {
        let worker = Path::new("/fixed/worker");
        let LaunchCommand::Run(request) =
            LaunchCommand::parse(base(worker)).expect("policy should parse")
        else {
            panic!("run command expected");
        };
        assert_eq!(request.validate(worker, false), Ok(()));
        assert_eq!(
            request.validate(Path::new("/fixed/other-worker"), false),
            Err(LauncherError::InvalidLaunchPolicy)
        );
        assert_eq!(
            request.validate(worker, true),
            Err(LauncherError::InvalidLaunchPolicy)
        );

        let (uid, gid) = current_credentials().expect("credentials should be ordinary");
        for (index, value) in [(6, uid.wrapping_add(1)), (8, gid.wrapping_add(1))] {
            let mut args = base(worker);
            args[index] = value.to_string().into();
            let LaunchCommand::Run(request) =
                LaunchCommand::parse(args).expect("mismatched policy should parse")
            else {
                panic!("run command expected");
            };
            assert_eq!(
                request.validate(worker, false),
                Err(LauncherError::InvalidLaunchPolicy)
            );
        }

        let mut daemon_args = base(worker);
        daemon_args.insert(daemon_args.len() - 1, DAEMONIZE_OPTION.into());
        let LaunchCommand::Run(daemon_request) =
            LaunchCommand::parse(daemon_args).expect("daemon policy should parse")
        else {
            panic!("run command expected");
        };
        assert_eq!(daemon_request.validate(worker, true), Ok(()));
        assert_eq!(
            daemon_request.validate(worker, false),
            Err(LauncherError::InvalidLaunchPolicy)
        );
    }

    #[test]
    fn rejects_forwarded_singletons_before_worker_delimiter_only() {
        let worker = Path::new("/fixed/worker");
        for option in FORWARDED_SINGLETONS {
            let mut separate = base(worker);
            separate.extend([OsString::from(option), OsString::from("forged")]);
            assert_invalid(separate);

            let mut attached = base(worker);
            attached.push(format!("{option}=forged").into());
            assert_invalid(attached);
        }

        let opaque = OsString::from_vec(vec![0xff, 0xfe]);
        let mut args = base(worker);
        args.extend([
            OsString::from(DELIMITER),
            OsString::from(ID_OPTION),
            opaque.clone(),
        ]);
        let LaunchCommand::Run(request) =
            LaunchCommand::parse(args).expect("post-delimiter values should stay opaque")
        else {
            panic!("run command expected");
        };
        let prepared = request
            .prepare(
                worker,
                LaunchTiming::sample().expect("timing should sample"),
                false,
                networkless_profile(),
            )
            .expect("opaque worker tail should prepare");
        assert_eq!(
            prepared.worker_args.get(prepared.worker_args.len() - 2),
            Some(&OsString::from(ID_OPTION))
        );
        assert_eq!(prepared.worker_args.last(), Some(&opaque));
    }

    #[test]
    fn recognizes_only_exact_early_help_and_version() {
        assert!(matches!(
            LaunchCommand::parse(vec![JAILER_ACTIVATION.into(), "--help".into()]),
            Ok(LaunchCommand::Help)
        ));
        assert!(matches!(
            LaunchCommand::parse(vec![JAILER_ACTIVATION.into(), "--version".into()]),
            Ok(LaunchCommand::Version)
        ));
        assert!(matches!(
            LaunchCommand::parse(vec!["--help".into()]),
            Ok(LaunchCommand::Run(_))
        ));
    }

    #[test]
    fn debug_and_errors_do_not_disclose_policy_values() {
        let command =
            LaunchCommand::parse(base(Path::new("/private/worker"))).expect("policy should parse");
        assert_eq!(format!("{command:?}"), "Run(<redacted>)");
        assert_eq!(
            LauncherError::InvalidLaunchPolicy.to_string(),
            "invalid production launch policy"
        );
    }
}
