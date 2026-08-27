use std::process::ExitCode;

fn main() -> ExitCode {
    let mut arguments = std::env::args_os();
    let _program = arguments.next();
    let mode = arguments.next();
    if arguments.next().is_some() {
        return ExitCode::from(2);
    }

    #[cfg(target_os = "macos")]
    {
        let result = match mode.as_deref().and_then(std::ffi::OsStr::to_str) {
            Some(bangbang_vmnet_provider::PRIVATE_BROKER_MODE) => {
                bangbang_vmnet_provider::run_private_broker()
            }
            Some(bangbang_vmnet_provider::PRIVATE_OWNER_MODE) => {
                bangbang_vmnet_provider::run_private_owner()
            }
            _ => return ExitCode::from(2),
        };
        match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(bangbang_vmnet_provider::BrokerError::Authority) => ExitCode::from(16),
            Err(bangbang_vmnet_provider::BrokerError::Descriptor) => ExitCode::from(17),
            Err(bangbang_vmnet_provider::BrokerError::BootstrapDescriptor) => ExitCode::from(18),
            Err(bangbang_vmnet_provider::BrokerError::ProviderDescriptor) => ExitCode::from(19),
            Err(bangbang_vmnet_provider::BrokerError::InvalidConfiguration) => ExitCode::from(10),
            Err(bangbang_vmnet_provider::BrokerError::Protocol) => ExitCode::from(11),
            Err(bangbang_vmnet_provider::BrokerError::Process) => ExitCode::from(12),
            Err(bangbang_vmnet_provider::BrokerError::Timeout) => ExitCode::from(13),
            Err(bangbang_vmnet_provider::BrokerError::CleanupUncertain) => ExitCode::from(14),
            Err(bangbang_vmnet_provider::BrokerError::Io(_)) => ExitCode::from(15),
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = mode;
        ExitCode::FAILURE
    }
}
