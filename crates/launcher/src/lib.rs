//! Production macOS bundle construction and launcher supervision.

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
mod supervisor;

pub use error::{LauncherError, PackageError};
pub use layout::{
    BundleLayout, LAUNCHER_BUNDLE_IDENTIFIER, LAUNCHER_EXECUTABLE_NAME, OUTER_BUNDLE_NAME,
    WORKER_BUNDLE_IDENTIFIER, WORKER_BUNDLE_NAME, WORKER_EXECUTABLE_NAME,
};
pub use package::build_bundle;
pub use package_options::PackageOptions;
pub use supervisor::{LauncherExit, launch_embedded_worker};
