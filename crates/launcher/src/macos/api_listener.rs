//! Launcher-owned staged publication for one ordinary granted API listener.

use std::cell::Cell;
use std::ffi::{CStr, CString};
use std::io;
use std::mem::{MaybeUninit, size_of};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixListener;

use bangbang_session::macos::runtime::{
    SocketOwnershipRecord, WorkerSocketNamespace, socket_staging_name,
};
use bangbang_session::macos::set_cloexec;
use bangbang_session::{ObjectIdentity, ResourceRole, SocketChild};

use crate::grant_manifest::SocketDirectoryAnchor;

use super::scoped_cwd::{ScopedCwdOperationError, directory_descriptor_identity, with_scoped_cwd};

const LISTEN_BACKLOG: libc::c_int = 128;

/// Value-redacted failure while publishing one ordinary API listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApiListenerPublicationError {
    Io(io::ErrorKind),
    Invalid,
    PathExists,
    PathChanged,
    Cwd,
    Record,
}

impl ApiListenerPublicationError {
    pub(crate) const fn category(self) -> io::ErrorKind {
        match self {
            Self::Io(kind) => kind,
            Self::PathExists => io::ErrorKind::AlreadyExists,
            Self::Invalid | Self::PathChanged | Self::Cwd | Self::Record => io::ErrorKind::Other,
        }
    }
}

/// Exact record and listener alias retained until the broker response is sent.
pub(crate) struct ApiListenerPublication {
    listener: Option<UnixListener>,
    record: SocketOwnershipRecord,
}

impl std::fmt::Debug for ApiListenerPublication {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ApiListenerPublication(<redacted>)")
    }
}

impl ApiListenerPublication {
    pub(crate) fn listener_fd(&self) -> Option<RawFd> {
        self.listener.as_ref().map(AsRawFd::as_raw_fd)
    }

    pub(crate) const fn identity(&self) -> ObjectIdentity {
        self.record.identity()
    }

    #[cfg(test)]
    const fn record(&self) -> &SocketOwnershipRecord {
        &self.record
    }

    #[cfg(test)]
    pub(crate) fn from_test_descriptor(descriptor: OwnedFd, identity: ObjectIdentity) -> Self {
        Self {
            listener: Some(UnixListener::from(descriptor)),
            record: SocketOwnershipRecord::new(
                ResourceRole::ApiSocketDirectory,
                SocketChild::parse("test-api.sock").expect("test child should validate"),
                identity,
            )
            .expect("test record should validate"),
        }
    }

    pub(crate) fn release_listener_alias(&mut self) -> Result<(), ApiListenerPublicationError> {
        self.listener
            .take()
            .map(drop)
            .ok_or(ApiListenerPublicationError::Invalid)
    }
}

