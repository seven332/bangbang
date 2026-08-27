//! Shared macOS host-network implementation used by the VMM and its narrow
//! vmnet provider process.

#[doc(hidden)]
#[cfg(target_os = "macos")]
#[path = "provider_host_network.rs"]
pub mod host_network;
