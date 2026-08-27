mod backend;
mod broker_service;
mod owner_service;
mod process;
mod topology;
mod transport;

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

use bangbang_unix_stream::connected_unix_stream;

use crate::broker::BrokerError;

pub const PRIVATE_BROKER_MODE: &str = "--private-broker-v1";
pub const PRIVATE_DAEMON_BROKER_MODE: &str = "--private-daemon-broker-v1";
pub const PRIVATE_OWNER_MODE: &str = "--private-owner-v1";
pub const PRIVATE_LAUNCHER_TRANSITION_MODE: &str = "--private-launcher-transition-v1";

const VMNET_DAEMON_ENV_KEY: &str = "BANGBANG_INTERNAL_VMNET_DAEMON_V1";
const VMNET_DAEMON_ENV_VALUE: &str = "1";

const BOOTSTRAP_FD: RawFd = 3;
const PROVIDER_FD: RawFd = 4;

pub fn run_private_broker() -> Result<(), BrokerError> {
    require_exact_root()?;
    let bootstrap =
        adopt_connected_stream(BOOTSTRAP_FD).map_err(|_| BrokerError::BootstrapDescriptor)?;
    let control =
        adopt_connected_stream(PROVIDER_FD).map_err(|_| BrokerError::ProviderDescriptor)?;
    broker_service::run(bootstrap, control)
}

pub fn run_private_owner() -> Result<(), BrokerError> {
    require_exact_root()?;
    let supervision =
        adopt_connected_stream(BOOTSTRAP_FD).map_err(|_| BrokerError::BootstrapDescriptor)?;
    let data = adopt_connected_stream(PROVIDER_FD).map_err(|_| BrokerError::ProviderDescriptor)?;
    owner_service::run(supervision, data)
}

pub use topology::{
    run_private_daemon_broker, run_private_launcher_transition, run_public_bootstrap,
};

fn require_exact_root() -> Result<(), BrokerError> {
    // SAFETY: Darwin credential getters have no pointer or ownership contract.
    let ids = unsafe {
        (
            libc::getuid(),
            libc::geteuid(),
            libc::getgid(),
            libc::getegid(),
        )
    };
    if ids == (0, 0, 0, 0) {
        Ok(())
    } else {
        Err(BrokerError::Authority)
    }
}

fn adopt_connected_stream(descriptor: RawFd) -> Result<UnixStream, BrokerError> {
    if descriptor < 0 {
        return Err(BrokerError::Descriptor);
    }
    // SAFETY: Each private entry point owns its fixed inherited descriptor
    // exactly once. No wrapper exists before this adoption.
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    set_cloexec(descriptor.as_raw_fd()).map_err(|_| BrokerError::Descriptor)?;
    connected_unix_stream(descriptor).map_err(|_| BrokerError::Descriptor)
}

fn set_cloexec(descriptor: RawFd) -> Result<(), BrokerError> {
    // SAFETY: F_GETFD operates on the owned live descriptor only.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    if flags < 0 {
        return Err(BrokerError::Io(std::io::Error::last_os_error().kind()));
    }
    // SAFETY: F_SETFD operates on the same owned live descriptor only.
    let set_result = unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
    if set_result < 0 {
        return Err(BrokerError::Io(std::io::Error::last_os_error().kind()));
    }
    Ok(())
}