/// Publishes one fixed-role API listener through private staging and a durable record.
pub(crate) fn publish_api_listener(
    namespace: WorkerSocketNamespace,
    anchor: SocketDirectoryAnchor,
    child: SocketChild,
) -> Result<ApiListenerPublication, ApiListenerPublicationError> {
    let namespace_identity = ObjectIdentity {
        device: namespace.identity().device,
        inode: namespace.identity().inode,
    };
    if namespace_identity.device != anchor.identity().device
        || directory_descriptor_identity(namespace.anchor_fd())
            .map_err(|_| ApiListenerPublicationError::Invalid)?
            != namespace_identity
        || directory_descriptor_identity(anchor.descriptor())
            .map_err(|_| ApiListenerPublicationError::Invalid)?
            != anchor.identity()
    {
        return Err(ApiListenerPublicationError::Invalid);
    }
    // SAFETY: Identity calls have no pointer or ownership contract.
    let expected_owner = unsafe { (libc::geteuid(), libc::getegid()) };
    let staging = socket_staging_name(ResourceRole::ApiSocketDirectory)
        .map_err(|_| ApiListenerPublicationError::Invalid)?;
    let record_child = child.clone();
    let child = CString::new(child.as_bytes()).map_err(|_| ApiListenerPublicationError::Invalid)?;
    ensure_absent(namespace.anchor_fd(), staging)?;
    ensure_absent(anchor.descriptor(), &child)?;

    let staged_identity = Cell::new(None);
    let staged = with_scoped_cwd(namespace.anchor_fd(), namespace_identity, || {
        let result = bind_staging(staging, expected_owner);
        if let Ok((_, identity)) = &result {
            staged_identity.set(Some(*identity));
        }
        result
    });
    let (listener, identity) = match staged {
        Ok(staged) => staged,
        Err(ScopedCwdOperationError::Operation(error)) => return Err(error),
        Err(ScopedCwdOperationError::Boundary(_)) => {
            if let Some(identity) = staged_identity.get() {
                cleanup_unrecorded(
                    namespace.anchor_fd(),
                    staging,
                    Some(identity),
                    expected_owner,
                );
            }
            return Err(ApiListenerPublicationError::Cwd);
        }
    };
    let mut staging_guard = StagingGuard {
        directory: namespace.anchor_fd(),
        name: staging,
        identity,
        expected_owner,
        armed: true,
    };
    let record =
        SocketOwnershipRecord::new(ResourceRole::ApiSocketDirectory, record_child, identity)
            .map_err(|_| ApiListenerPublicationError::Invalid)?;
    namespace
        .write_socket_record(&record)
        .map_err(|_| ApiListenerPublicationError::Record)?;
    // The durable record now owns rollback. A local or transport failure must
    // leave it for the worker or post-reap launcher recovery.
    staging_guard.armed = false;

    // SAFETY: Both validated directory anchors and bounded C strings remain live.
    if unsafe {
        libc::renameatx_np(
            namespace.anchor_fd(),
            staging.as_ptr(),
            anchor.descriptor(),
            child.as_ptr(),
            libc::RENAME_EXCL,
        )
    } != 0
    {
        let error = io::Error::last_os_error();
        return if matches!(
            error.kind(),
            io::ErrorKind::AlreadyExists | io::ErrorKind::AddrInUse
        ) {
            Err(ApiListenerPublicationError::PathExists)
        } else {
            Err(ApiListenerPublicationError::Io(error.kind()))
        };
    }

    if socket_identity_at(anchor.descriptor(), &child, expected_owner, Some(0o600))? != identity {
        return Err(ApiListenerPublicationError::PathChanged);
    }
    validate_listener_descriptor(listener.as_raw_fd(), staging)?;
    namespace
        .require_socket_record(&record)
        .map_err(|_| ApiListenerPublicationError::Record)?;
    Ok(ApiListenerPublication {
        listener: Some(listener),
        record,
    })
}

fn bind_staging(
    staging: &CStr,
    expected_owner: (u32, u32),
) -> Result<(UnixListener, ObjectIdentity), ApiListenerPublicationError> {
    ensure_absent(libc::AT_FDCWD, staging)?;
    // SAFETY: A successful descriptor is immediately wrapped for unique ownership.
    let descriptor = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if descriptor < 0 {
        return Err(ApiListenerPublicationError::Io(
            io::Error::last_os_error().kind(),
        ));
    }
    // SAFETY: The fresh descriptor has no other owner.
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    set_cloexec(descriptor.as_raw_fd())
        .map_err(|error| ApiListenerPublicationError::Io(error.kind()))?;
    set_nonblocking(descriptor.as_raw_fd())?;
    let (address, address_length) = relative_address(staging)?;
    // SAFETY: The descriptor and initialized bounded address remain live.
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
            Err(ApiListenerPublicationError::PathExists)
        } else {
            Err(ApiListenerPublicationError::Io(error.kind()))
        };
    }
    let identity = match socket_identity_at(libc::AT_FDCWD, staging, expected_owner, None) {
        Ok(identity) => identity,
        Err(error) => {
            cleanup_unrecorded(libc::AT_FDCWD, staging, None, expected_owner);
            return Err(error);
        }
    };
    let mut guard = StagingGuard {
        directory: libc::AT_FDCWD,
        name: staging,
        identity,
        expected_owner,
        armed: true,
    };
    // SAFETY: The live relative path names the exact captured socket.
    if unsafe { libc::chmod(staging.as_ptr(), 0o600) } != 0 {
        return Err(ApiListenerPublicationError::Io(
            io::Error::last_os_error().kind(),
        ));
    }
    if socket_identity_at(libc::AT_FDCWD, staging, expected_owner, Some(0o600))? != identity {
        return Err(ApiListenerPublicationError::PathChanged);
    }
    // SAFETY: The live stream socket is ready to become a listener.
    if unsafe { libc::listen(descriptor.as_raw_fd(), LISTEN_BACKLOG) } != 0 {
        return Err(ApiListenerPublicationError::Io(
            io::Error::last_os_error().kind(),
        ));
    }
    validate_listener_descriptor(descriptor.as_raw_fd(), staging)?;
    guard.armed = false;
    Ok((UnixListener::from(descriptor), identity))
}

