use std::ffi::{CStr, OsStr, OsString};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use bangbang_session::elevated_probe::{
    LAUNCHER_ACTIVATION, ProbeBootstrap, ProbeMode, WORKER_ACTIVATION,
};
use bangbang_session::{ObjectIdentity, SessionId};

use crate::LauncherError;

const ROOT_OPTION: &str = "--root";
const TARGET_UID_OPTION: &str = "--target-uid";
const TARGET_GID_OPTION: &str = "--target-gid";
const MODE_OPTION: &str = "--mode";
const DELIMITER: &str = "--";
const MAX_ROOT_PATH_BYTES: usize = 1024;
const ROOT_CHILD_PREFIX: &str = "bangbang-elevated-probe.";
const ROOT_CHILD_SUFFIX_BYTES: usize = 8;

pub(crate) struct Config {
    root: OwnedFd,
    root_identity: ObjectIdentity,
    target_uid: u32,
    target_gid: u32,
    mode: ProbeMode,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ElevatedProbeConfig(<redacted>)")
    }
}

impl Config {
    pub(crate) fn parse(
        args: Vec<OsString>,
    ) -> Result<(Option<Self>, Vec<OsString>), LauncherError> {
        if args
            .first()
            .is_none_or(|arg| arg != OsStr::new(LAUNCHER_ACTIVATION))
        {
            return Ok((None, args));
        }
        if args.len() != 10
            || args.get(1) != Some(&OsString::from(ROOT_OPTION))
            || args.get(3) != Some(&OsString::from(TARGET_UID_OPTION))
            || args.get(5) != Some(&OsString::from(TARGET_GID_OPTION))
            || args.get(7) != Some(&OsString::from(MODE_OPTION))
            || args.get(9) != Some(&OsString::from(DELIMITER))
        {
            return Err(LauncherError::InvalidLaunchPolicy);
        }
        let root_path = args
            .get(2)
            .filter(|value| {
                !value.is_empty()
                    && value.as_bytes().len() <= MAX_ROOT_PATH_BYTES
                    && !value.as_bytes().contains(&0)
                    && value.to_str().is_some()
            })
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or(LauncherError::InvalidLaunchPolicy)?;
        let target_uid = parse_u32(args.get(4))?;
        let target_gid = parse_u32(args.get(6))?;
        let mode = args
            .get(8)
            .and_then(|value| value.to_str())
            .and_then(|value| ProbeMode::parse(value, target_uid, target_gid))
            .ok_or(LauncherError::InvalidLaunchPolicy)?;
        validate_initial_root()?;
        let (root, root_identity) = open_private_root(&root_path)?;
        Ok((
            Some(Self {
                root,
                root_identity,
                target_uid,
                target_gid,
                mode,
            }),
            Vec::new(),
        ))
    }

    pub(crate) fn prepend_worker_activation(
        &self,
        worker_args: &mut Vec<OsString>,
    ) -> Result<(), LauncherError> {
        if !worker_args.is_empty() {
            return Err(LauncherError::InvalidLaunchPolicy);
        }
        worker_args.push(OsString::from(WORKER_ACTIVATION));
        Ok(())
    }

    pub(crate) fn root_fd(&self) -> RawFd {
        self.root.as_raw_fd()
    }

    pub(crate) const fn mode(&self) -> ProbeMode {
        self.mode
    }

    pub(crate) fn bootstrap(&self) -> Result<ProbeBootstrap, LauncherError> {
        let nonce = SessionId::generate().map_err(|_| LauncherError::SessionProtocol)?;
        ProbeBootstrap::new(
            self.mode,
            self.target_uid,
            self.target_gid,
            self.root_identity,
            nonce,
        )
        .map_err(|_| LauncherError::SessionProtocol)
    }

