//! Direct-process destination resources for snapshot vsock reconstruction.

#[cfg(target_os = "macos")]
use std::ffi::CString;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
#[cfg(target_os = "macos")]
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::snapshot::SnapshotVsockSelectors;

use super::{
    SuppliedVsockListener, VirtioVsockReconstructionResource, VsockBackendSelector,
    VsockGuestConnector, guest_connection_socket_path, nonblocking_unix_stream_connect,
};

const DIRECT_SOCKET_MODE: u32 = 0o600;
const TEMP_BIND_ATTEMPTS: usize = 16;
static NEXT_TEMP_SOCKET_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SocketIdentity {
    device: u64,
    inode: u64,
}

impl SocketIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

/// Value-redacted failure while preparing a direct restore socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectVsockRestoreError {
    PathCheck(io::ErrorKind),
    UnrelatedEntry,
    ActiveSocket,
    Bind(io::ErrorKind),
    Metadata(io::ErrorKind),
    Permissions(io::ErrorKind),
    Publish(io::ErrorKind),
    PathChanged,
    StaleReplacementUnsupported,
    Cleanup(DirectVsockRestoreCleanupError),
}

impl fmt::Display for DirectVsockRestoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::PathCheck(_) => "failed to inspect direct vsock destination",
            Self::UnrelatedEntry => "direct vsock destination is not a socket",
            Self::ActiveSocket => "direct vsock destination is active or indeterminate",
            Self::Bind(_) => "failed to bind a provisional direct vsock socket",
            Self::Metadata(_) => "failed to inspect a provisional direct vsock socket",
            Self::Permissions(_) => "failed to restrict a provisional direct vsock socket",
            Self::Publish(_) => "failed to publish a direct vsock socket",
            Self::PathChanged => "direct vsock destination changed during preparation",
            Self::StaleReplacementUnsupported => {
                "atomic stale direct vsock replacement is unsupported on this platform"
            }
            Self::Cleanup(_) => "failed to clean a provisional direct vsock socket",
        })
    }
}

impl std::error::Error for DirectVsockRestoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Cleanup(source) => Some(source),
            Self::PathCheck(_)
            | Self::UnrelatedEntry
            | Self::ActiveSocket
            | Self::Bind(_)
            | Self::Metadata(_)
            | Self::Permissions(_)
            | Self::Publish(_)
            | Self::PathChanged
            | Self::StaleReplacementUnsupported => None,
        }
    }
}

/// Failure to remove a still-owned direct restore socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectVsockRestoreCleanupError {
    Inspect(io::ErrorKind),
    Remove(io::ErrorKind),
}

impl fmt::Display for DirectVsockRestoreCleanupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("failed to clean an owned direct vsock socket")
    }
}

impl std::error::Error for DirectVsockRestoreCleanupError {}

/// Exact cleanup authority for one published direct restore socket.
pub struct DirectVsockSocketGuard {
    path: PathBuf,
    identity: Option<SocketIdentity>,
}

impl DirectVsockSocketGuard {
    /// Removes the destination only if it still names this socket.
    pub fn cleanup(mut self) -> Result<(), DirectVsockRestoreCleanupError> {
        self.cleanup_inner()
    }

    fn cleanup_inner(&mut self) -> Result<(), DirectVsockRestoreCleanupError> {
        let Some(identity) = self.identity else {
            return Ok(());
        };
        match socket_identity(&self.path) {
            Ok(Some(current)) if current == identity => {
                fs::remove_file(&self.path)
                    .map_err(|error| DirectVsockRestoreCleanupError::Remove(error.kind()))?;
                self.identity = None;
                Ok(())
            }
            Ok(_) => {
                // The owned name is already absent or has been replaced. Never
                // remove the replacement.
                self.identity = None;
                Ok(())
            }
            Err(error) => Err(DirectVsockRestoreCleanupError::Inspect(error.kind())),
        }
    }
}

impl fmt::Debug for DirectVsockSocketGuard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DirectVsockSocketGuard")
            .field("path", &"<redacted>")
            .field("armed", &self.identity.is_some())
            .finish()
    }
}

impl Drop for DirectVsockSocketGuard {
    fn drop(&mut self) {
        let _ = self.cleanup_inner();
    }
}

/// One direct listener/connector resource and its independent cleanup owner.
pub struct PreparedDirectVsockRestore {
    resource: VirtioVsockReconstructionResource,
    guard: DirectVsockSocketGuard,
}

impl PreparedDirectVsockRestore {
    /// Borrows the single-use reconstruction resource.
    pub const fn resource_mut(&mut self) -> &mut VirtioVsockReconstructionResource {
        &mut self.resource
    }