struct StagingGuard<'a> {
    directory: RawFd,
    name: &'a CStr,
    identity: ObjectIdentity,
    expected_owner: (u32, u32),
    armed: bool,
}

impl Drop for StagingGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            cleanup_unrecorded(
                self.directory,
                self.name,
                Some(self.identity),
                self.expected_owner,
            );
        }
    }
}

fn cleanup_unrecorded(
    directory: RawFd,
    name: &CStr,
    expected_identity: Option<ObjectIdentity>,
    expected_owner: (u32, u32),
) {
    let Ok(identity) = socket_identity_at(directory, name, expected_owner, None) else {
        return;
    };
    if expected_identity.is_some_and(|expected| expected != identity) {
        return;
    }
    // SAFETY: The retained anchor and bounded name identify the validated socket.
    let _ = unsafe { libc::unlinkat(directory, name.as_ptr(), 0) };
}

fn ensure_absent(directory: RawFd, name: &CStr) -> Result<(), ApiListenerPublicationError> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: The directory, bounded name, and writable stat storage remain live.
    if unsafe {
        libc::fstatat(
            directory,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == 0
    {
        return Err(ApiListenerPublicationError::PathExists);
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::NotFound {
        Ok(())
    } else {
        Err(ApiListenerPublicationError::Io(error.kind()))
    }
}

fn socket_identity_at(
    directory: RawFd,
    name: &CStr,
    expected_owner: (u32, u32),
    expected_mode: Option<u32>,
) -> Result<ObjectIdentity, ApiListenerPublicationError> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: The retained directory, name, and writable stat storage remain live.
    if unsafe {
        libc::fstatat(
            directory,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(ApiListenerPublicationError::Io(
            io::Error::last_os_error().kind(),
        ));
    }
    // SAFETY: Successful fstatat initialized the complete structure.
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFSOCK
        || stat.st_uid != expected_owner.0
        || stat.st_gid != expected_owner.1
        || stat.st_nlink != 1
        || expected_mode.is_some_and(|mode| u32::from(stat.st_mode & 0o7777) != mode)
    {
        return Err(ApiListenerPublicationError::PathChanged);
    }
    let identity = stat_identity(&stat);
    if identity.device == 0 || identity.inode == 0 {
        return Err(ApiListenerPublicationError::Invalid);
    }
    Ok(identity)
}

fn relative_address(
    name: &CStr,
) -> Result<(libc::sockaddr_un, libc::socklen_t), ApiListenerPublicationError> {
    let bytes = name.to_bytes_with_nul();
    let address = MaybeUninit::<libc::sockaddr_un>::zeroed();
    // SAFETY: Zeroed sockaddr_un is valid before initialization below.
    let mut address = unsafe { address.assume_init() };
    if bytes.len() > address.sun_path.len() {
        return Err(ApiListenerPublicationError::Invalid);
    }
    address.sun_family = libc::sa_family_t::try_from(libc::AF_UNIX)
        .map_err(|_| ApiListenerPublicationError::Invalid)?;
    address.sun_len = u8::try_from(
        std::mem::offset_of!(libc::sockaddr_un, sun_path)
            .checked_add(bytes.len())
            .ok_or(ApiListenerPublicationError::Invalid)?,
    )
    .map_err(|_| ApiListenerPublicationError::Invalid)?;
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

fn set_nonblocking(descriptor: RawFd) -> Result<(), ApiListenerPublicationError> {
    // SAFETY: F_GETFL inspects the live descriptor.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0
        // SAFETY: F_SETFL changes only status flags on the same live descriptor.
        || unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(ApiListenerPublicationError::Io(
            io::Error::last_os_error().kind(),
        ));
    }
    Ok(())
}

fn validate_listener_descriptor(
    descriptor: RawFd,
    expected_name: &CStr,
) -> Result<(), ApiListenerPublicationError> {
    // SAFETY: F_GETFD/F_GETFL inspect the live descriptor.
    let descriptor_flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    // SAFETY: F_GETFL inspects the same live descriptor.
    let status_flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if descriptor_flags < 0
        || status_flags < 0
        || descriptor_flags & libc::FD_CLOEXEC == 0
        || status_flags & libc::O_NONBLOCK == 0
        || socket_int_option(descriptor, libc::SO_TYPE)? != libc::SOCK_STREAM
        || socket_int_option(descriptor, libc::SO_ERROR)? != 0
    {
        return Err(ApiListenerPublicationError::Invalid);
    }
    let mut address = MaybeUninit::<libc::sockaddr_un>::zeroed();
    let mut length = libc::socklen_t::try_from(size_of::<libc::sockaddr_un>())
        .map_err(|_| ApiListenerPublicationError::Invalid)?;
    // SAFETY: Address storage and length are writable for the live listener.
    if unsafe { libc::getsockname(descriptor, address.as_mut_ptr().cast(), &raw mut length) } != 0 {
        return Err(ApiListenerPublicationError::Io(
            io::Error::last_os_error().kind(),
        ));
    }
    // SAFETY: Successful getsockname initialized the returned address prefix.
    let address = unsafe { address.assume_init() };
    let returned = usize::try_from(length).map_err(|_| ApiListenerPublicationError::Invalid)?;
    let path_length = returned
        .checked_sub(std::mem::offset_of!(libc::sockaddr_un, sun_path))
        .ok_or(ApiListenerPublicationError::Invalid)?;
    if address.sun_family
        != libc::sa_family_t::try_from(libc::AF_UNIX)
            .map_err(|_| ApiListenerPublicationError::Invalid)?
        || path_length != expected_name.to_bytes_with_nul().len()
        || path_length > address.sun_path.len()
    {
        return Err(ApiListenerPublicationError::Invalid);
    }
    // SAFETY: Kernel-returned length bounds the path read within address.
    let path =
        unsafe { std::slice::from_raw_parts(address.sun_path.as_ptr().cast::<u8>(), path_length) };
    if path != expected_name.to_bytes_with_nul() {
        return Err(ApiListenerPublicationError::Invalid);
    }
    // SAFETY: Null address arguments probe for an already queued client only.
    let accepted = unsafe { libc::accept(descriptor, std::ptr::null_mut(), std::ptr::null_mut()) };
    if accepted >= 0 {
        // SAFETY: A successful accept returns a uniquely owned descriptor.
        drop(unsafe { OwnedFd::from_raw_fd(accepted) });
        return Err(ApiListenerPublicationError::Invalid);
    }
    if io::Error::last_os_error().kind() != io::ErrorKind::WouldBlock {
        return Err(ApiListenerPublicationError::Invalid);
    }
    Ok(())
}

fn socket_int_option(
    descriptor: RawFd,
    option: libc::c_int,
) -> Result<libc::c_int, ApiListenerPublicationError> {
    let mut value = 0;
    let mut length = libc::socklen_t::try_from(size_of::<libc::c_int>())
        .map_err(|_| ApiListenerPublicationError::Invalid)?;
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
        return Err(ApiListenerPublicationError::Io(
            io::Error::last_os_error().kind(),
        ));
    }
    if usize::try_from(length).ok() != Some(size_of::<libc::c_int>()) {
        return Err(ApiListenerPublicationError::Invalid);
    }
    Ok(value)
}

fn stat_identity(stat: &libc::stat) -> ObjectIdentity {
    ObjectIdentity {
        device: u64::from(u32::from_ne_bytes(stat.st_dev.to_ne_bytes())),
        inode: stat.st_ino,
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::os::unix::fs::{FileTypeExt, PermissionsExt, symlink};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDirectories {
        root: PathBuf,
        namespace: PathBuf,
        external: PathBuf,
    }

    impl TestDirectories {
        fn create() -> Self {
            let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "bangbang-api-publication-{}-{id}",
                std::process::id()
            ));
            let namespace = root.join("namespace");
            let external = root.join("external");
            fs::create_dir(&root).expect("test root should create");
            fs::create_dir(&namespace).expect("namespace should create");
            fs::create_dir(&external).expect("external directory should create");
            for directory in [&namespace, &external] {
                fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
                    .expect("directory mode should set");
            }
            Self {
                root,
                namespace,
                external,
            }
        }

        fn namespace(&self) -> WorkerSocketNamespace {
            WorkerSocketNamespace::from_directory_for_test(&self.namespace)
                .expect("test namespace should validate")
        }

        fn anchor(&self) -> (File, SocketDirectoryAnchor) {
            let directory = File::open(&self.external).expect("external directory should open");
            let descriptor = directory.as_raw_fd();
            let identity = directory_descriptor_identity(descriptor)
                .expect("external identity should inspect");
            (
                directory,
                SocketDirectoryAnchor::for_test(descriptor, identity),
            )
        }

        fn final_path(&self, child: &str) -> PathBuf {
            self.external.join(child)
        }
    }

    impl Drop for TestDirectories {
        fn drop(&mut self) {
            for directory in [&self.namespace, &self.external] {
                if let Ok(entries) = fs::read_dir(directory) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.is_dir() {
                            let _ = fs::remove_dir(&path);
                        } else {
                            let _ = fs::remove_file(&path);
                        }
                    }
                }
                let _ = fs::remove_dir(directory);
            }
            let _ = fs::remove_dir(&self.root);
        }
    }

    fn child(value: &str) -> SocketChild {
        SocketChild::parse(value).expect("test child should parse")
    }

    fn cleanup_recorded(
        namespace: &WorkerSocketNamespace,
        directory: &TestDirectories,
        record: &SocketOwnershipRecord,
    ) {
        let final_path = directory.final_path(
            std::str::from_utf8(record.child().as_bytes()).expect("child should be UTF-8"),
        );
        if final_path.exists() {
            fs::remove_file(&final_path).expect("recorded socket should remove");
        }
        namespace
            .unlink_staged_socket(record)
            .expect("staging cleanup should be idempotent");
        namespace
            .clear_socket_record(record)
            .expect("record should clear");
    }

    #[test]
    fn staged_listener_publishes_exact_record_and_survives_alias_release() {
        let directories = TestDirectories::create();
        let namespace = directories.namespace();
        let inspection = namespace.try_clone().expect("namespace should duplicate");
        let (_anchor_file, anchor) = directories.anchor();
        let mut publication = publish_api_listener(namespace, anchor, child("api.sock"))
            .expect("API listener should publish");
        let record = publication.record().clone();
        assert_eq!(
            inspection
                .socket_record(ResourceRole::ApiSocketDirectory)
                .expect("record should read"),
            Some(record.clone())
        );
        assert_eq!(record.identity(), publication.identity());
        assert!(directories.final_path("api.sock").exists());
        assert!(!directories.namespace.join(".api-socket.pending").exists());
        assert!(
            fs::symlink_metadata(directories.final_path("api.sock"))
                .expect("final socket should inspect")
                .file_type()
                .is_socket()
        );

        let client = UnixStream::connect(directories.final_path("api.sock"))
            .expect("published listener should accept connections");
        let accepted = publication
            .listener
            .as_ref()
            .expect("launcher alias should remain")
            .accept()
            .expect("queued connection should accept")
            .0;
        drop((client, accepted));
        publication
            .release_listener_alias()
            .expect("launcher alias should release once");
        assert!(directories.final_path("api.sock").exists());
        assert_eq!(
            publication.release_listener_alias(),
            Err(ApiListenerPublicationError::Invalid)
        );
        cleanup_recorded(&inspection, &directories, &record);
    }

    #[test]
    fn final_collisions_and_wrong_anchor_identity_are_preserved() {
        let directories = TestDirectories::create();
        let namespace = directories.namespace();
        let (_anchor_file, anchor) = directories.anchor();
        let final_path = directories.final_path("api.sock");
        File::create(&final_path).expect("collision file should create");
        assert!(matches!(
            publish_api_listener(namespace, anchor, child("api.sock")),
            Err(ApiListenerPublicationError::PathExists)
        ));
        assert!(final_path.is_file());
        assert!(!directories.namespace.join(".api-socket.pending").exists());

        fs::remove_file(&final_path).expect("collision file should remove");
        symlink("missing", &final_path).expect("collision symlink should create");
        let namespace = directories.namespace();
        let (_anchor_file, anchor) = directories.anchor();
        assert!(matches!(
            publish_api_listener(namespace, anchor, child("api.sock")),
            Err(ApiListenerPublicationError::PathExists)
        ));
        assert!(
            fs::symlink_metadata(&final_path)
                .expect("collision symlink should inspect")
                .file_type()
                .is_symlink()
        );

        fs::remove_file(&final_path).expect("collision symlink should remove");
        let namespace = directories.namespace();
        let (_anchor_file, anchor) = directories.anchor();
        let wrong = SocketDirectoryAnchor::for_test(
            anchor.descriptor(),
            ObjectIdentity {
                device: anchor.identity().device,
                inode: anchor.identity().inode.saturating_add(1),
            },
        );
        assert_eq!(
            publish_api_listener(namespace, wrong, child("api.sock"))
                .expect_err("wrong anchor identity should fail"),
            ApiListenerPublicationError::Invalid
        );
        assert!(!final_path.exists());
    }

    #[test]
    fn record_collision_cleans_only_unrecorded_staging() {
        let directories = TestDirectories::create();
        let namespace = directories.namespace();
        let inspection = namespace.try_clone().expect("namespace should duplicate");
        let existing = SocketOwnershipRecord::new(
            ResourceRole::ApiSocketDirectory,
            child("existing.sock"),
            ObjectIdentity {
                device: 7,
                inode: 11,
            },
        )
        .expect("fixture record should validate");
        namespace
            .write_socket_record(&existing)
            .expect("fixture record should write");
        let (_anchor_file, anchor) = directories.anchor();
        assert_eq!(
            publish_api_listener(namespace, anchor, child("api.sock"))
                .expect_err("record collision should fail"),
            ApiListenerPublicationError::Record
        );
        assert!(!directories.namespace.join(".api-socket.pending").exists());
        assert!(!directories.final_path("api.sock").exists());
        assert_eq!(
            inspection
                .socket_record(ResourceRole::ApiSocketDirectory)
                .expect("existing record should read"),
            Some(existing.clone())
        );
        inspection
            .clear_socket_record(&existing)
            .expect("existing record should clear");
    }

    #[test]
    fn debug_and_errors_do_not_expose_names_or_identities() {
        let error = ApiListenerPublicationError::PathChanged;
        assert_eq!(error.category(), io::ErrorKind::Other);
        assert!(!format!("{error:?}").contains("api.sock"));
        let directories = TestDirectories::create();
        let namespace = directories.namespace();
        let inspection = namespace.try_clone().expect("namespace should duplicate");
        let (_anchor_file, anchor) = directories.anchor();
        let publication = publish_api_listener(namespace, anchor, child("sensitive.sock"))
            .expect("API listener should publish");
        let record = publication.record().clone();
        let debug = format!("{publication:?}");
        assert!(!debug.contains("sensitive.sock"));
        assert!(!debug.contains(&record.identity().inode.to_string()));
        drop(publication);
        cleanup_recorded(&inspection, &directories, &record);
    }

    #[test]
    fn child_validation_remains_owned_by_the_closed_socket_child_type() {
        for invalid in ["", ".", "..", "nested/api.sock", "nul\0.sock"] {
            assert!(SocketChild::parse(invalid).is_err());
        }
        assert!(Path::new("api.sock").is_relative());
    }
}
