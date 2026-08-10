//! Feature-only launcher authority for the fixed final guest API listener.

use std::ffi::CStr;
use std::io;
use std::mem::{MaybeUninit, size_of};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixListener;

use bangbang_session::elevated_probe::{GUEST_API_SOCKET_CHILD, RuntimeWorkload};
use bangbang_session::macos::runtime::SocketOwnershipRecord;
use bangbang_session::macos::set_cloexec;
use bangbang_session::{ObjectIdentity, ResourceRole, SocketChild};

use crate::grant_manifest::{ElevatedGuestContract, SocketDirectoryAnchor};

use super::scoped_cwd::{ScopedCwdOperationError, directory_descriptor_identity, with_scoped_cwd};

const LISTEN_BACKLOG: libc::c_int = 128;
const FINAL_CHILD: &CStr = c"evidence-api.sock";

/// Redacted failure while creating or owning the fixed API listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ElevatedApiListenerError {
    Io(io::ErrorKind),
    Invalid,
    PathExists,
    PathChanged,
    Cwd,
}

/// Strict post-reap policy for the one feature-owned final socket.
#[derive(Clone, Copy)]
struct ElevatedApiCleanupPolicy {
    anchor_descriptor: RawFd,
    anchor_identity: ObjectIdentity,
    target_uid: u32,
    target_gid: u32,
    path_identity: ObjectIdentity,
}

impl std::fmt::Debug for ElevatedApiCleanupPolicy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ElevatedApiCleanupPolicy(<redacted>)")
    }
}

/// Listener alias, durable-record value, and exact retained pathname cleanup.
pub(crate) struct ElevatedApiPublication {
    listener: Option<UnixListener>,
    record: SocketOwnershipRecord,
    path_guard: ElevatedApiPathGuard,
}

impl std::fmt::Debug for ElevatedApiPublication {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ElevatedApiPublication(<redacted>)")
    }
}

impl ElevatedApiPublication {
    pub(crate) fn listener_fd(&self) -> Option<RawFd> {
        self.listener.as_ref().map(AsRawFd::as_raw_fd)
    }

    pub(crate) fn release_listener_alias(&mut self) -> Result<(), ElevatedApiListenerError> {
        self.listener
            .take()
            .map(drop)
            .ok_or(ElevatedApiListenerError::Invalid)
    }

    #[cfg(test)]
    const fn record(&self) -> &SocketOwnershipRecord {
        &self.record
    }

    pub(crate) const fn path_identity(&self) -> ObjectIdentity {
        self.record.identity()
    }

    pub(crate) fn validate_path(&self) -> Result<(), ElevatedApiListenerError> {
        if self.record.identity() != self.path_guard.policy.path_identity {
            return Err(ElevatedApiListenerError::Invalid);
        }
        self.path_guard.validate()
    }

    pub(crate) fn cleanup(&mut self) -> Result<(), ElevatedApiListenerError> {
        self.listener.take();
        self.path_guard.cleanup()
    }
}

/// Binds the one fixed API listener beneath the contract's retained API anchor.
pub(crate) fn bind_elevated_api_listener(
    contract: ElevatedGuestContract,
    target_uid: u32,
    target_gid: u32,
) -> Result<ElevatedApiPublication, ElevatedApiListenerError> {
    if contract.workload() != RuntimeWorkload::GuestApi {
        return Err(ElevatedApiListenerError::Invalid);
    }
    let anchor = contract
        .api_anchor()
        .ok_or(ElevatedApiListenerError::Invalid)?;
    bind_at_anchor(anchor, target_uid, target_gid)
}

