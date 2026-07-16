//! Production macOS bundle construction and launcher supervision.

/// Private immediate-success command used only by the disposable vmnet AMFI probe.
#[doc(hidden)]
pub const VMNET_AUTHORIZATION_PROBE_ARG: &str = "--private-vmnet-authorization-probe";

mod error;
#[cfg(target_os = "macos")]
mod grant_manifest;
#[cfg(target_os = "macos")]
mod launch_policy;
mod layout;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "macos", test))]
mod package;
#[cfg(not(any(target_os = "macos", test)))]
#[path = "package_unsupported.rs"]
mod package;
mod package_options;
#[cfg(any(target_os = "macos", test))]
mod provisioning_profile;
mod supervisor;

pub use error::{JailerIsolationArgument, LauncherError, PackageError};
pub use layout::{
    BundleLayout, LAUNCHER_BUNDLE_IDENTIFIER, LAUNCHER_EXECUTABLE_NAME, OUTER_BUNDLE_NAME,
    WORKER_BUNDLE_IDENTIFIER, WORKER_BUNDLE_NAME, WORKER_EXECUTABLE_NAME,
};
pub use package::{build_bundle, preflight_bundle};
pub use package_options::{PackageOptions, PackageProfile};
pub use supervisor::{LauncherExit, launch_embedded_worker};
