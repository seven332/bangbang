use std::path::PathBuf;

use crate::{PackageError, PackageOptions};

/// Reports that production bundle construction is unavailable on this target.
pub fn build_bundle(options: &PackageOptions) -> Result<PathBuf, PackageError> {
    let _ = options;
    Err(PackageError::UnsupportedPlatform)
}
