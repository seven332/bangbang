//! Minimal one-shot root vmnet broker and privilege-dropped interface owner.
//!
//! The portable pieces in this crate own only fixed bootstrap policy,
//! broker/owner process state, and provider-v1 adaptation. The macOS adapter is
//! the sole caller of the system vmnet backend and credential transition.

mod bootstrap;
mod broker;
mod owner;
mod policy;
mod supervision;
mod topology;

#[cfg(target_os = "macos")]
mod macos;

pub use bootstrap::{BROKER_BOOTSTRAP_BYTES, BootstrapError, BrokerBootstrap};
pub use broker::BrokerError;
pub use owner::{
    DroppedOwner, OwnerBackend, OwnerCredentialError, OwnerCredentialOps, OwnerError,
    OwnerReadinessCallback, OwnerStartFailure, PrivilegedOwner,
};
pub use policy::{BoundedBridgeName, ResolvedVmnetPolicy, VmnetBrokerPolicy};
pub use supervision::{
    OWNER_SUPERVISION_BYTES, OwnerBootstrap, OwnerScope, OwnerSupervisionMessage,
};
pub use topology::PUBLIC_BOOTSTRAP_MODE;

#[cfg(target_os = "macos")]
pub use macos::{
    PRIVATE_BROKER_MODE, PRIVATE_DAEMON_BROKER_MODE, PRIVATE_LAUNCHER_TRANSITION_MODE,
    PRIVATE_OWNER_MODE, run_private_broker, run_private_daemon_broker,
    run_private_launcher_transition, run_private_owner, run_public_bootstrap,
};

#[cfg(test)]
mod surface_tests {
    #[test]
    fn privileged_package_has_a_narrow_static_link_and_entry_surface() {
        let manifest = include_str!("../Cargo.toml");
        for required in [
            "bangbang-session = { path = \"../session\" }",
            "bangbang-unix-stream = { path = \"../unix-stream\" }",
            "bangbang = { path = \"../bangbang\", default-features = false }",
        ] {
            assert!(manifest.contains(required));
        }
        for forbidden in [
            "bangbang-api",
            "bangbang-hvf",
            "bangbang-launcher",
            "hyper =",
            "tokio =",
        ] {
            assert!(!manifest.contains(forbidden));
        }

        let entry = include_str!("main.rs");
        assert!(entry.contains("PRIVATE_BROKER_MODE"));
        assert!(entry.contains("PRIVATE_OWNER_MODE"));
        for forbidden in [
            "Command::",
            "TcpListener",
            "UnixListener",
            "api_server",
            "Vmm",
            "Bundle",
            "Grant",
            "sudo",
        ] {
            assert!(!entry.contains(forbidden));
        }

        let bangbang_library = include_str!("../../bangbang/src/lib.rs");
        assert!(bangbang_library.contains("provider_host_network.rs"));
        for forbidden in [
            "mod api_server",
            "mod contained_session",
            "mod vmm",
            "mod elevated_bootstrap_probe",
            "mod grant_integration_probe",
        ] {
            assert!(!bangbang_library.contains(forbidden));
        }
    }
}
