use std::process::ExitCode;

fn main() -> ExitCode {
    let mut arguments = std::env::args_os();
    let _program = arguments.next();
    let mode = arguments.next();
    let arguments = arguments.collect::<Vec<_>>();

    #[cfg(target_os = "macos")]
    {
        let result = match mode.as_deref().and_then(std::ffi::OsStr::to_str) {
            Some(bangbang_vmnet_provider::PUBLIC_BOOTSTRAP_MODE) => {
                return match bangbang_vmnet_provider::run_public_bootstrap(arguments) {
                    Ok(code) => ExitCode::from(code),
                    Err(error) => error_exit(error),
                };
            }
            Some(bangbang_vmnet_provider::PRIVATE_LAUNCHER_TRANSITION_MODE) => {
                bangbang_vmnet_provider::run_private_launcher_transition(arguments)
            }
            Some(bangbang_vmnet_provider::PRIVATE_DAEMON_BROKER_MODE) => {
                bangbang_vmnet_provider::run_private_daemon_broker(arguments)
            }
            Some(bangbang_vmnet_provider::PRIVATE_BROKER_MODE) if arguments.is_empty() => {
                bangbang_vmnet_provider::run_private_broker()
            }
            Some(bangbang_vmnet_provider::PRIVATE_OWNER_MODE) if arguments.is_empty() => {
                bangbang_vmnet_provider::run_private_owner()
            }
            _ => return ExitCode::from(2),
        };
        match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => error_exit(error),
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (mode, arguments);
        ExitCode::FAILURE
    }
}

#[cfg(target_os = "macos")]
fn error_exit(error: bangbang_vmnet_provider::BrokerError) -> ExitCode {
    match error {
        bangbang_vmnet_provider::BrokerError::Authority => ExitCode::from(16),
        bangbang_vmnet_provider::BrokerError::Descriptor => ExitCode::from(17),
        bangbang_vmnet_provider::BrokerError::BootstrapDescriptor => ExitCode::from(18),
        bangbang_vmnet_provider::BrokerError::ProviderDescriptor => ExitCode::from(19),
        bangbang_vmnet_provider::BrokerError::InvalidConfiguration => ExitCode::from(10),
        bangbang_vmnet_provider::BrokerError::Protocol => ExitCode::from(11),
        bangbang_vmnet_provider::BrokerError::Process => ExitCode::from(12),
        bangbang_vmnet_provider::BrokerError::Timeout => ExitCode::from(13),
        bangbang_vmnet_provider::BrokerError::CleanupUncertain => ExitCode::from(14),
        bangbang_vmnet_provider::BrokerError::Io(_) => ExitCode::from(15),
    }
}