    /// Splits the resource from the cleanup authority for process ownership.
    pub fn into_parts(self) -> (VirtioVsockReconstructionResource, DirectVsockSocketGuard) {
        (self.resource, self.guard)
    }

    /// Aborts preparation and verifies cleanup of the owned destination name.
    pub fn abort(self) -> Result<(), DirectVsockRestoreCleanupError> {
        let Self { resource, guard } = self;
        drop(resource);
        guard.cleanup()
    }
}

impl fmt::Debug for PreparedDirectVsockRestore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedDirectVsockRestore")
            .field("resource", &"<owned>")
            .field("guard", &self.guard)
            .finish()
    }
}

#[derive(Debug)]
struct DirectVsockGuestConnector {
    selector: VsockBackendSelector,
}

impl VsockGuestConnector for DirectVsockGuestConnector {
    fn connect(&mut self, host_port: u32) -> io::Result<UnixStream> {
        nonblocking_unix_stream_connect(&guest_connection_socket_path(
            self.selector.path(),
            host_port,
        ))
    }
}

/// Prepares an owned direct listener and guest connector for one reconstruction.
pub fn prepare_direct_vsock_restore(
    selectors: SnapshotVsockSelectors,
) -> Result<PreparedDirectVsockRestore, DirectVsockRestoreError> {
    prepare_direct_vsock_restore_with(selectors, || {})
}

fn prepare_direct_vsock_restore_with(
    selectors: SnapshotVsockSelectors,
    before_stale_exchange: impl FnOnce(),
) -> Result<PreparedDirectVsockRestore, DirectVsockRestoreError> {
    let (captured_selector, destination_selector) = selectors.into_parts();
    let destination = destination_selector.path().to_path_buf();
    let existing = destination_entry(&destination)?;
    if let Some(existing) = existing {
        match nonblocking_unix_stream_connect(&destination) {
            Ok(stream) => {
                drop(stream);
                return Err(DirectVsockRestoreError::ActiveSocket);
            }
            Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {}
            Err(_) => return Err(DirectVsockRestoreError::ActiveSocket),
        }
        if socket_identity(&destination)
            .map_err(|error| DirectVsockRestoreError::PathCheck(error.kind()))?
            != Some(existing)
        {
            return Err(DirectVsockRestoreError::PathChanged);
        }
    }

    let (listener, temporary, new_identity) = bind_temporary_socket(&destination)?;
    let publication = match existing {
        None => publish_absent(&temporary, &destination, new_identity),
        Some(stale_identity) => {
            before_stale_exchange();
            publish_over_stale(&temporary, &destination, new_identity, stale_identity)
        }
    };
    if let Err(error) = publication {
        cleanup_owned_path(&temporary, new_identity).map_err(DirectVsockRestoreError::Cleanup)?;
        cleanup_owned_path(&destination, new_identity).map_err(DirectVsockRestoreError::Cleanup)?;
        return Err(error);
    }

    let connector = DirectVsockGuestConnector {
        selector: destination_selector.clone(),
    };
    let supplied = SuppliedVsockListener::new(listener).with_guest_connector(connector);
    Ok(PreparedDirectVsockRestore {
        resource: VirtioVsockReconstructionResource::with_destination_selector(
            captured_selector,
            destination_selector,
            supplied,
        ),
        guard: DirectVsockSocketGuard {
            path: destination,
            identity: Some(new_identity),
        },
    })
}

fn destination_entry(path: &Path) -> Result<Option<SocketIdentity>, DirectVsockRestoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => {
            Ok(Some(SocketIdentity::from_metadata(&metadata)))
        }
        Ok(_) => Err(DirectVsockRestoreError::UnrelatedEntry),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(DirectVsockRestoreError::PathCheck(error.kind())),
    }
}

fn bind_temporary_socket(
    destination: &Path,
) -> Result<(UnixListener, PathBuf, SocketIdentity), DirectVsockRestoreError> {
    for _ in 0..TEMP_BIND_ATTEMPTS {
        let temporary = next_temporary_socket_path(destination);
        let listener = match UnixListener::bind(&temporary) {
            Ok(listener) => listener,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::AlreadyExists | io::ErrorKind::AddrInUse
                ) =>
            {
                continue;
            }
            Err(error) => return Err(DirectVsockRestoreError::Bind(error.kind())),
        };
        let identity = match required_socket_identity(&temporary) {
            Ok(identity) => identity,
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                return Err(error);
            }
        };
        if let Err(error) =
            fs::set_permissions(&temporary, fs::Permissions::from_mode(DIRECT_SOCKET_MODE))
        {
            cleanup_owned_path(&temporary, identity).map_err(DirectVsockRestoreError::Cleanup)?;
            return Err(DirectVsockRestoreError::Permissions(error.kind()));
        }
        if required_socket_identity(&temporary)? != identity {
            cleanup_owned_path(&temporary, identity).map_err(DirectVsockRestoreError::Cleanup)?;
            return Err(DirectVsockRestoreError::PathChanged);
        }
        if let Err(error) = listener.set_nonblocking(true) {
            cleanup_owned_path(&temporary, identity).map_err(DirectVsockRestoreError::Cleanup)?;
            return Err(DirectVsockRestoreError::Bind(error.kind()));
        }
        return Ok((listener, temporary, identity));
    }

    Err(DirectVsockRestoreError::Bind(io::ErrorKind::AlreadyExists))
}