fn bind_at_anchor(
    anchor: SocketDirectoryAnchor,
    target_uid: u32,
    target_gid: u32,
) -> Result<ElevatedApiPublication, ElevatedApiListenerError> {
    if !credentials_match(target_uid, target_gid)
        || directory_descriptor_identity(anchor.descriptor())
            .map_err(|_| ElevatedApiListenerError::Invalid)?
            != anchor.identity()
    {
        return Err(ElevatedApiListenerError::Invalid);
    }
    let publication = with_scoped_cwd(anchor.descriptor(), anchor.identity(), || {
        bind_relative(anchor, target_uid, target_gid)
    })
    .map_err(map_scoped_cwd_error)?;
    if !credentials_match(target_uid, target_gid)
        || directory_descriptor_identity(anchor.descriptor())
            .map_err(|_| ElevatedApiListenerError::Invalid)?
            != anchor.identity()
        || publication.validate_path().is_err()
        || publication
            .listener_fd()
            .is_none_or(|descriptor| validate_listener_descriptor(descriptor).is_err())
    {
        return Err(ElevatedApiListenerError::PathChanged);
    }
    Ok(publication)
}

fn bind_relative(
    anchor: SocketDirectoryAnchor,
    target_uid: u32,
    target_gid: u32,
) -> Result<ElevatedApiPublication, ElevatedApiListenerError> {
    ensure_child_absent()?;
    // SAFETY: A successful descriptor is immediately wrapped for unique ownership.
    let descriptor = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if descriptor < 0 {
        return Err(ElevatedApiListenerError::Io(
            io::Error::last_os_error().kind(),
        ));
    }
    // SAFETY: The fresh descriptor has no other owner.
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    set_cloexec(descriptor.as_raw_fd())
        .map_err(|error| ElevatedApiListenerError::Io(error.kind()))?;
    set_nonblocking(descriptor.as_raw_fd())?;
    let (address, address_length) = fixed_relative_address()?;
    // SAFETY: The descriptor and fully initialized bounded address remain live.
    if unsafe {
        libc::bind(
            descriptor.as_raw_fd(),
            (&raw const address).cast(),
            address_length,
        )
    } != 0
    {
        let error = io::Error::last_os_error();
        return if matches!(
            error.kind(),
            io::ErrorKind::AlreadyExists | io::ErrorKind::AddrInUse
        ) {
            Err(ElevatedApiListenerError::PathExists)
        } else {
            Err(ElevatedApiListenerError::Io(error.kind()))
        };
    }

    let path_identity = relative_socket_identity(target_uid, target_gid, None)?;
    let policy = ElevatedApiCleanupPolicy {
        anchor_descriptor: anchor.descriptor(),
        anchor_identity: anchor.identity(),
        target_uid,
        target_gid,
        path_identity,
    };
    let mut path_guard = ElevatedApiPathGuard {
        policy,
        expected_mode: None,
        armed: true,
    };
    // SAFETY: The fixed relative path names the identity-captured socket just bound.
    if unsafe { libc::chmod(FINAL_CHILD.as_ptr(), 0o600) } != 0 {
        return Err(ElevatedApiListenerError::Io(
            io::Error::last_os_error().kind(),
        ));
    }
    path_guard.expected_mode = Some(0o600);
    if relative_socket_identity(target_uid, target_gid, Some(0o600))? != path_identity {
        return Err(ElevatedApiListenerError::PathChanged);
    }
    // SAFETY: The live stream socket is ready to become a listener.
    if unsafe { libc::listen(descriptor.as_raw_fd(), LISTEN_BACKLOG) } != 0 {
        return Err(ElevatedApiListenerError::Io(
            io::Error::last_os_error().kind(),
        ));
    }
    validate_listener_descriptor(descriptor.as_raw_fd())?;
    path_guard.validate()?;
    let child = SocketChild::parse(GUEST_API_SOCKET_CHILD)
        .map_err(|_| ElevatedApiListenerError::Invalid)?;
    let record = SocketOwnershipRecord::new(ResourceRole::ApiSocketDirectory, child, path_identity)
        .map_err(|_| ElevatedApiListenerError::Invalid)?;
    Ok(ElevatedApiPublication {
        listener: Some(UnixListener::from(descriptor)),
        record,
        path_guard,
    })
}

struct ElevatedApiPathGuard {
    policy: ElevatedApiCleanupPolicy,
    expected_mode: Option<u32>,
    armed: bool,
}

