use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

/// Closed worker authority profile selected when assembling a production bundle.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum PackageProfile {
    /// App Sandbox plus Hypervisor, with no provisioning profile or vmnet authority.
    #[default]
    Networkless,
    /// App Sandbox, Hypervisor, and an Apple-authorized vmnet entitlement profile.
    Vmnet,
}

/// Inputs for one immutable production bundle publication.
#[derive(Clone, PartialEq, Eq)]
pub struct PackageOptions {
    /// Already-built production launcher executable.
    pub launcher_binary: PathBuf,
    /// Already-built direct VMM executable copied into the sandbox worker app.
    pub worker_binary: PathBuf,
    /// Final output, whose file name must be `Bangbang.app` and must not exist.
    pub output_bundle: PathBuf,
    /// One identity for both separately signed code objects; `-` selects ad-hoc signing.
    pub signing_identity: OsString,
    /// Exact worker authority profile; networkless remains the default CLI profile.
    pub profile: PackageProfile,
    /// Caller-owned macOS provisioning profile required only by the vmnet profile.
    pub provisioning_profile: Option<PathBuf>,
    /// Hidden repository-integration resource tree copied before signing.
    pub test_worker_resources: Option<PathBuf>,
}

impl fmt::Debug for PackageOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PackageOptions")
            .field("profile", &self.profile)
            .field("inputs", &"<redacted>")
            .finish()
    }
}