    pub(crate) fn run_control(&self) -> Result<(), LauncherError> {
        if self.mode != ProbeMode::Control {
            return Err(LauncherError::InvalidLaunchPolicy);
        }
        // SAFETY: `root` is the exact retained private directory validated by
        // descriptor and this root-only control process is single-threaded.
        if unsafe { libc::fchdir(self.root.as_raw_fd()) } != 0 {
            return Err(LauncherError::SessionProtocol);
        }
        // SAFETY: Cwd is the retained root and the fixed relative string is
        // NUL-terminated. This signed launcher intentionally exits afterward.
        if unsafe { libc::chroot(c".".as_ptr()) } != 0 {
            return Err(LauncherError::SessionProtocol);
        }
        // SAFETY: The process is inside the private root and the fixed absolute
        // path is NUL-terminated.
        if unsafe { libc::chdir(c"/".as_ptr()) } != 0 {
            return Err(LauncherError::SessionProtocol);
        }
        Ok(())
    }
}

fn parse_u32(value: Option<&OsString>) -> Result<u32, LauncherError> {
    value
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .filter(|value| value.len() == 1 || !value.starts_with('0'))
        .and_then(|value| value.parse().ok())
        .ok_or(LauncherError::InvalidLaunchPolicy)
}

fn validate_initial_root() -> Result<(), LauncherError> {
    // SAFETY: Credential getters have no pointer or ownership contract.
    let identities = unsafe {
        (
            libc::getuid(),
            libc::geteuid(),
            libc::getgid(),
            libc::getegid(),
        )
    };
    if identities == (0, 0, 0, 0) {
        Ok(())
    } else {
        Err(LauncherError::InvalidLaunchPolicy)
    }
}

fn open_private_root(path: &Path) -> Result<(OwnedFd, ObjectIdentity), LauncherError> {
    let child = private_root_child(path).ok_or(LauncherError::InvalidLaunchPolicy)?;
    let mut directory = open_directory(c"/")?;
    validate_ancestor(directory.as_raw_fd())?;
    for component in [c"private", c"var", c"root"] {
        directory = openat_directory(directory.as_raw_fd(), component)?;
        validate_ancestor(directory.as_raw_fd())?;
    }
    let child =
        std::ffi::CString::new(child.as_bytes()).map_err(|_| LauncherError::InvalidLaunchPolicy)?;
    let root = openat_directory(directory.as_raw_fd(), &child)?;
    let stat = descriptor_stat(root.as_raw_fd())?;
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR
        || stat.st_mode & 0o7777 != 0o700
        || stat.st_uid != 0
        || stat.st_gid != 0
        || stat.st_nlink < 2
    {
        return Err(LauncherError::InvalidLaunchPolicy);
    }
    let identity = stat_identity(&stat);
    if identity.device == 0 || identity.inode == 0 {
        return Err(LauncherError::InvalidLaunchPolicy);
    }
    Ok((root, identity))
}

fn private_root_child(path: &Path) -> Option<&OsStr> {
    let mut components = path.components();
    if components.next() != Some(Component::RootDir)
        || components.next() != Some(Component::Normal(OsStr::new("private")))
        || components.next() != Some(Component::Normal(OsStr::new("var")))
        || components.next() != Some(Component::Normal(OsStr::new("root")))
    {
        return None;
    }
    let child = match components.next()? {
        Component::Normal(child) => child,
        _ => return None,
    };
    if components.next().is_some() {
        return None;
    }
    let child = child.to_str()?;
    let suffix = child.strip_prefix(ROOT_CHILD_PREFIX)?;
    if suffix.len() != ROOT_CHILD_SUFFIX_BYTES
        || !suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return None;
    }
    Some(OsStr::new(child))
}

fn open_directory(path: &CStr) -> Result<OwnedFd, LauncherError> {
    // SAFETY: `path` is NUL-terminated and no pointer is retained.
    let descriptor = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    owned_descriptor(descriptor)
}

