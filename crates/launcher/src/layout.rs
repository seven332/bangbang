use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use crate::LauncherError;

/// Stable outer app-bundle directory name.
pub const OUTER_BUNDLE_NAME: &str = "Bangbang.app";
/// Stable outer app code-signing identifier.
pub const LAUNCHER_BUNDLE_IDENTIFIER: &str = "dev.bangbang";
/// Stable outer app executable name.
pub const LAUNCHER_EXECUTABLE_NAME: &str = "bangbang";
/// Stable nested worker app-bundle directory name.
pub const WORKER_BUNDLE_NAME: &str = "BangbangWorker.app";
/// Stable nested worker code-signing identifier.
pub const WORKER_BUNDLE_IDENTIFIER: &str = "dev.bangbang.worker";
/// Stable nested worker executable name.
pub const WORKER_EXECUTABLE_NAME: &str = "bangbang-worker";
#[cfg(any(target_os = "macos", test))]
pub(crate) const APP_SANDBOX_ENTITLEMENT: &str = "com.apple.security.app-sandbox";
#[cfg(any(target_os = "macos", test))]
pub(crate) const HYPERVISOR_ENTITLEMENT: &str = "com.apple.security.hypervisor";
#[cfg(any(target_os = "macos", test))]
pub(crate) const VMNET_ENTITLEMENT: &str = "com.apple.vm.networking";
#[cfg(any(target_os = "macos", test))]
pub(crate) const APPLICATION_IDENTIFIER_ENTITLEMENT: &str = "com.apple.application-identifier";
#[cfg(any(target_os = "macos", test))]
pub(crate) const TEAM_IDENTIFIER_ENTITLEMENT: &str = "com.apple.developer.team-identifier";

/// Fixed production bundle paths derived from the running launcher executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleLayout {
    outer_bundle: PathBuf,
    launcher_executable: PathBuf,
    worker_bundle: PathBuf,
    worker_executable: PathBuf,
}

impl BundleLayout {
    /// Derives and validates the fixed production hierarchy from a launcher executable path.
    pub fn from_launcher_executable(path: &Path) -> Result<Self, LauncherError> {
        let macos = exact_parent(path, LAUNCHER_EXECUTABLE_NAME, "MacOS")?;
        let contents = exact_parent(&macos, "MacOS", "Contents")?;
        let outer_bundle = exact_parent(&contents, "Contents", OUTER_BUNDLE_NAME)?;

        let worker_bundle = contents.join("Helpers").join(WORKER_BUNDLE_NAME);
        let worker_executable = worker_bundle
            .join("Contents/MacOS")
            .join(WORKER_EXECUTABLE_NAME);
        let layout = Self {
            outer_bundle,
            launcher_executable: path.to_path_buf(),
            worker_bundle,
            worker_executable,
        };
        layout.validate_entries()?;
        Ok(layout)
    }

    /// Returns the outer app-bundle path.
    #[must_use]
    pub fn outer_bundle(&self) -> &Path {
        &self.outer_bundle
    }

    /// Returns the launcher executable path.
    #[must_use]
    pub fn launcher_executable(&self) -> &Path {
        &self.launcher_executable
    }

    /// Returns the nested worker app-bundle path.
    #[must_use]
    pub fn worker_bundle(&self) -> &Path {
        &self.worker_bundle
    }

    /// Returns the nested worker executable path.
    #[must_use]
    pub fn worker_executable(&self) -> &Path {
        &self.worker_executable
    }

    /// Returns the fixed nested worker provisioning-profile path.
    #[must_use]
    #[cfg(target_os = "macos")]
    pub(crate) fn worker_provisioning_profile(&self) -> PathBuf {
        self.worker_bundle
            .join("Contents/embedded.provisionprofile")
    }

    fn validate_entries(&self) -> Result<(), LauncherError> {
        require_plain_dir(&self.outer_bundle)?;
        require_plain_dir(&self.outer_bundle.join("Contents"))?;
        require_plain_dir(&self.outer_bundle.join("Contents/MacOS"))?;
        require_plain_file(&self.launcher_executable)?;
        require_plain_dir(&self.outer_bundle.join("Contents/Helpers"))?;
        require_plain_dir(&self.worker_bundle)?;
        require_plain_dir(&self.worker_bundle.join("Contents"))?;
        require_plain_dir(&self.worker_bundle.join("Contents/MacOS"))?;
        require_plain_file(&self.worker_executable)
    }
}