fn next_temporary_socket_path(destination: &Path) -> PathBuf {
    let id = NEXT_TEMP_SOCKET_ID.fetch_add(1, Ordering::Relaxed);
    let mut name = OsString::from(".bb-vsr-");
    name.push(format!("{}-{id}", std::process::id()));
    destination.with_file_name(name)
}

fn required_socket_identity(path: &Path) -> Result<SocketIdentity, DirectVsockRestoreError> {
    socket_identity(path)
        .map_err(|error| DirectVsockRestoreError::Metadata(error.kind()))?
        .ok_or(DirectVsockRestoreError::PathChanged)
}

fn socket_identity(path: &Path) -> io::Result<Option<SocketIdentity>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => {
            Ok(Some(SocketIdentity::from_metadata(&metadata)))
        }
        Ok(_) => Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(target_os = "macos")]
fn path_identity(path: &Path) -> io::Result<Option<SocketIdentity>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(SocketIdentity::from_metadata(&metadata))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn cleanup_owned_path(
    path: &Path,
    identity: SocketIdentity,
) -> Result<(), DirectVsockRestoreCleanupError> {
    match socket_identity(path) {
        Ok(Some(current)) if current == identity => fs::remove_file(path)
            .map_err(|error| DirectVsockRestoreCleanupError::Remove(error.kind())),
        Ok(_) => Ok(()),
        Err(error) => Err(DirectVsockRestoreCleanupError::Inspect(error.kind())),
    }
}

#[cfg(target_os = "macos")]
fn publish_absent(
    temporary: &Path,
    destination: &Path,
    new_identity: SocketIdentity,
) -> Result<(), DirectVsockRestoreError> {
    rename_with_flags(temporary, destination, libc::RENAME_EXCL).map_err(|error| {
        if matches!(
            error.kind(),
            io::ErrorKind::AlreadyExists | io::ErrorKind::AddrInUse
        ) {
            DirectVsockRestoreError::PathChanged
        } else {
            DirectVsockRestoreError::Publish(error.kind())
        }
    })?;
    if socket_identity(destination)
        .map_err(|error| DirectVsockRestoreError::PathCheck(error.kind()))?
        == Some(new_identity)
    {
        Ok(())
    } else {
        Err(DirectVsockRestoreError::PathChanged)
    }
}

