use std::ffi::OsString;
use std::path::PathBuf;

/// Inputs for one immutable production bundle publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageOptions {
    /// Already-built production launcher executable.
    pub launcher_binary: PathBuf,
    /// Already-built direct VMM executable copied into the sandbox worker app.
    pub worker_binary: PathBuf,
    /// Final output, whose file name must be `Bangbang.app` and must not exist.
    pub output_bundle: PathBuf,
    /// One identity for both separately signed code objects; `-` selects ad-hoc signing.
    pub signing_identity: OsString,
    /// Hidden repository-integration resource tree copied before signing.
    pub test_worker_resources: Option<PathBuf>,
}