fn exact_parent(path: &Path, name: &str, parent_name: &str) -> Result<PathBuf, LauncherError> {
    if path.file_name() != Some(OsStr::new(name)) {
        return Err(LauncherError::InvalidBundleLayout);
    }
    let parent = path.parent().ok_or(LauncherError::InvalidBundleLayout)?;
    if parent.file_name() != Some(OsStr::new(parent_name)) {
        return Err(LauncherError::InvalidBundleLayout);
    }
    Ok(parent.to_path_buf())
}

fn require_plain_dir(path: &Path) -> Result<(), LauncherError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| LauncherError::InvalidBundleEntry)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(LauncherError::InvalidBundleEntry);
    }
    Ok(())
}

fn require_plain_file(path: &Path) -> Result<(), LauncherError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| LauncherError::InvalidBundleEntry)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(LauncherError::InvalidBundleEntry);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    #[derive(Debug)]
    struct TestBundle {
        root: PathBuf,
        launcher: PathBuf,
        worker: PathBuf,
    }

    impl TestBundle {
        fn new() -> Self {
            let id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
            let root = std::env::temp_dir().join(format!(
                "bangbang-launcher-layout-{}-{id}",
                std::process::id()
            ));
            let outer = root.join(OUTER_BUNDLE_NAME);
            let launcher = outer.join("Contents/MacOS").join(LAUNCHER_EXECUTABLE_NAME);
            let worker = outer
                .join("Contents/Helpers")
                .join(WORKER_BUNDLE_NAME)
                .join("Contents/MacOS")
                .join(WORKER_EXECUTABLE_NAME);
            fs::create_dir_all(launcher.parent().expect("launcher should have a parent"))
                .expect("launcher directory should be created");
            fs::create_dir_all(worker.parent().expect("worker should have a parent"))
                .expect("worker directory should be created");
            fs::write(&launcher, b"launcher").expect("launcher should be written");
            fs::write(&worker, b"worker").expect("worker should be written");
            Self {
                root,
                launcher,
                worker,
            }
        }
    }

    impl Drop for TestBundle {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn derives_exact_production_layout() {
        let bundle = TestBundle::new();
        let layout = BundleLayout::from_launcher_executable(&bundle.launcher)
            .expect("valid layout should be accepted");
        assert_eq!(layout.worker_executable(), bundle.worker);
        assert_eq!(layout.outer_bundle(), bundle.root.join(OUTER_BUNDLE_NAME));
    }

    #[test]
    fn rejects_wrong_launcher_name() {
        let bundle = TestBundle::new();
        let wrong = bundle.launcher.with_file_name("other");
        fs::write(&wrong, b"launcher").expect("wrong launcher should be written");
        assert_eq!(
            BundleLayout::from_launcher_executable(&wrong),
            Err(LauncherError::InvalidBundleLayout)
        );
    }

    #[test]
    fn rejects_missing_worker() {
        let bundle = TestBundle::new();
        fs::remove_file(&bundle.worker).expect("worker should be removed");
        assert_eq!(
            BundleLayout::from_launcher_executable(&bundle.launcher),
            Err(LauncherError::InvalidBundleEntry)
        );
    }

    #[test]
    fn rejects_symlinked_worker() {
        let bundle = TestBundle::new();
        let target = bundle.root.join("replacement");
        fs::write(&target, b"worker").expect("replacement should be written");
        fs::remove_file(&bundle.worker).expect("worker should be removed");
        symlink(&target, &bundle.worker).expect("worker symlink should be created");
        assert_eq!(
            BundleLayout::from_launcher_executable(&bundle.launcher),
            Err(LauncherError::InvalidBundleEntry)
        );
    }

    #[test]
    fn errors_do_not_include_paths() {
        let display = LauncherError::InvalidBundleEntry.to_string();
        assert_eq!(display, "invalid production bundle entry");
        assert!(!display.contains(Path::new("/private/secret").to_string_lossy().as_ref()));
    }
}