#[cfg(not(target_os = "macos"))]
fn publish_absent(
    temporary: &Path,
    destination: &Path,
    new_identity: SocketIdentity,
) -> Result<(), DirectVsockRestoreError> {
    // A same-directory hard link provides an atomic no-clobber publication on
    // Unix targets without Darwin's `RENAME_EXCL`. The listener keeps owning
    // the inode after the provisional name is removed.
    fs::hard_link(temporary, destination).map_err(|error| {
        if matches!(
            error.kind(),
            io::ErrorKind::AlreadyExists | io::ErrorKind::AddrInUse
        ) {
            DirectVsockRestoreError::PathChanged
        } else {
            DirectVsockRestoreError::Publish(error.kind())
        }
    })?;
    if socket_identity(destination)
        .map_err(|error| DirectVsockRestoreError::PathCheck(error.kind()))?
        != Some(new_identity)
    {
        return Err(DirectVsockRestoreError::PathChanged);
    }
    fs::remove_file(temporary).map_err(|error| DirectVsockRestoreError::Publish(error.kind()))?;
    if socket_identity(destination)
        .map_err(|error| DirectVsockRestoreError::PathCheck(error.kind()))?
        != Some(new_identity)
    {
        return Err(DirectVsockRestoreError::PathChanged);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn publish_over_stale(
    temporary: &Path,
    destination: &Path,
    new_identity: SocketIdentity,
    stale_identity: SocketIdentity,
) -> Result<(), DirectVsockRestoreError> {
    rename_with_flags(temporary, destination, libc::RENAME_SWAP)
        .map_err(|error| DirectVsockRestoreError::Publish(error.kind()))?;

    let final_identity = socket_identity(destination)
        .map_err(|error| DirectVsockRestoreError::PathCheck(error.kind()))?;
    let displaced_identity = path_identity(temporary)
        .map_err(|error| DirectVsockRestoreError::PathCheck(error.kind()))?;
    if final_identity == Some(new_identity) && displaced_identity == Some(stale_identity) {
        cleanup_owned_path(temporary, stale_identity).map_err(DirectVsockRestoreError::Cleanup)?;
        return Ok(());
    }

    if final_identity == Some(new_identity) && displaced_identity.is_some() {
        // A replacement won the race after the stale probe. Swap it back and
        // remove only our provisional inode. The replacement can be any entry
        // type, not merely another socket.
        rename_with_flags(temporary, destination, libc::RENAME_SWAP)
            .map_err(|error| DirectVsockRestoreError::Publish(error.kind()))?;
    }
    cleanup_owned_path(temporary, new_identity).map_err(DirectVsockRestoreError::Cleanup)?;
    cleanup_owned_path(destination, new_identity).map_err(DirectVsockRestoreError::Cleanup)?;
    Err(DirectVsockRestoreError::PathChanged)
}

#[cfg(not(target_os = "macos"))]
fn publish_over_stale(
    _temporary: &Path,
    _destination: &Path,
    _new_identity: SocketIdentity,
    _stale_identity: SocketIdentity,
) -> Result<(), DirectVsockRestoreError> {
    Err(DirectVsockRestoreError::StaleReplacementUnsupported)
}

#[cfg(target_os = "macos")]
fn rename_with_flags(from: &Path, to: &Path, flags: libc::c_uint) -> io::Result<()> {
    let from = path_to_cstring(from)?;
    let to = path_to_cstring(to)?;
    // SAFETY: both pointers come from live NUL-terminated `CString` values and
    // remain valid for the synchronous Darwin rename call.
    let result = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), flags) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
fn path_to_cstring(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::snapshot::{SnapshotVsockOverride, resolve_snapshot_vsock_selectors};

    use super::*;

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    fn test_path(name: &str) -> PathBuf {
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        PathBuf::from("/tmp").join(format!("bb-vsr-{name}-{}-{id}.sock", std::process::id()))
    }

    fn selectors(captured: &Path, destination: &Path) -> SnapshotVsockSelectors {
        let captured = VsockBackendSelector::try_from_path(captured)
            .expect("captured selector should validate");
        resolve_snapshot_vsock_selectors(
            Some(&captured),
            Some(&SnapshotVsockOverride::new(destination)),
        )
        .expect("selector resolution should succeed")
        .expect("captured device should produce selectors")
    }

    #[test]
    fn direct_restore_prepares_owner_only_listener_and_exact_cleanup() {
        let captured = test_path("captured");
        let destination = test_path("destination");
        let prepared = prepare_direct_vsock_restore(selectors(&captured, &destination))
            .expect("absent destination should prepare");
        let metadata = fs::symlink_metadata(&destination).expect("destination should exist");
        assert!(metadata.file_type().is_socket());
        assert_eq!(metadata.permissions().mode() & 0o777, DIRECT_SOCKET_MODE);
        assert!(!format!("{prepared:?}").contains(destination.to_string_lossy().as_ref()));

        prepared.abort().expect("owned socket should clean");
        assert!(!destination.exists());
    }

    #[test]
    fn direct_restore_isolates_concurrent_destinations_and_reuses_cleaned_name() {
        let captured = test_path("captured-isolation");
        let first = test_path("first-destination");
        let second = test_path("second-destination");
        let first_prepared = prepare_direct_vsock_restore(selectors(&captured, &first))
            .expect("first destination should prepare");
        let second_prepared = prepare_direct_vsock_restore(selectors(&captured, &second))
            .expect("second destination should prepare independently");
        assert!(UnixStream::connect(&first).is_ok());
        assert!(UnixStream::connect(&second).is_ok());

        first_prepared.abort().expect("first socket should clean");
        assert!(!first.exists());
        assert!(second.exists());
        let reused = prepare_direct_vsock_restore(selectors(&captured, &first))
            .expect("cleaned destination should be deterministically reusable");
        assert!(UnixStream::connect(&first).is_ok());

        reused.abort().expect("reused socket should clean");
        second_prepared.abort().expect("second socket should clean");
        assert!(!first.exists());
        assert!(!second.exists());
    }

    #[test]
    fn direct_restore_preserves_live_and_unrelated_destinations() {
        let captured = test_path("captured-preserve");
        let live_path = test_path("live");
        let live = UnixListener::bind(&live_path).expect("live socket should bind");
        assert!(matches!(
            prepare_direct_vsock_restore(selectors(&captured, &live_path)),
            Err(DirectVsockRestoreError::ActiveSocket)
        ));
        assert!(live_path.exists());
        drop(live);
        fs::remove_file(&live_path).expect("test socket should clean");

        let file_path = test_path("file");
        fs::write(&file_path, b"preserve").expect("test file should write");
        assert!(matches!(
            prepare_direct_vsock_restore(selectors(&captured, &file_path)),
            Err(DirectVsockRestoreError::UnrelatedEntry)
        ));
        assert_eq!(fs::read(&file_path).unwrap(), b"preserve");
        fs::remove_file(&file_path).expect("test file should clean");

        let symlink_path = test_path("symlink");
        symlink("missing-private-target", &symlink_path).expect("test symlink should create");
        assert!(matches!(
            prepare_direct_vsock_restore(selectors(&captured, &symlink_path)),
            Err(DirectVsockRestoreError::UnrelatedEntry)
        ));
        assert!(
            fs::symlink_metadata(&symlink_path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        fs::remove_file(&symlink_path).expect("test symlink should clean");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn direct_restore_replaces_stale_socket_and_preserves_later_replacement() {
        let captured = test_path("captured-stale");
        let destination = test_path("stale");
        let stale = UnixListener::bind(&destination).expect("stale socket should bind");
        let stale_identity = SocketIdentity::from_metadata(
            &fs::symlink_metadata(&destination).expect("stale socket should exist"),
        );
        drop(stale);

        let prepared = prepare_direct_vsock_restore(selectors(&captured, &destination))
            .expect("stale socket should be replaced");
        let current = SocketIdentity::from_metadata(
            &fs::symlink_metadata(&destination).expect("new socket should exist"),
        );
        assert_ne!(current, stale_identity);

        fs::remove_file(&destination).expect("owned path should unlink for replacement test");
        let replacement = UnixListener::bind(&destination).expect("replacement should bind");
        let replacement_inode = fs::symlink_metadata(&destination).unwrap().ino();
        drop(prepared);
        assert_eq!(
            fs::symlink_metadata(&destination).unwrap().ino(),
            replacement_inode
        );
        drop(replacement);
        fs::remove_file(&destination).expect("replacement should clean");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn direct_restore_rolls_back_replacement_that_wins_stale_probe_race() {
        let captured = test_path("captured-race");
        let destination = test_path("race");
        let stale = UnixListener::bind(&destination).expect("stale socket should bind");
        drop(stale);

        let mut replacement = None;
        let error = prepare_direct_vsock_restore_with(selectors(&captured, &destination), || {
            fs::remove_file(&destination).expect("stale path should unlink");
            replacement =
                Some(UnixListener::bind(&destination).expect("replacement socket should bind"));
        })
        .expect_err("replacement race should not be clobbered");
        assert_eq!(error, DirectVsockRestoreError::PathChanged);
        let replacement = replacement.expect("replacement listener should remain owned");
        assert!(UnixStream::connect(&destination).is_ok());
        drop(replacement);
        fs::remove_file(&destination).expect("replacement should clean");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn direct_restore_rolls_back_unrelated_entry_that_wins_stale_probe_race() {
        let captured = test_path("captured-unrelated-race");
        let destination = test_path("unrelated-race");
        let stale = UnixListener::bind(&destination).expect("stale socket should bind");
        drop(stale);

        let error = prepare_direct_vsock_restore_with(selectors(&captured, &destination), || {
            fs::remove_file(&destination).expect("stale path should unlink");
            fs::write(&destination, b"preserve replacement")
                .expect("replacement file should write");
        })
        .expect_err("unrelated replacement race should not be clobbered");
        assert_eq!(error, DirectVsockRestoreError::PathChanged);
        assert_eq!(
            fs::read(&destination).expect("replacement should remain at destination"),
            b"preserve replacement"
        );
        fs::remove_file(&destination).expect("replacement should clean");
    }

    #[test]
    fn direct_restore_diagnostics_are_value_redacted() {
        let secret = test_path("private-secret");
        fs::write(&secret, b"private").expect("test entry should write");
        let error = prepare_direct_vsock_restore(selectors(&test_path("captured"), &secret))
            .expect_err("unrelated entry should fail");
        let diagnostic = format!("{error:?} {error}");
        assert!(!diagnostic.contains(secret.to_string_lossy().as_ref()));
        fs::remove_file(secret).expect("test entry should clean");
    }
}