fn openat_directory(parent: RawFd, child: &CStr) -> Result<OwnedFd, LauncherError> {
    // SAFETY: `parent` is a retained directory, `child` is NUL-terminated, and
    // no pointer is retained.
    let descriptor = unsafe {
        libc::openat(
            parent,
            child.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    owned_descriptor(descriptor)
}

fn owned_descriptor(descriptor: RawFd) -> Result<OwnedFd, LauncherError> {
    if descriptor < 0 {
        Err(LauncherError::InvalidLaunchPolicy)
    } else {
        // SAFETY: `descriptor` is a fresh successful result whose ownership is
        // transferred exactly once.
        Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
    }
}

fn validate_ancestor(descriptor: RawFd) -> Result<(), LauncherError> {
    let stat = descriptor_stat(descriptor)?;
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR
        || stat.st_mode & 0o022 != 0
        || stat.st_uid != 0
        || stat.st_gid != 0
        || stat.st_nlink < 2
    {
        Err(LauncherError::InvalidLaunchPolicy)
    } else {
        Ok(())
    }
}

fn descriptor_stat(descriptor: RawFd) -> Result<libc::stat, LauncherError> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `descriptor` is live and `stat` is writable for one result.
    if unsafe { libc::fstat(descriptor, stat.as_mut_ptr()) } != 0 {
        return Err(LauncherError::InvalidLaunchPolicy);
    }
    // SAFETY: Successful `fstat` initialized the complete value.
    Ok(unsafe { stat.assume_init() })
}

fn stat_identity(stat: &libc::stat) -> ObjectIdentity {
    ObjectIdentity {
        device: u64::from(u32::from_ne_bytes(stat.st_dev.to_ne_bytes())),
        inode: stat.st_ino,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_root_path_is_exact_and_symlink_free_by_shape() {
        assert_eq!(
            private_root_child(Path::new(
                "/private/var/root/bangbang-elevated-probe.A1b2C3d4"
            )),
            Some(OsStr::new("bangbang-elevated-probe.A1b2C3d4"))
        );
        for path in [
            "/tmp/bangbang-elevated-probe.A1b2C3d4",
            "/private/var/root/bangbang-elevated-probe.short",
            "/private/var/root/bangbang-elevated-probe.A1b2C3d4/child",
            "/private/var/root/not-the-probe.A1b2C3d4",
        ] {
            assert_eq!(private_root_child(Path::new(path)), None);
        }
    }

    #[test]
    fn debug_output_redacts_values() {
        let root = std::fs::File::open("/").expect("test root descriptor should open");
        let config = Config {
            root: root.into(),
            root_identity: ObjectIdentity {
                device: 123,
                inode: 456,
            },
            target_uid: 1_234_567_891,
            target_gid: 1_234_567_893,
            mode: ProbeMode::Drop,
        };
        let output = format!("{config:?}");
        assert_eq!(output, "ElevatedProbeConfig(<redacted>)");
        assert!(!output.contains("1234567891"));
        assert!(!output.contains("456"));

        let mut unexpected = vec![OsString::from("--version")];
        assert!(config.prepend_worker_activation(&mut unexpected).is_err());
        assert_eq!(unexpected, [OsString::from("--version")]);
        let mut empty = Vec::new();
        config
            .prepend_worker_activation(&mut empty)
            .expect("empty worker envelope should activate");
        assert_eq!(empty, [OsString::from(WORKER_ACTIVATION)]);
    }

    #[test]
    fn numeric_parser_is_decimal_and_bounded() {
        assert_eq!(parse_u32(Some(&OsString::from("0"))), Ok(0));
        assert_eq!(
            parse_u32(Some(&OsString::from(u32::MAX.to_string()))),
            Ok(u32::MAX)
        );
        for invalid in ["", "00", "0501", "+1", "-1", "4294967296", "1_000", " 1"] {
            assert_eq!(
                parse_u32(Some(&OsString::from(invalid))),
                Err(LauncherError::InvalidLaunchPolicy)
            );
        }
    }
}
