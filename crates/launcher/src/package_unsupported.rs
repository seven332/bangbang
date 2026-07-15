use std::path::PathBuf;

use crate::{PackageError, PackageOptions};

/// Reports that production bundle construction is unavailable on this target.
pub fn build_bundle(options: &PackageOptions) -> Result<PathBuf, PackageError> {
    let _ = options;
    Err(PackageError::UnsupportedPlatform)
}

/// Reports that production vmnet preflight is unavailable on this target.
pub fn preflight_bundle(options: &PackageOptions) -> Result<(), PackageError> {
    let _ = options;
    Err(PackageError::UnsupportedPlatform)
}
