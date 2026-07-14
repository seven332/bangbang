//! Production macOS bundle construction and launcher supervision.

mod error;
mod layout;
#[cfg(target_os = "macos")]
mod macos;
mod package;
mod supervisor;

pub use error::{LauncherError, PackageError};
pub use layout::{
    BundleLayout, LAUNCHER_BUNDLE_IDENTIFIER, LAUNCHER_EXECUTABLE_NAME, OUTER_BUNDLE_NAME,
    WORKER_BUNDLE_IDENTIFIER, WORKER_BUNDLE_NAME, WORKER_EXECUTABLE_NAME,
};
pub use package::{PackageOptions, build_bundle};
pub use supervisor::{LauncherExit, launch_embedded_worker};