impl ElevatedApiPathGuard {
    fn validate(&self) -> Result<(), ElevatedApiListenerError> {
        if !self.armed
            || directory_descriptor_identity(self.policy.anchor_descriptor)
                .map_err(|_| ElevatedApiListenerError::Invalid)?
                != self.policy.anchor_identity
        {
            return Err(ElevatedApiListenerError::PathChanged);
        }
        let identity = socket_identity_at(
            self.policy.anchor_descriptor,
            self.policy.target_uid,
            self.policy.target_gid,
            self.expected_mode,
        )?;
        if identity != self.policy.path_identity {
            return Err(ElevatedApiListenerError::PathChanged);
        }
        Ok(())
    }

    fn cleanup(&mut self) -> Result<(), ElevatedApiListenerError> {
        if !self.armed {
            return Ok(());
        }
        self.armed = false;
        if directory_descriptor_identity(self.policy.anchor_descriptor).ok()
            != Some(self.policy.anchor_identity)
        {
            return Ok(());
        }
        let identity = match socket_identity_at(
            self.policy.anchor_descriptor,
            self.policy.target_uid,
            self.policy.target_gid,
            self.expected_mode,
        ) {
            Ok(identity) => identity,
            Err(ElevatedApiListenerError::Io(io::ErrorKind::NotFound))
            | Err(ElevatedApiListenerError::Invalid)
            | Err(ElevatedApiListenerError::PathChanged) => return Ok(()),
            Err(error) => return Err(error),
        };
        if identity != self.policy.path_identity {
            return Ok(());
        }
        // SAFETY: The retained anchor and fixed child name the exact validated socket.
        if unsafe { libc::unlinkat(self.policy.anchor_descriptor, FINAL_CHILD.as_ptr(), 0) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::NotFound {
            Ok(())
        } else {
            Err(ElevatedApiListenerError::Io(error.kind()))
        }
    }
}

impl Drop for ElevatedApiPathGuard {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn map_scoped_cwd_error(
    error: ScopedCwdOperationError<ElevatedApiListenerError>,
) -> ElevatedApiListenerError {
    match error {
        ScopedCwdOperationError::Boundary(_) => ElevatedApiListenerError::Cwd,
        ScopedCwdOperationError::Operation(error) => error,
    }
}

fn credentials_match(target_uid: u32, target_gid: u32) -> bool {
    // SAFETY: Identity calls have no pointer or ownership contract.
    unsafe {
        libc::getuid() == target_uid
            && libc::geteuid() == target_uid
            && libc::getgid() == target_gid
            && libc::getegid() == target_gid
    }
}

fn ensure_child_absent() -> Result<(), ElevatedApiListenerError> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: The fixed relative child and writable stat storage remain valid.
    if unsafe {
        libc::fstatat(
            libc::AT_FDCWD,
            FINAL_CHILD.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == 0
    {
        return Err(ElevatedApiListenerError::PathExists);
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::NotFound {
        Ok(())
    } else {
        Err(ElevatedApiListenerError::Io(error.kind()))
    }
}

fn relative_socket_identity(
    expected_uid: u32,
    expected_gid: u32,
    expected_mode: Option<u32>,
) -> Result<ObjectIdentity, ElevatedApiListenerError> {
    socket_identity_at(libc::AT_FDCWD, expected_uid, expected_gid, expected_mode)
}

fn socket_identity_at(
    directory: RawFd,
    expected_uid: u32,
    expected_gid: u32,
    expected_mode: Option<u32>,
) -> Result<ObjectIdentity, ElevatedApiListenerError> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: The retained anchor, fixed child, and writable stat remain live.
    if unsafe {
        libc::fstatat(
            directory,
            FINAL_CHILD.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(ElevatedApiListenerError::Io(
            io::Error::last_os_error().kind(),
        ));
    }
    // SAFETY: Successful fstatat initialized the complete structure.
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFSOCK
        || stat.st_uid != expected_uid
        || stat.st_gid != expected_gid
        || stat.st_nlink != 1
        || expected_mode.is_some_and(|mode| u32::from(stat.st_mode & 0o7777) != mode)
    {
        return Err(ElevatedApiListenerError::PathChanged);
    }
    let identity = ObjectIdentity {
        device: u64::from(u32::from_ne_bytes(stat.st_dev.to_ne_bytes())),
        inode: stat.st_ino,
    };
    if identity.device == 0 || identity.inode == 0 {
        return Err(ElevatedApiListenerError::Invalid);
    }
    Ok(identity)
}

fn set_nonblocking(descriptor: RawFd) -> Result<(), ElevatedApiListenerError> {
    // SAFETY: F_GETFL inspects the live descriptor.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 {
        return Err(ElevatedApiListenerError::Io(
            io::Error::last_os_error().kind(),
        ));
    }
    // SAFETY: F_SETFL updates status flags on the same live descriptor.
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(ElevatedApiListenerError::Io(
            io::Error::last_os_error().kind(),
        ));
    }
    Ok(())
}

fn fixed_relative_address() -> Result<(libc::sockaddr_un, libc::socklen_t), ElevatedApiListenerError>
{
    let bytes = FINAL_CHILD.to_bytes_with_nul();
    let address = MaybeUninit::<libc::sockaddr_un>::zeroed();
    // SAFETY: Zeroed sockaddr_un is valid before initialization below.
    let mut address = unsafe { address.assume_init() };
    if bytes.len() > address.sun_path.len() {
        return Err(ElevatedApiListenerError::Invalid);
    }
    address.sun_family = libc::sa_family_t::try_from(libc::AF_UNIX)
        .map_err(|_| ElevatedApiListenerError::Invalid)?;
    address.sun_len = u8::try_from(
        std::mem::offset_of!(libc::sockaddr_un, sun_path)
            .checked_add(bytes.len())
            .ok_or(ElevatedApiListenerError::Invalid)?,
    )
    .map_err(|_| ElevatedApiListenerError::Invalid)?;
    // SAFETY: The bounded source including NUL fits the destination array.
    unsafe {
        std::ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            address.sun_path.as_mut_ptr().cast::<u8>(),
            bytes.len(),
        );
    }
    Ok((address, libc::socklen_t::from(address.sun_len)))
}

fn validate_listener_descriptor(descriptor: RawFd) -> Result<(), ElevatedApiListenerError> {
    // SAFETY: F_GETFD and F_GETFL inspect the live listener.
    let descriptor_flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    // SAFETY: F_GETFL inspects the same live listener.
    let status_flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if descriptor_flags < 0
        || status_flags < 0
        || descriptor_flags & libc::FD_CLOEXEC == 0
        || status_flags & libc::O_NONBLOCK == 0
        || socket_int_option(descriptor, libc::SO_TYPE)? != libc::SOCK_STREAM
    {
        return Err(ElevatedApiListenerError::Invalid);
    }

    let mut address = MaybeUninit::<libc::sockaddr_un>::zeroed();
    let mut address_length = libc::socklen_t::try_from(size_of::<libc::sockaddr_un>())
        .map_err(|_| ElevatedApiListenerError::Invalid)?;
    // SAFETY: Address storage and length are writable for the live listener.
    if unsafe {
        libc::getsockname(
            descriptor,
            address.as_mut_ptr().cast(),
            &raw mut address_length,
        )
    } != 0
    {
        return Err(ElevatedApiListenerError::Io(
            io::Error::last_os_error().kind(),
        ));
    }
    // SAFETY: Successful getsockname initialized the returned address prefix.
    let address = unsafe { address.assume_init() };
    if address.sun_family
        != libc::sa_family_t::try_from(libc::AF_UNIX)
            .map_err(|_| ElevatedApiListenerError::Invalid)?
    {
        return Err(ElevatedApiListenerError::Invalid);
    }
    let path_length = usize::try_from(address_length)
        .map_err(|_| ElevatedApiListenerError::Invalid)?
        .checked_sub(std::mem::offset_of!(libc::sockaddr_un, sun_path))
        .ok_or(ElevatedApiListenerError::Invalid)?;
    if path_length != FINAL_CHILD.to_bytes_with_nul().len() || path_length > address.sun_path.len()
    {
        return Err(ElevatedApiListenerError::Invalid);
    }
    // SAFETY: Kernel-returned length bounds the path read within `address`.
    let path =
        unsafe { std::slice::from_raw_parts(address.sun_path.as_ptr().cast::<u8>(), path_length) };
    if path != FINAL_CHILD.to_bytes_with_nul() {
        return Err(ElevatedApiListenerError::Invalid);
    }

    // SAFETY: Null address arguments probe for an already queued client only.
    let accepted = unsafe { libc::accept(descriptor, std::ptr::null_mut(), std::ptr::null_mut()) };
    if accepted >= 0 {
        // SAFETY: A successful accept returns a uniquely owned descriptor.
        drop(unsafe { OwnedFd::from_raw_fd(accepted) });
        return Err(ElevatedApiListenerError::Invalid);
    }
    if io::Error::last_os_error().kind() != io::ErrorKind::WouldBlock {
        return Err(ElevatedApiListenerError::Invalid);
    }
    Ok(())
}

fn socket_int_option(
    descriptor: RawFd,
    option: libc::c_int,
) -> Result<libc::c_int, ElevatedApiListenerError> {
    let mut value = 0;
    let mut length = libc::socklen_t::try_from(size_of::<libc::c_int>())
        .map_err(|_| ElevatedApiListenerError::Invalid)?;
    // SAFETY: Option storage and length are writable for the live listener.
    if unsafe {
        libc::getsockopt(
            descriptor,
            libc::SOL_SOCKET,
            option,
            (&raw mut value).cast(),
            &raw mut length,
        )
    } != 0
    {
        return Err(ElevatedApiListenerError::Io(
            io::Error::last_os_error().kind(),
        ));
    }
    if usize::try_from(length).ok() != Some(size_of::<libc::c_int>()) {
        return Err(ElevatedApiListenerError::Invalid);
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    use super::*;

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn create() -> Self {
            let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "bangbang-elevated-api-listener-{}-{id}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("test directory should create");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                .expect("test directory mode should set");
            Self(path)
        }

        fn anchor(&self) -> (File, SocketDirectoryAnchor) {
            let file = File::open(&self.0).expect("test directory should open");
            let identity = directory_descriptor_identity(file.as_raw_fd())
                .expect("anchor identity should inspect");
            let anchor = SocketDirectoryAnchor::for_test(file.as_raw_fd(), identity);
            (file, anchor)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let child = self.0.join(GUEST_API_SOCKET_CHILD);
            if let Ok(metadata) = fs::symlink_metadata(&child) {
                if metadata.file_type().is_dir() {
                    let _ = fs::remove_dir(&child);
                } else {
                    let _ = fs::remove_file(&child);
                }
            }
            let _ = fs::remove_dir(&self.0);
        }
    }

    fn current_ids() -> (u32, u32) {
        // SAFETY: Effective identity calls have no pointer or ownership contract.
        unsafe { (libc::geteuid(), libc::getegid()) }
    }

    #[test]
    fn final_listener_is_exact_lives_past_alias_close_and_cleans() {
        let directory = TestDir::create();
        let (_file, anchor) = directory.anchor();
        let (uid, gid) = current_ids();
        let mut publication = bind_at_anchor(anchor, uid, gid).expect("fixed listener should bind");
        assert!(publication.listener_fd().is_some());
        assert_eq!(
            publication.record().role(),
            ResourceRole::ApiSocketDirectory
        );
        assert_eq!(
            publication.record().child().as_bytes(),
            GUEST_API_SOCKET_CHILD.as_bytes()
        );
        assert_eq!(publication.record().identity(), publication.path_identity());
        publication.validate_path().expect("path should validate");
        assert!(!directory.0.join(".api-socket.pending").exists());

        publication
            .release_listener_alias()
            .expect("listener alias should release");
        assert!(directory.0.join(GUEST_API_SOCKET_CHILD).exists());
        publication.cleanup().expect("path should clean");
        assert!(!directory.0.join(GUEST_API_SOCKET_CHILD).exists());
    }

    #[test]
    fn collisions_and_wrong_anchor_identity_fail_without_replacement() {
        let directory = TestDir::create();
        let (_file, anchor) = directory.anchor();
        let (uid, gid) = current_ids();
        let child = directory.0.join(GUEST_API_SOCKET_CHILD);
        File::create(&child).expect("collision file should create");
        assert!(matches!(
            bind_at_anchor(anchor, uid, gid),
            Err(ElevatedApiListenerError::PathExists)
        ));
        assert!(child.is_file());
        fs::remove_file(&child).expect("collision file should remove");
        symlink("missing", &child).expect("collision symlink should create");
        assert!(matches!(
            bind_at_anchor(anchor, uid, gid),
            Err(ElevatedApiListenerError::PathExists)
        ));
        assert!(
            fs::symlink_metadata(&child)
                .expect("symlink should inspect")
                .file_type()
                .is_symlink()
        );
        fs::remove_file(&child).expect("collision symlink should remove");

        let wrong = SocketDirectoryAnchor::for_test(
            anchor.descriptor(),
            ObjectIdentity {
                device: anchor.identity().device,
                inode: anchor.identity().inode.saturating_add(1),
            },
        );
        assert!(matches!(
            bind_at_anchor(wrong, uid, gid),
            Err(ElevatedApiListenerError::Invalid)
        ));
        assert!(!child.exists());
    }

    #[test]
    fn cleanup_preserves_a_replaced_pathname() {
        let directory = TestDir::create();
        let (_file, anchor) = directory.anchor();
        let (uid, gid) = current_ids();
        let mut publication = bind_at_anchor(anchor, uid, gid).expect("fixed listener should bind");
        publication
            .release_listener_alias()
            .expect("listener alias should release");
        let child = directory.0.join(GUEST_API_SOCKET_CHILD);
        fs::remove_file(&child).expect("owned socket should unlink for replacement");
        File::create(&child).expect("replacement should create");
        publication
            .cleanup()
            .expect("replacement should be preserved");
        assert!(child.is_file());
    }

    #[test]
    fn independent_anchors_bind_concurrently_under_the_shared_cwd_boundary() {
        let directories = [TestDir::create(), TestDir::create()];
        let (uid, gid) = current_ids();
        thread::scope(|scope| {
            for directory in &directories {
                scope.spawn(move || {
                    let (_file, anchor) = directory.anchor();
                    let mut publication = bind_at_anchor(anchor, uid, gid)
                        .expect("concurrent fixed listener should bind");
                    publication.validate_path().expect("path should validate");
                    publication.cleanup().expect("path should clean");
                });
            }
        });
        for directory in &directories {
            assert!(!directory.0.join(GUEST_API_SOCKET_CHILD).exists());
        }
    }

    #[test]
    fn strict_metadata_validation_rejects_expected_owner_group_and_mode_changes() {
        let directory = TestDir::create();
        let (_file, anchor) = directory.anchor();
        let (uid, gid) = current_ids();
        let mut publication = bind_at_anchor(anchor, uid, gid).expect("fixed listener should bind");
        let child = directory.0.join(GUEST_API_SOCKET_CHILD);
        assert_eq!(
            socket_identity_at(anchor.descriptor(), uid ^ 1, gid, Some(0o600)),
            Err(ElevatedApiListenerError::PathChanged)
        );
        assert_eq!(
            socket_identity_at(anchor.descriptor(), uid, gid ^ 1, Some(0o600)),
            Err(ElevatedApiListenerError::PathChanged)
        );
        fs::set_permissions(&child, fs::Permissions::from_mode(0o666))
            .expect("socket mode should mutate");
        assert_eq!(
            publication.validate_path(),
            Err(ElevatedApiListenerError::PathChanged)
        );
        publication
            .cleanup()
            .expect("tampered mode should be preserved");
        let metadata = fs::symlink_metadata(&child).expect("socket should remain");
        assert_eq!(metadata.mode() & 0o7777, 0o666);
        assert_eq!(metadata.uid(), uid);
        assert_eq!(metadata.gid(), gid);
    }
}
