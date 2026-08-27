use std::ffi::{OsStr, OsString};

use bangbang_session::credential::CredentialTarget;
use bangbang_session::vmnet_topology::VmnetTopologyMode;

use crate::BrokerError;

/// Fixed explicitly elevated product entry.
pub const PUBLIC_BOOTSTRAP_MODE: &str = "--bootstrap-v1";

const TARGET_UID_OPTION: &str = "--target-uid";
const TARGET_GID_OPTION: &str = "--target-gid";
const DAEMONIZE_OPTION: &str = "--daemonize";
const DELIMITER: &str = "--";

pub(crate) struct BootstrapRequest {
    target: CredentialTarget,
    mode: VmnetTopologyMode,
    launcher_args: Vec<OsString>,
}

impl BootstrapRequest {
    pub(crate) fn parse(args: Vec<OsString>) -> Result<Self, BrokerError> {
        let mut target_uid = None;
        let mut target_gid = None;
        let mut daemon = false;
        let mut index = 0;
        while index < args.len() {
            let argument = args
                .get(index)
                .and_then(|argument| argument.to_str())
                .ok_or(BrokerError::InvalidConfiguration)?;
            if argument == DELIMITER {
                index += 1;
                break;
            }
            match argument {
                TARGET_UID_OPTION if target_uid.is_none() => {
                    index += 1;
                    target_uid = Some(parse_id(args.get(index))?);
                }
                TARGET_GID_OPTION if target_gid.is_none() => {
                    index += 1;
                    target_gid = Some(parse_id(args.get(index))?);
                }
                DAEMONIZE_OPTION if !daemon => daemon = true,
                _ => {
                    return Err(BrokerError::InvalidConfiguration);
                }
            }
            index += 1;
        }
        if index == 0
            || args
                .get(index.saturating_sub(1))
                .is_none_or(|argument| argument != OsStr::new(DELIMITER))
        {
            return Err(BrokerError::InvalidConfiguration);
        }
        let target = CredentialTarget::new(
            target_uid.ok_or(BrokerError::InvalidConfiguration)?,
            target_gid.ok_or(BrokerError::InvalidConfiguration)?,
        )
        .map_err(|_| BrokerError::InvalidConfiguration)?;
        Ok(Self {
            target,
            mode: if daemon {
                VmnetTopologyMode::Daemon
            } else {
                VmnetTopologyMode::Foreground
            },
            launcher_args: args.into_iter().skip(index).collect(),
        })
    }

    pub(crate) const fn target(&self) -> CredentialTarget {
        self.target
    }

    pub(crate) const fn mode(&self) -> VmnetTopologyMode {
        self.mode
    }

    pub(crate) fn launcher_args(&self) -> &[OsString] {
        &self.launcher_args
    }
}

pub(crate) fn parse_private_transition_args(
    args: Vec<OsString>,
) -> Result<Vec<OsString>, BrokerError> {
    let mut arguments = args.into_iter();
    if arguments.next().as_deref() != Some(OsStr::new(DELIMITER)) {
        return Err(BrokerError::InvalidConfiguration);
    }
    Ok(arguments.collect())
}

fn parse_id(value: Option<&OsString>) -> Result<u32, BrokerError> {
    let value = value
        .and_then(|value| value.to_str())
        .ok_or(BrokerError::InvalidConfiguration)?;
    if value.is_empty()
        || value.starts_with('0')
        || value.len() > 10
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(BrokerError::InvalidConfiguration);
    }
    value
        .parse::<u32>()
        .ok()
        .filter(|value| *value != 0)
        .ok_or(BrokerError::InvalidConfiguration)
}

impl std::fmt::Debug for BootstrapRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("BootstrapRequest(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments() -> Vec<OsString> {
        [
            TARGET_UID_OPTION,
            "501",
            TARGET_GID_OPTION,
            "20",
            DELIMITER,
            "--bangbang-jailer-v1",
            "--id",
            "guest",
        ]
        .into_iter()
        .map(OsString::from)
        .collect()
    }

    #[test]
    fn parses_exact_target_mode_and_opaque_launcher_suffix() {
        let request = BootstrapRequest::parse(arguments()).expect("request should parse");
        assert_eq!(request.target().uid(), 501);
        assert_eq!(request.target().gid(), 20);
        assert_eq!(request.mode(), VmnetTopologyMode::Foreground);
        assert_eq!(
            request.launcher_args(),
            ["--bangbang-jailer-v1", "--id", "guest"]
                .map(OsString::from)
                .as_slice()
        );

        let mut daemon = arguments();
        daemon.insert(4, OsString::from(DAEMONIZE_OPTION));
        assert_eq!(
            BootstrapRequest::parse(daemon)
                .expect("daemon should parse")
                .mode(),
            VmnetTopologyMode::Daemon
        );
    }

    #[test]
    fn rejects_missing_duplicate_malformed_root_and_unknown_authority() {
        for args in [
            vec![],
            vec![TARGET_UID_OPTION, "501", DELIMITER],
            vec![TARGET_GID_OPTION, "20", DELIMITER],
            vec![TARGET_UID_OPTION, "0", TARGET_GID_OPTION, "20", DELIMITER],
            vec![
                TARGET_UID_OPTION,
                "0501",
                TARGET_GID_OPTION,
                "20",
                DELIMITER,
            ],
            vec![
                TARGET_UID_OPTION,
                "501",
                TARGET_UID_OPTION,
                "502",
                TARGET_GID_OPTION,
                "20",
                DELIMITER,
            ],
            vec![
                TARGET_UID_OPTION,
                "501",
                TARGET_GID_OPTION,
                "20",
                "--path",
                "/private",
                DELIMITER,
            ],
        ] {
            assert_eq!(
                BootstrapRequest::parse(args.into_iter().map(OsString::from).collect()).err(),
                Some(BrokerError::InvalidConfiguration)
            );
        }
    }

    #[test]
    fn private_transition_requires_one_leading_delimiter() {
        assert_eq!(
            parse_private_transition_args(vec![OsString::from(DELIMITER), OsString::from("arg")]),
            Ok(vec![OsString::from("arg")])
        );
        assert_eq!(
            parse_private_transition_args(vec![OsString::from("arg")]),
            Err(BrokerError::InvalidConfiguration)
        );
    }
}
