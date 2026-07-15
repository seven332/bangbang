use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use bangbang_session::WorkerPolicy;

use crate::LauncherError;
use crate::grant_manifest::{LaunchInput, PreparedGrantBatch};

pub(crate) const JAILER_ACTIVATION: &str = "--bangbang-jailer-v1";
const DELIMITER: &str = "--";
const DEFAULT_NO_FILE: u64 = 2048;
const ID_OPTION: &str = "--id";
const EXEC_FILE_OPTION: &str = "--exec-file";
const UID_OPTION: &str = "--uid";
const GID_OPTION: &str = "--gid";
const RESOURCE_LIMIT_OPTION: &str = "--resource-limit";
const DAEMONIZE_OPTION: &str = "--daemonize";
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
    jailer: Option<JailerOptions>,
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
    ) -> Result<PreparedLaunch, LauncherError> {
        self.validate(worker_executable, daemonized)?;
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
            worker_policy: WorkerPolicy::new(uid, gid, no_file, file_size, daemonized),
        })
    }
}

pub(crate) const fn help() -> &'static str {
    "Usage: bangbang-launcher --bangbang-jailer-v1 --id ID --exec-file PATH --uid UID --gid GID [--resource-limit fsize=U64] [--resource-limit no-file=U64] [--daemonize] -- [WORKER OPTIONS]\n"
}

fn parse_jailer(args: Vec<OsString>) -> Result<LaunchCommand, LauncherError> {
    let raw_args = args.clone();
    let mut id = None;
    let mut exec_file = None;
    let mut uid = None;
    let mut gid = None;
    let mut no_file = DEFAULT_NO_FILE;
    let mut file_size = None;
    let mut daemonize = false;
    let mut index = 1;
    while index < args.len() {
        let argument = policy_text(args.get(index).ok_or(LauncherError::InvalidLaunchPolicy)?)?;
        if argument == DELIMITER {
            index += 1;
            break;
        }
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
    let jailer = JailerOptions {
        id: id.ok_or(LauncherError::InvalidLaunchPolicy)?,
        exec_file: exec_file.ok_or(LauncherError::InvalidLaunchPolicy)?,
        uid: uid.ok_or(LauncherError::InvalidLaunchPolicy)?,
        gid: gid.ok_or(LauncherError::InvalidLaunchPolicy)?,
        no_file,
        file_size,
        daemonize,
    };
    Ok(LaunchCommand::Run(LaunchRequest {
        raw_args,
        grants,
        jailer: Some(jailer),
    }))
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
            )
            .expect("legacy should prepare");
        assert_eq!(prepared.worker_args, vec![opaque]);
        assert_eq!(prepared.worker_policy.no_file(), DEFAULT_NO_FILE);
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
            assert!(matches!(
                LaunchCommand::parse(mutation),
                Err(LauncherError::InvalidLaunchPolicy)
            ));
        }
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
