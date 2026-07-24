//! Failure-atomic worker grant staging and one-time typed adoption.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::OpenOptions;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::path::Path;

use crate::macos::bookmark::{BookmarkError, ScopedBookmark};
use crate::macos::grant_transport::ReceivedGrant;
use crate::macos::{normalized_block_status_flags, peer_identity};
use crate::{
    BatchId, BlockDeviceGrant, ConnectedUnixPeer, GrantAccess, GrantId, GrantObjectKind,
    GrantRecord, MAX_BATCH_BOOKMARK_BYTES, MAX_BOOKMARK_BYTES, MAX_GRANT_RECORDS, MAX_GRANTS,
    ObjectIdentity, ResourceRole, SessionId,
};

/// Redacted staging or adoption failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrantRegistryError;

impl fmt::Display for GrantRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("private grant registry failure")
    }
}

impl std::error::Error for GrantRegistryError {}

impl From<BookmarkError> for GrantRegistryError {
    fn from(_: BookmarkError) -> Self {
        Self
    }
}

/// Atomically committed grant batch and acknowledgment values.
#[derive(Debug)]
pub struct CommittedGrantBatch {
    /// Session-owned registry.
    pub registry: GrantRegistry,
    /// Exact redacted batch identity.
    pub batch: BatchId,
    /// Number of semantic grants.
    pub grant_count: u16,
    /// Final launcher record sequence.
    pub final_sequence: u64,
}

/// One-time session-owned resource registry.
#[derive(Default)]
pub struct GrantRegistry {
    files: HashMap<GrantId, GrantedFile>,
    directories: HashMap<GrantId, GrantedDirectory>,
    streams: HashMap<GrantId, GrantedUnixStream>,
}

impl fmt::Debug for GrantRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrantRegistry")
            .field("entries", &"<redacted>")
            .finish()
    }
}

impl GrantRegistry {
    /// Returns the number of unadopted grants.
    #[must_use]
    pub fn len(&self) -> usize {
        self.files.len() + self.directories.len() + self.streams.len()
    }

    /// Returns whether no unadopted authority remains.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.directories.is_empty() && self.streams.is_empty()
    }

    /// Adopts one exact existing-file descriptor once.
    pub fn take_file(
        &mut self,
        id: &GrantId,
        role: ResourceRole,
        access: GrantAccess,
    ) -> Result<GrantedFile, GrantRegistryError> {
        take_file(&mut self.files, id, role, access)
    }

    /// Adopts one exact regular or block-special drive backing once.
    pub fn take_drive_backing(
        &mut self,
        id: &GrantId,
        access: GrantAccess,
    ) -> Result<GrantedFile, GrantRegistryError> {
        take_drive_backing(&mut self.files, id, access)
    }

    /// Atomically adopts an ordered set of exact existing-file descriptors.
    ///
    /// Every request is validated, including duplicate IDs, before any entry is
    /// removed. A failed request therefore leaves the complete registry intact.
    pub fn take_files(
        &mut self,
        requests: &[(GrantId, ResourceRole, GrantAccess)],
    ) -> Result<Vec<GrantedFile>, GrantRegistryError> {
        take_files(&mut self.files, requests)
    }

    /// Duplicates an ordered set of exact file descriptors without adopting them.
    ///
    /// Every request is validated before any descriptor is duplicated. The
    /// original registry remains unchanged on success and failure.
    pub fn duplicate_files(
        &self,
        requests: &[(GrantId, ResourceRole, GrantAccess)],
    ) -> Result<Vec<GrantedFile>, GrantRegistryError> {
        duplicate_files(&self.files, requests)
    }

    /// Moves all existing-file grants into a sendable one-time registry.
    pub fn take_file_registry(&mut self) -> FileGrantRegistry {
        FileGrantRegistry {
            entries: std::mem::take(&mut self.files),
        }
    }

    /// Moves all active directory scopes into an owner-thread registry.
    pub fn take_directory_registry(&mut self) -> DirectoryGrantRegistry {
        DirectoryGrantRegistry {
            entries: std::mem::take(&mut self.directories),
        }
    }

    /// Moves all connected-stream grants into a sendable one-time registry.
    pub fn take_stream_registry(&mut self) -> ConnectedStreamGrantRegistry {
        ConnectedStreamGrantRegistry {
            entries: std::mem::take(&mut self.streams),
        }
    }

    /// Adopts one exact connected local stream once.
    pub fn take_connected_stream(
        &mut self,
        id: &GrantId,
        role: ResourceRole,
    ) -> Result<GrantedUnixStream, GrantRegistryError> {
        take_connected_stream(&mut self.streams, id, role)
    }

    /// Adopts one exact active directory scope once.
    pub fn take_scoped_directory(
        &mut self,
        id: &GrantId,
        role: ResourceRole,
    ) -> Result<GrantedDirectory, GrantRegistryError> {
        take_scoped_directory(&mut self.directories, id, role)
    }

    /// Atomically adopts an ordered set of exact active directory scopes.
    pub fn take_scoped_directories(
        &mut self,
        requests: &[(GrantId, ResourceRole)],
    ) -> Result<Vec<GrantedDirectory>, GrantRegistryError> {
        take_scoped_directories(&mut self.directories, requests)
    }
}

/// One-time registry containing only thread-transferable existing-file grants.
#[derive(Default)]
pub struct FileGrantRegistry {
    entries: HashMap<GrantId, GrantedFile>,
}

impl fmt::Debug for FileGrantRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FileGrantRegistry")
            .field("entries", &"<redacted>")
            .finish()
    }
}

impl FileGrantRegistry {
    /// Returns the number of unadopted file grants.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns whether no unadopted file authority remains.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Adopts one exact existing-file descriptor once.
    pub fn take_file(
        &mut self,
        id: &GrantId,
        role: ResourceRole,
        access: GrantAccess,
    ) -> Result<GrantedFile, GrantRegistryError> {
        take_file(&mut self.entries, id, role, access)
    }

    /// Adopts one exact regular or block-special drive backing once.
    pub fn take_drive_backing(
        &mut self,
        id: &GrantId,
        access: GrantAccess,
    ) -> Result<GrantedFile, GrantRegistryError> {
        take_drive_backing(&mut self.entries, id, access)
    }

    /// Atomically adopts an ordered set of exact existing-file descriptors.
    pub fn take_files(
        &mut self,
        requests: &[(GrantId, ResourceRole, GrantAccess)],
    ) -> Result<Vec<GrantedFile>, GrantRegistryError> {
        take_files(&mut self.entries, requests)
    }

    /// Duplicates an ordered set of exact file descriptors without adopting them.
    pub fn duplicate_files(
        &self,
        requests: &[(GrantId, ResourceRole, GrantAccess)],
    ) -> Result<Vec<GrantedFile>, GrantRegistryError> {
        duplicate_files(&self.entries, requests)
    }

    /// Duplicates one exact drive backing without adopting the original.
    pub fn duplicate_drive_backing(
        &self,
        id: &GrantId,
        access: GrantAccess,
    ) -> Result<GrantedFile, GrantRegistryError> {
        let file = self.entries.get(id).ok_or(GrantRegistryError)?;
        if !matches_drive_backing(file, access) {
            return Err(GrantRegistryError);
        }
        duplicate_file(file)
    }

    /// Returns one reserved file grant to the same registry after an aborted
    /// consumer transaction.
    pub fn restore_file(
        &mut self,
        id: GrantId,
        file: GrantedFile,
    ) -> Result<(), GrantRegistryError> {
        if self.entries.contains_key(&id) || file.role == ResourceRole::DriveBacking {
            return Err(GrantRegistryError);
        }
        let previous = self.entries.insert(id, file);
        debug_assert!(previous.is_none());
        Ok(())
    }

    /// Returns one reserved drive backing after an aborted transaction.
    pub fn restore_drive_backing(
        &mut self,
        id: GrantId,
        file: GrantedFile,
    ) -> Result<(), GrantRegistryError> {
        if self.entries.contains_key(&id)
            || file.role != ResourceRole::DriveBacking
            || !matches!(file.access, GrantAccess::ReadOnly | GrantAccess::ReadWrite)
        {
            return Err(GrantRegistryError);
        }
        let previous = self.entries.insert(id, file);
        debug_assert!(previous.is_none());
        Ok(())
    }
}

/// One-time registry containing only connected local-stream grants.
#[derive(Default)]
pub struct ConnectedStreamGrantRegistry {
    entries: HashMap<GrantId, GrantedUnixStream>,
}

impl fmt::Debug for ConnectedStreamGrantRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConnectedStreamGrantRegistry")
            .field("entries", &"<redacted>")
            .finish()
    }
}

impl ConnectedStreamGrantRegistry {
    /// Returns the number of unadopted connected-stream grants.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns whether no unadopted connected-stream authority remains.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Validates one exact connected local stream without adopting it.
    #[must_use]
    pub fn validates_connected_stream(&self, id: &GrantId, role: ResourceRole) -> bool {
        matches_connected_stream(&self.entries, id, role)
    }

    /// Adopts one exact connected local stream once.
    pub fn take_connected_stream(
        &mut self,
        id: &GrantId,
        role: ResourceRole,
    ) -> Result<GrantedUnixStream, GrantRegistryError> {
        take_connected_stream(&mut self.entries, id, role)
    }

    /// Returns one reserved connected stream after an aborted consumer transaction.
    pub fn restore_connected_stream(
        &mut self,
        id: GrantId,
        stream: GrantedUnixStream,
    ) -> Result<(), GrantRegistryError> {
        if self.entries.contains_key(&id)
            || stream.role != ResourceRole::SnapshotPagerStream
            || stream.access != GrantAccess::ReadWrite
        {
            return Err(GrantRegistryError);
        }
        let previous = self.entries.insert(id, stream);
        debug_assert!(previous.is_none());
        Ok(())
    }
}

/// One-time owner-thread registry containing active directory scopes.
#[derive(Default)]
pub struct DirectoryGrantRegistry {
    entries: HashMap<GrantId, GrantedDirectory>,
}

impl fmt::Debug for DirectoryGrantRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DirectoryGrantRegistry")
            .field("entries", &"<redacted>")
            .finish()
    }
}

impl DirectoryGrantRegistry {
    /// Returns the number of unadopted directory grants.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns whether no unadopted directory authority remains.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Adopts one exact active directory scope once.
    pub fn take_scoped_directory(
        &mut self,
        id: &GrantId,
        role: ResourceRole,
    ) -> Result<GrantedDirectory, GrantRegistryError> {
        take_scoped_directory(&mut self.entries, id, role)
    }

    /// Atomically adopts an ordered set of exact active directory scopes.
    pub fn take_scoped_directories(
        &mut self,
        requests: &[(GrantId, ResourceRole)],
    ) -> Result<Vec<GrantedDirectory>, GrantRegistryError> {
        take_scoped_directories(&mut self.entries, requests)
    }

    /// Returns one reserved directory grant after an aborted owner transaction.
    pub fn restore_scoped_directory(
        &mut self,
        id: GrantId,
        directory: GrantedDirectory,
    ) -> Result<(), GrantRegistryError> {
        if self.entries.contains_key(&id)
            || !directory.role.is_scoped_directory()
            || !directory.role.permits(directory.access)
        {
            return Err(GrantRegistryError);
        }
        let previous = self.entries.insert(id, directory);
        debug_assert!(previous.is_none());
        Ok(())
    }
}

fn take_scoped_directory(
    entries: &mut HashMap<GrantId, GrantedDirectory>,
    id: &GrantId,
    role: ResourceRole,
) -> Result<GrantedDirectory, GrantRegistryError> {
    let matches = matches!(
        entries.get(id),
        Some(directory)
            if directory.role == role && role.permits(directory.access)
    );
    if !matches {
        return Err(GrantRegistryError);
    }
    entries.remove(id).ok_or(GrantRegistryError)
}

fn take_scoped_directories(
    entries: &mut HashMap<GrantId, GrantedDirectory>,
    requests: &[(GrantId, ResourceRole)],
) -> Result<Vec<GrantedDirectory>, GrantRegistryError> {
    let mut ids = HashSet::with_capacity(requests.len());
    for (id, role) in requests {
        if !ids.insert(id)
            || !matches!(
                entries.get(id),
                Some(directory)
                    if directory.role == *role
                        && role.permits(directory.access)
            )
        {
            return Err(GrantRegistryError);
        }
    }

    let mut directories = Vec::new();
    directories
        .try_reserve_exact(requests.len())
        .map_err(|_| GrantRegistryError)?;
    for (id, _) in requests {
        let Some(directory) = entries.remove(id) else {
            for (restored_id, restored_directory) in requests.iter().zip(directories.drain(..)).map(
                |((restored_id, _), restored_directory)| (restored_id.clone(), restored_directory),
            ) {
                entries.insert(restored_id, restored_directory);
            }
            return Err(GrantRegistryError);
        };
        directories.push(directory);
    }
    Ok(directories)
}

fn take_connected_stream(
    entries: &mut HashMap<GrantId, GrantedUnixStream>,
    id: &GrantId,
    role: ResourceRole,
) -> Result<GrantedUnixStream, GrantRegistryError> {
    if !matches_connected_stream(entries, id, role) {
        return Err(GrantRegistryError);
    }
    entries.remove(id).ok_or(GrantRegistryError)
}

fn matches_connected_stream(
    entries: &HashMap<GrantId, GrantedUnixStream>,
    id: &GrantId,
    role: ResourceRole,
) -> bool {
    matches!(
        entries.get(id),
        Some(stream)
            if stream.role == role
                && role == ResourceRole::SnapshotPagerStream
                && stream.access == GrantAccess::ReadWrite
    )
}

fn take_file(
    entries: &mut HashMap<GrantId, GrantedFile>,
    id: &GrantId,
    role: ResourceRole,
    access: GrantAccess,
) -> Result<GrantedFile, GrantRegistryError> {
    let matches = matches!(
        entries.get(id),
        Some(file) if matches_generic_file(file, role, access)
    );
    if !matches {
        return Err(GrantRegistryError);
    }
    entries.remove(id).ok_or(GrantRegistryError)
}

fn take_drive_backing(
    entries: &mut HashMap<GrantId, GrantedFile>,
    id: &GrantId,
    access: GrantAccess,
) -> Result<GrantedFile, GrantRegistryError> {
    if !entries
        .get(id)
        .is_some_and(|file| matches_drive_backing(file, access))
    {
        return Err(GrantRegistryError);
    }
    entries.remove(id).ok_or(GrantRegistryError)
}

fn take_files(
    entries: &mut HashMap<GrantId, GrantedFile>,
    requests: &[(GrantId, ResourceRole, GrantAccess)],
) -> Result<Vec<GrantedFile>, GrantRegistryError> {
    let mut ids = HashSet::with_capacity(requests.len());
    for (id, role, access) in requests {
        if !ids.insert(id)
            || !matches!(
                entries.get(id),
                Some(file) if matches_generic_file(file, *role, *access)
            )
        {
            return Err(GrantRegistryError);
        }
    }

    let mut files = Vec::new();
    files
        .try_reserve_exact(requests.len())
        .map_err(|_| GrantRegistryError)?;
    for (id, _, _) in requests {
        let Some(file) = entries.remove(id) else {
            for (restored_id, restored_file) in requests
                .iter()
                .zip(files.drain(..))
                .map(|((restored_id, _, _), restored_file)| (restored_id.clone(), restored_file))
            {
                entries.insert(restored_id, restored_file);
            }
            return Err(GrantRegistryError);
        };
        files.push(file);
    }
    Ok(files)
}

fn duplicate_files(
    entries: &HashMap<GrantId, GrantedFile>,
    requests: &[(GrantId, ResourceRole, GrantAccess)],
) -> Result<Vec<GrantedFile>, GrantRegistryError> {
    let mut ids = HashSet::with_capacity(requests.len());
    for (id, role, access) in requests {
        if !ids.insert(id)
            || !matches!(
                entries.get(id),
                Some(file) if matches_generic_file(file, *role, *access)
            )
        {
            return Err(GrantRegistryError);
        }
    }

    let mut files = Vec::new();
    files
        .try_reserve_exact(requests.len())
        .map_err(|_| GrantRegistryError)?;
    for (id, _, _) in requests {
        let file = entries.get(id).ok_or(GrantRegistryError)?;
        files.push(duplicate_file(file)?);
    }
    Ok(files)
}

fn matches_generic_file(file: &GrantedFile, role: ResourceRole, access: GrantAccess) -> bool {
    role != ResourceRole::DriveBacking
        && file.role == role
        && file.access == access
        && file.kind == GrantObjectKind::RegularFile
        && file.block_device.is_none()
}

fn matches_drive_backing(file: &GrantedFile, access: GrantAccess) -> bool {
    file.role == ResourceRole::DriveBacking
        && file.access == access
        && matches!(access, GrantAccess::ReadOnly | GrantAccess::ReadWrite)
        && matches!(
            (file.kind, file.block_device),
            (GrantObjectKind::RegularFile, None) | (GrantObjectKind::BlockDevice, Some(_))
        )
}

fn duplicate_file(file: &GrantedFile) -> Result<GrantedFile, GrantRegistryError> {
    // SAFETY: the source descriptor remains live for fcntl; success returns
    // an independently owned close-on-exec descriptor.
    let descriptor = unsafe { libc::fcntl(file.descriptor.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if descriptor < 0 {
        return Err(GrantRegistryError);
    }
    // SAFETY: descriptor is the fresh duplicate returned above.
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    Ok(GrantedFile {
        role: file.role,
        access: file.access,
        kind: file.kind,
        identity: file.identity,
        status_flags: file.status_flags,
        block_device: file.block_device,
        descriptor,
    })
}

/// Adopted existing-file capability.
pub struct GrantedFile {
    role: ResourceRole,
    access: GrantAccess,
    kind: GrantObjectKind,
    identity: ObjectIdentity,
    status_flags: u32,
    block_device: Option<BlockDeviceGrant>,
    descriptor: OwnedFd,
}

impl fmt::Debug for GrantedFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrantedFile")
            .field("role", &self.role)
            .field("access", &self.access)
            .field("kind", &self.kind)
            .field("identity", &"<redacted>")
            .field("status_flags", &"<redacted>")
            .field(
                "block_device",
                &self.block_device.as_ref().map(|_| "<redacted>"),
            )
            .field("descriptor", &"<owned>")
            .finish()
    }
}

impl GrantedFile {
    /// Returns the exact opened access.
    #[must_use]
    pub const fn access(&self) -> GrantAccess {
        self.access
    }

    /// Returns the authenticated descriptor kind.
    #[must_use]
    pub const fn kind(&self) -> GrantObjectKind {
        self.kind
    }

    /// Returns the verified stable identity without exposing a path.
    #[must_use]
    pub const fn identity(&self) -> ObjectIdentity {
        self.identity
    }

    /// Returns the authenticated stable status flags.
    #[must_use]
    pub const fn status_flags(&self) -> u32 {
        self.status_flags
    }

    /// Returns authenticated block metadata, if this is a block-special drive.
    #[must_use]
    pub const fn block_device(&self) -> Option<BlockDeviceGrant> {
        self.block_device
    }

    /// Returns the live descriptor without transferring ownership.
    #[must_use]
    pub fn as_raw_fd(&self) -> RawFd {
        self.descriptor.as_raw_fd()
    }

    /// Transfers descriptor ownership to a resource consumer.
    #[must_use]
    pub fn into_owned_fd(self) -> OwnedFd {
        self.descriptor
    }
}

/// Adopted already-connected local stream capability.
pub struct GrantedUnixStream {
    role: ResourceRole,
    access: GrantAccess,
    identity: ObjectIdentity,
    source_identity: ObjectIdentity,
    status_flags: u32,
    peer: ConnectedUnixPeer,
    descriptor: OwnedFd,
}

impl fmt::Debug for GrantedUnixStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrantedUnixStream")
            .field("role", &self.role)
            .field("access", &self.access)
            .field("identity", &"<redacted>")
            .field("source_identity", &"<redacted>")
            .field("status_flags", &"<redacted>")
            .field("peer", &"<redacted>")
            .field("descriptor", &"<owned>")
            .finish()
    }
}

impl GrantedUnixStream {
    /// Returns the verified connected descriptor identity.
    #[must_use]
    pub const fn identity(&self) -> ObjectIdentity {
        self.identity
    }

    /// Returns the launcher-validated source socket identity.
    #[must_use]
    pub const fn source_identity(&self) -> ObjectIdentity {
        self.source_identity
    }

    /// Returns the authenticated connected peer identity.
    #[must_use]
    pub const fn peer(&self) -> ConnectedUnixPeer {
        self.peer
    }

    /// Returns the authenticated stable stream status.
    #[must_use]
    pub const fn status_flags(&self) -> u32 {
        self.status_flags
    }

    /// Returns the live descriptor without transferring ownership.
    #[must_use]
    pub fn as_raw_fd(&self) -> RawFd {
        self.descriptor.as_raw_fd()
    }

    /// Transfers the connected stream into its consumer.
    #[must_use]
    pub fn into_stream(self) -> UnixStream {
        UnixStream::from(self.descriptor)
    }
}

/// Adopted active process-lifetime directory scope.
pub struct GrantedDirectory {
    role: ResourceRole,
    access: GrantAccess,
    identity: ObjectIdentity,
    anchor: OwnedFd,
    scope: ScopedBookmark,
}

impl fmt::Debug for GrantedDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrantedDirectory")
            .field("role", &self.role)
            .field("access", &self.access)
            .field("identity", &"<redacted>")
            .field("anchor", &"<owned>")
            .field("scope", &self.scope)
            .finish()
    }
}

impl GrantedDirectory {
    /// Returns the resolved path while the scope is active.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.scope.path()
    }

    /// Returns the verified stable directory identity.
    #[must_use]
    pub const fn identity(&self) -> ObjectIdentity {
        self.identity
    }

    /// Returns the anchor descriptor without transferring ownership.
    #[must_use]
    pub fn anchor_fd(&self) -> RawFd {
        self.anchor.as_raw_fd()
    }
}

/// Receiver-side batch that exposes no authority before exact Commit.
pub struct StagedGrantBatch {
    session: SessionId,
    batch: Option<BatchId>,
    next_sequence: u64,
    declaration: Option<BatchDeclaration>,
    records_received: u16,
    bookmark_bytes_received: u32,
    singleton_roles: HashSet<ResourceRole>,
    identities: HashSet<ObjectIdentity>,
    entries: HashMap<GrantId, StagedResource>,
    poisoned: bool,
}

impl fmt::Debug for StagedGrantBatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StagedGrantBatch")
            .field("session", &"<redacted>")
            .field("batch", &self.batch.as_ref().map(|_| "<redacted>"))
            .field("state", &"<redacted>")
            .finish()
    }
}

impl StagedGrantBatch {
    /// Starts an empty receiver bound to one lifecycle session.
    #[must_use]
    pub fn new(session: SessionId) -> Self {
        Self {
            session,
            batch: None,
            next_sequence: 0,
            declaration: None,
            records_received: 0,
            bookmark_bytes_received: 0,
            singleton_roles: HashSet::new(),
            identities: HashSet::new(),
            entries: HashMap::new(),
            poisoned: false,
        }
    }

    /// Applies one exact datagram, returning a registry only at valid Commit.
    pub fn accept(
        &mut self,
        received: ReceivedGrant,
    ) -> Result<Option<CommittedGrantBatch>, GrantRegistryError> {
        if self.poisoned {
            return Err(GrantRegistryError);
        }
        let result = self.accept_inner(received);
        if result.is_err() || matches!(&result, Ok(Some(_))) {
            self.poisoned = true;
        }
        if result.is_err() {
            self.entries.clear();
        }
        result
    }

    fn accept_inner(
        &mut self,
        received: ReceivedGrant,
    ) -> Result<Option<CommittedGrantBatch>, GrantRegistryError> {
        let ReceivedGrant { frame, descriptor } = received;
        if frame.session != self.session
            || frame.sequence != self.next_sequence
            || self.batch.is_some_and(|batch| frame.batch != batch)
            || descriptor.is_some() != (frame.descriptor_count == 1)
        {
            return Err(GrantRegistryError);
        }
        if self.batch.is_none() {
            self.batch = Some(frame.batch);
        }
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(GrantRegistryError)?;
        self.records_received = self
            .records_received
            .checked_add(1)
            .ok_or(GrantRegistryError)?;
        match frame.record {
            GrantRecord::Begin {
                grant_count,
                record_count,
                bookmark_bytes,
            } => {
                if frame.sequence != 0
                    || descriptor.is_some()
                    || self.declaration.is_some()
                    || grant_count > MAX_GRANTS
                    || !(2..=MAX_GRANT_RECORDS).contains(&record_count)
                    || bookmark_bytes > MAX_BATCH_BOOKMARK_BYTES
                {
                    return Err(GrantRegistryError);
                }
                self.declaration = Some(BatchDeclaration {
                    grant_count,
                    record_count,
                    bookmark_bytes,
                });
                Ok(None)
            }
            GrantRecord::Descriptor {
                id,
                role,
                access,
                kind,
                identity,
                status_flags,
                block_device,
            } => {
                self.require_open_batch()?;
                if role.is_scoped_directory()
                    || !role.permits(access)
                    || !matches!(
                        (kind, block_device),
                        (GrantObjectKind::RegularFile, None)
                            | (GrantObjectKind::BlockDevice, Some(_))
                    )
                    || (kind == GrantObjectKind::BlockDevice && role != ResourceRole::DriveBacking)
                {
                    return Err(GrantRegistryError);
                }
                let descriptor = descriptor.ok_or(GrantRegistryError)?;
                validate_descriptor(
                    descriptor.as_raw_fd(),
                    kind,
                    access,
                    identity,
                    Some(status_flags),
                    block_device,
                )?;
                self.insert_identity_role(&id, role, identity)?;
                self.entries.insert(
                    id,
                    StagedResource::File {
                        role,
                        access,
                        kind,
                        identity,
                        status_flags,
                        block_device,
                        descriptor,
                    },
                );
                Ok(None)
            }
            GrantRecord::ConnectedStream {
                id,
                role,
                access,
                identity,
                source_identity,
                status_flags,
                peer,
            } => {
                self.require_open_batch()?;
                if role != ResourceRole::SnapshotPagerStream
                    || access != GrantAccess::ReadWrite
                    || identity.inode == 0
                    || source_identity.inode == 0
                {
                    return Err(GrantRegistryError);
                }
                let descriptor = descriptor.ok_or(GrantRegistryError)?;
                validate_descriptor(
                    descriptor.as_raw_fd(),
                    GrantObjectKind::ConnectedUnixStream,
                    access,
                    identity,
                    Some(status_flags),
                    None,
                )?;
                validate_connected_stream_descriptor(descriptor.as_raw_fd(), peer)?;
                self.insert_identity_role(&id, role, identity)?;
                self.entries.insert(
                    id,
                    StagedResource::Stream {
                        role,
                        access,
                        identity,
                        source_identity,
                        status_flags,
                        peer,
                        descriptor,
                    },
                );
                Ok(None)
            }
            GrantRecord::ScopedDirectory {
                id,
                role,
                access,
                identity,
                bookmark_bytes,
                fragment_count,
            } => {
                self.require_open_batch()?;
                if !role.is_scoped_directory()
                    || !role.permits(access)
                    || bookmark_bytes == 0
                    || bookmark_bytes > MAX_BOOKMARK_BYTES
                    || fragment_count == 0
                    || fragment_count > MAX_GRANT_RECORDS
                {
                    return Err(GrantRegistryError);
                }
                let descriptor = descriptor.ok_or(GrantRegistryError)?;
                validate_descriptor(
                    descriptor.as_raw_fd(),
                    GrantObjectKind::Directory,
                    access,
                    identity,
                    None,
                    None,
                )?;
                self.insert_identity_role(&id, role, identity)?;
                self.entries.insert(
                    id,
                    StagedResource::Directory {
                        role,
                        access,
                        identity,
                        anchor: descriptor,
                        expected_bytes: bookmark_bytes,
                        expected_fragments: fragment_count,
                        fragments: 0,
                        bookmark: Vec::with_capacity(
                            usize::try_from(bookmark_bytes).map_err(|_| GrantRegistryError)?,
                        ),
                    },
                );
                Ok(None)
            }
            GrantRecord::BookmarkFragment { id, offset, bytes } => {
                self.require_open_batch()?;
                if descriptor.is_some() || bytes.is_empty() {
                    return Err(GrantRegistryError);
                }
                let StagedResource::Directory {
                    expected_bytes,
                    expected_fragments,
                    fragments,
                    bookmark,
                    ..
                } = self.entries.get_mut(&id).ok_or(GrantRegistryError)?
                else {
                    return Err(GrantRegistryError);
                };
                if usize::try_from(offset).ok() != Some(bookmark.len())
                    || *fragments >= *expected_fragments
                {
                    return Err(GrantRegistryError);
                }
                let next_length = bookmark
                    .len()
                    .checked_add(bytes.len())
                    .ok_or(GrantRegistryError)?;
                if u32::try_from(next_length)
                    .ok()
                    .is_none_or(|value| value > *expected_bytes)
                {
                    return Err(GrantRegistryError);
                }
                bookmark.extend_from_slice(&bytes);
                *fragments = fragments.checked_add(1).ok_or(GrantRegistryError)?;
                self.bookmark_bytes_received = self
                    .bookmark_bytes_received
                    .checked_add(u32::try_from(bytes.len()).map_err(|_| GrantRegistryError)?)
                    .ok_or(GrantRegistryError)?;
                Ok(None)
            }
            GrantRecord::Commit {
                grant_count,
                record_count,
                bookmark_bytes,
            } => {
                if descriptor.is_some() {
                    return Err(GrantRegistryError);
                }
                self.commit(
                    BatchDeclaration {
                        grant_count,
                        record_count,
                        bookmark_bytes,
                    },
                    frame.sequence,
                )
                .map(Some)
            }
        }
    }

    fn require_open_batch(&self) -> Result<(), GrantRegistryError> {
        let declaration = self.declaration.ok_or(GrantRegistryError)?;
        if self.records_received >= declaration.record_count {
            return Err(GrantRegistryError);
        }
        Ok(())
    }

    fn insert_identity_role(
        &mut self,
        id: &GrantId,
        role: ResourceRole,
        identity: ObjectIdentity,
    ) -> Result<(), GrantRegistryError> {
        if self.entries.contains_key(id)
            || !self.identities.insert(identity)
            || (!role.is_repeatable() && !self.singleton_roles.insert(role))
        {
            return Err(GrantRegistryError);
        }
        Ok(())
    }

    fn commit(
        &mut self,
        commit: BatchDeclaration,
        final_sequence: u64,
    ) -> Result<CommittedGrantBatch, GrantRegistryError> {
        let declaration = self.declaration.ok_or(GrantRegistryError)?;
        if commit != declaration
            || self.records_received != declaration.record_count
            || self.entries.len() != usize::from(declaration.grant_count)
            || self.bookmark_bytes_received != declaration.bookmark_bytes
        {
            return Err(GrantRegistryError);
        }
        for resource in self.entries.values() {
            if let StagedResource::Directory {
                expected_bytes,
                expected_fragments,
                fragments,
                bookmark,
                ..
            } = resource
                && (u32::try_from(bookmark.len()).ok() != Some(*expected_bytes)
                    || fragments != expected_fragments)
            {
                return Err(GrantRegistryError);
            }
        }

        let staged = std::mem::take(&mut self.entries);
        let mut files = HashMap::with_capacity(staged.len());
        let mut directories = HashMap::new();
        let mut streams = HashMap::new();
        for (id, resource) in staged {
            match resource {
                StagedResource::File {
                    role,
                    access,
                    kind,
                    identity,
                    status_flags,
                    block_device,
                    descriptor,
                } => {
                    files.insert(
                        id,
                        GrantedFile {
                            role,
                            access,
                            kind,
                            identity,
                            status_flags,
                            block_device,
                            descriptor,
                        },
                    );
                }
                StagedResource::Stream {
                    role,
                    access,
                    identity,
                    source_identity,
                    status_flags,
                    peer,
                    descriptor,
                } => {
                    streams.insert(
                        id,
                        GrantedUnixStream {
                            role,
                            access,
                            identity,
                            source_identity,
                            status_flags,
                            peer,
                            descriptor,
                        },
                    );
                }
                StagedResource::Directory {
                    role,
                    access,
                    identity,
                    anchor,
                    bookmark,
                    ..
                } => {
                    let scope = ScopedBookmark::resolve(&bookmark)?;
                    // A freshly minted usable implicit bookmark may report stale.
                    // Observe the private bit without logging or making it a verdict.
                    let _ = scope.is_stale();
                    validate_scoped_path(scope.path(), identity, access)?;
                    directories.insert(
                        id,
                        GrantedDirectory {
                            role,
                            access,
                            identity,
                            anchor,
                            scope,
                        },
                    );
                }
            }
        }
        Ok(CommittedGrantBatch {
            registry: GrantRegistry {
                files,
                directories,
                streams,
            },
            batch: self.batch.ok_or(GrantRegistryError)?,
            grant_count: declaration.grant_count,
            final_sequence,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BatchDeclaration {
    grant_count: u16,
    record_count: u16,
    bookmark_bytes: u32,
}

enum StagedResource {
    File {
        role: ResourceRole,
        access: GrantAccess,
        kind: GrantObjectKind,
        identity: ObjectIdentity,
        status_flags: u32,
        block_device: Option<BlockDeviceGrant>,
        descriptor: OwnedFd,
    },
    Stream {
        role: ResourceRole,
        access: GrantAccess,
        identity: ObjectIdentity,
        source_identity: ObjectIdentity,
        status_flags: u32,
        peer: ConnectedUnixPeer,
        descriptor: OwnedFd,
    },
    Directory {
        role: ResourceRole,
        access: GrantAccess,
        identity: ObjectIdentity,
        anchor: OwnedFd,
        expected_bytes: u32,
        expected_fragments: u16,
        fragments: u16,
        bookmark: Vec<u8>,
    },
}

fn validate_descriptor(
    descriptor: RawFd,
    kind: GrantObjectKind,
    access: GrantAccess,
    identity: ObjectIdentity,
    expected_status_flags: Option<u32>,
    expected_block_device: Option<BlockDeviceGrant>,
) -> Result<(), GrantRegistryError> {
    // SAFETY: F_GETFD and F_GETFL inspect the live received descriptor.
    let descriptor_flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    // SAFETY: F_GETFL inspects the same live descriptor.
    let status_flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if descriptor_flags < 0
        || status_flags < 0
        || descriptor_flags & libc::FD_CLOEXEC == 0
        || !access_matches(status_flags, access)
        || (kind == GrantObjectKind::BlockDevice && status_flags & libc::O_APPEND != 0)
        || expected_status_flags.is_some_and(|expected| {
            let actual = if kind == GrantObjectKind::BlockDevice {
                normalized_block_status_flags(status_flags)
            } else if kind == GrantObjectKind::ConnectedUnixStream {
                u32::try_from(status_flags & (libc::O_ACCMODE | libc::O_NONBLOCK)).ok()
            } else {
                u32::try_from(status_flags).ok()
            };
            actual != Some(expected)
        })
    {
        return Err(GrantRegistryError);
    }
    let stat = descriptor_stat(descriptor)?;
    let actual_kind = match stat.st_mode & libc::S_IFMT {
        libc::S_IFREG => GrantObjectKind::RegularFile,
        libc::S_IFDIR => GrantObjectKind::Directory,
        libc::S_IFBLK => GrantObjectKind::BlockDevice,
        libc::S_IFSOCK => GrantObjectKind::ConnectedUnixStream,
        _ => return Err(GrantRegistryError),
    };
    let actual_identity = ObjectIdentity {
        device: normalized_device(stat.st_dev),
        inode: stat.st_ino,
    };
    let target_device = normalized_device(stat.st_rdev);
    if actual_kind != kind
        || actual_identity != identity
        || match (kind, expected_block_device) {
            (GrantObjectKind::BlockDevice, Some(block)) => target_device != block.target_device(),
            (
                GrantObjectKind::RegularFile
                | GrantObjectKind::Directory
                | GrantObjectKind::ConnectedUnixStream,
                None,
            ) => target_device != 0,
            _ => true,
        }
    {
        return Err(GrantRegistryError);
    }
    Ok(())
}

fn validate_connected_stream_descriptor(
    descriptor: RawFd,
    expected_peer: ConnectedUnixPeer,
) -> Result<(), GrantRegistryError> {
    if socket_int_option(descriptor, libc::SO_TYPE)? != libc::SOCK_STREAM
        || socket_int_option(descriptor, libc::SO_ERROR)? != 0
    {
        return Err(GrantRegistryError);
    }
    let mut address = MaybeUninit::<libc::sockaddr_storage>::zeroed();
    let mut length = libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_storage>())
        .map_err(|_| GrantRegistryError)?;
    // SAFETY: Address storage and length are writable for this live socket.
    if unsafe { libc::getpeername(descriptor, address.as_mut_ptr().cast(), &raw mut length) } != 0 {
        return Err(GrantRegistryError);
    }
    // SAFETY: Successful getpeername initialized the returned address prefix.
    let address = unsafe { address.assume_init() };
    if address.ss_family
        != libc::sa_family_t::try_from(libc::AF_UNIX).map_err(|_| GrantRegistryError)?
    {
        return Err(GrantRegistryError);
    }
    let actual = peer_identity(descriptor).map_err(|_| GrantRegistryError)?;
    let actual_pid = u32::try_from(actual.pid).map_err(|_| GrantRegistryError)?;
    let actual_peer =
        ConnectedUnixPeer::new(actual.uid, actual.gid, actual_pid).ok_or(GrantRegistryError)?;
    if actual_peer != expected_peer {
        return Err(GrantRegistryError);
    }
    Ok(())
}

fn socket_int_option(descriptor: RawFd, option: libc::c_int) -> Result<i32, GrantRegistryError> {
    let mut value = 0_i32;
    let mut length =
        libc::socklen_t::try_from(std::mem::size_of::<i32>()).map_err(|_| GrantRegistryError)?;
    // SAFETY: Value and length are writable for this live socket descriptor.
    if unsafe {
        libc::getsockopt(
            descriptor,
            libc::SOL_SOCKET,
            option,
            (&raw mut value).cast(),
            &raw mut length,
        )
    } != 0
        || usize::try_from(length).ok() != Some(std::mem::size_of::<i32>())
    {
        return Err(GrantRegistryError);
    }
    Ok(value)
}

fn descriptor_stat(descriptor: RawFd) -> Result<libc::stat, GrantRegistryError> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: stat points to writable storage and descriptor remains live.
    if unsafe { libc::fstat(descriptor, stat.as_mut_ptr()) } != 0 {
        return Err(GrantRegistryError);
    }
    // SAFETY: Successful fstat initialized the complete structure.
    Ok(unsafe { stat.assume_init() })
}

fn validate_scoped_path(
    path: &Path,
    expected: ObjectIdentity,
    access: GrantAccess,
) -> Result<(), GrantRegistryError> {
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(
        libc::O_CLOEXEC | libc::O_NOFOLLOW_ANY | libc::O_DIRECTORY | libc::O_NONBLOCK,
    );
    let directory = options.open(path).map_err(|_| GrantRegistryError)?;
    let stat = descriptor_stat(directory.as_raw_fd())?;
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR
        || (ObjectIdentity {
            device: normalized_device(stat.st_dev),
            inode: stat.st_ino,
        }) != expected
    {
        return Err(GrantRegistryError);
    }
    let requested = if access == GrantAccess::ConnectChildren {
        libc::X_OK
    } else {
        libc::W_OK | libc::X_OK
    };
    // SAFETY: The reopened verified directory remains live, the fixed dot
    // component is NUL-terminated, and faccessat performs no mutation.
    if unsafe {
        libc::faccessat(
            directory.as_raw_fd(),
            c".".as_ptr(),
            requested,
            libc::AT_EACCESS,
        )
    } != 0
    {
        return Err(GrantRegistryError);
    }
    Ok(())
}
fn access_matches(flags: libc::c_int, access: GrantAccess) -> bool {
    let actual = flags & libc::O_ACCMODE;
    match access {
        GrantAccess::ReadOnly | GrantAccess::CreateChildren | GrantAccess::ConnectChildren => {
            actual == libc::O_RDONLY
        }
        GrantAccess::WriteOnly => actual == libc::O_WRONLY,
        GrantAccess::ReadWrite => actual == libc::O_RDWR,
    }
}

fn normalized_device(device: libc::dev_t) -> u64 {
    u64::from(u32::from_ne_bytes(device.to_ne_bytes()))
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::net::{UnixDatagram, UnixListener};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::macos::bookmark::create_implicit_bookmark;
    use crate::macos::grant_transport::ReceivedGrant;
    use crate::{GrantFrame, ProtocolError};

    use super::*;

    fn duplicate(file: &File) -> OwnedFd {
        // SAFETY: file remains live and a successful result is independently owned.
        let descriptor = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 100) };
        assert!(descriptor >= 100);
        // SAFETY: descriptor is the fresh duplicate above.
        unsafe { OwnedFd::from_raw_fd(descriptor) }
    }

    fn unlinked_file() -> File {
        static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

        for _ in 0..1_024 {
            let sequence = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "bangbang-grant-registry-unlinked-{}-{sequence}",
                std::process::id()
            ));
            let creator = match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("unlinked fixture should be created: {error}"),
            };
            let file = File::open(&path).expect("unlinked fixture should reopen read-only");
            fs::remove_file(&path).expect("unlinked fixture name should be removed");
            drop(creator);
            return file;
        }

        panic!("bounded unlinked fixture attempts should succeed");
    }

    fn assert_descriptor_released(descriptor: RawFd, original_identity: ObjectIdentity) {
        let mut stat = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: stat points to writable storage and fstat only observes the
        // descriptor number, which may now be closed or reused by another test.
        if unsafe { libc::fstat(descriptor, stat.as_mut_ptr()) } != 0 {
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EBADF)
            );
            return;
        }
        // SAFETY: Successful fstat initialized the complete structure.
        let stat = unsafe { stat.assume_init() };
        assert_ne!(
            ObjectIdentity {
                device: normalized_device(stat.st_dev),
                inode: stat.st_ino,
            },
            original_identity,
            "received descriptor must release its original open file"
        );
    }

    fn receive(
        session: SessionId,
        batch: BatchId,
        sequence: u64,
        record: GrantRecord,
        descriptor: Option<OwnedFd>,
    ) -> ReceivedGrant {
        ReceivedGrant {
            frame: GrantFrame {
                session,
                batch,
                sequence,
                descriptor_count: record.descriptor_count(),
                record,
            },
            descriptor,
        }
    }

    fn normalize_test_stream(descriptor: RawFd) {
        // SAFETY: These fcntl operations update one live test descriptor.
        let descriptor_flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
        assert!(descriptor_flags >= 0);
        // SAFETY: The descriptor remains live and uniquely owned by the fixture.
        let set_descriptor_flags = unsafe {
            libc::fcntl(
                descriptor,
                libc::F_SETFD,
                descriptor_flags | libc::FD_CLOEXEC,
            )
        };
        assert!(set_descriptor_flags >= 0);
        // SAFETY: F_GETFL inspects the same live descriptor.
        let status_flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
        assert!(status_flags >= 0);
        // SAFETY: The descriptor remains live and uniquely owned by the fixture.
        let set_status_flags =
            unsafe { libc::fcntl(descriptor, libc::F_SETFL, status_flags | libc::O_NONBLOCK) };
        assert!(set_status_flags >= 0);
    }

    fn stream_record(descriptor: RawFd, peer: ConnectedUnixPeer) -> GrantRecord {
        let stat = descriptor_stat(descriptor).expect("socket stat should read");
        // SAFETY: F_GETFL inspects one live test descriptor.
        let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
        GrantRecord::ConnectedStream {
            id: GrantId::parse("rejected-pager").expect("ID should parse"),
            role: ResourceRole::SnapshotPagerStream,
            access: GrantAccess::ReadWrite,
            identity: ObjectIdentity {
                device: normalized_device(stat.st_dev),
                inode: stat.st_ino,
            },
            source_identity: ObjectIdentity {
                device: 201,
                inode: 202,
            },
            status_flags: u32::try_from(flags & (libc::O_ACCMODE | libc::O_NONBLOCK))
                .expect("socket flags should fit"),
            peer,
        }
    }

    fn assert_connected_stream_record_rejected(descriptor: OwnedFd, record: GrantRecord) {
        let descriptor_number = descriptor.as_raw_fd();
        let stat = descriptor_stat(descriptor_number).expect("descriptor stat should read");
        let descriptor_identity = ObjectIdentity {
            device: normalized_device(stat.st_dev),
            inode: stat.st_ino,
        };
        let session = SessionId::from_bytes([37; 32]);
        let batch = BatchId::from_bytes([38; 16]);
        let mut staged = StagedGrantBatch::new(session);
        staged
            .accept(receive(
                session,
                batch,
                0,
                GrantRecord::Begin {
                    grant_count: 1,
                    record_count: 3,
                    bookmark_bytes: 0,
                },
                None,
            ))
            .expect("begin should stage");
        assert!(
            staged
                .accept(receive(session, batch, 1, record, Some(descriptor)))
                .is_err()
        );
        assert_descriptor_released(descriptor_number, descriptor_identity);
    }

    #[test]
    fn empty_batch_commits_only_after_exact_commit() {
        let session = SessionId::from_bytes([1; 32]);
        let batch = BatchId::from_bytes([2; 16]);
        let mut staged = StagedGrantBatch::new(session);
        assert!(
            staged
                .accept(receive(
                    session,
                    batch,
                    0,
                    GrantRecord::Begin {
                        grant_count: 0,
                        record_count: 2,
                        bookmark_bytes: 0,
                    },
                    None,
                ))
                .expect("begin should stage")
                .is_none()
        );
        let committed = staged
            .accept(receive(
                session,
                batch,
                1,
                GrantRecord::Commit {
                    grant_count: 0,
                    record_count: 2,
                    bookmark_bytes: 0,
                },
                None,
            ))
            .expect("commit should validate")
            .expect("commit should return registry");
        assert!(committed.registry.is_empty());
        assert_eq!(committed.final_sequence, 1);
    }

    #[test]
    fn receiver_revalidates_scoped_directory_limits_without_codec_assumptions() {
        let directory = std::env::temp_dir().join(format!(
            "bangbang-grant-registry-limits-{}",
            std::process::id()
        ));
        let _ = fs::create_dir(&directory);
        let directory = fs::canonicalize(directory).expect("directory should canonicalize");
        let anchor_file = File::open(&directory).expect("directory anchor should open");
        let stat = descriptor_stat(anchor_file.as_raw_fd()).expect("anchor stat should read");
        let identity = ObjectIdentity {
            device: normalized_device(stat.st_dev),
            inode: stat.st_ino,
        };
        let session = SessionId::from_bytes([21; 32]);
        let batch = BatchId::from_bytes([22; 16]);

        for (bookmark_bytes, fragment_count) in
            [(MAX_BOOKMARK_BYTES + 1, 1), (1, MAX_GRANT_RECORDS + 1)]
        {
            let mut staged = StagedGrantBatch::new(session);
            staged
                .accept(receive(
                    session,
                    batch,
                    0,
                    GrantRecord::Begin {
                        grant_count: 1,
                        record_count: 3,
                        bookmark_bytes,
                    },
                    None,
                ))
                .expect("begin should stage");
            let anchor = duplicate(&anchor_file);
            let anchor_fd = anchor.as_raw_fd();
            assert!(
                staged
                    .accept(receive(
                        session,
                        batch,
                        1,
                        GrantRecord::ScopedDirectory {
                            id: GrantId::parse("api-directory").expect("ID should parse"),
                            role: ResourceRole::ApiSocketDirectory,
                            access: GrantAccess::CreateChildren,
                            identity,
                            bookmark_bytes,
                            fragment_count,
                        },
                        Some(anchor),
                    ))
                    .is_err()
            );
            assert_descriptor_released(anchor_fd, identity);
        }

        fs::remove_dir(directory).expect("directory fixture should clean up");
    }

    #[test]
    fn descriptor_is_typed_and_adopted_once() {
        let session = SessionId::from_bytes([3; 32]);
        let batch = BatchId::from_bytes([4; 16]);
        let file = File::open(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("fixture should open");
        let descriptor = duplicate(&file);
        let stat = descriptor_stat(descriptor.as_raw_fd()).expect("stat should read");
        // SAFETY: F_GETFL inspects the live descriptor.
        let flags = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFL) };
        let id = GrantId::parse("kernel").expect("ID should parse");
        let mut staged = StagedGrantBatch::new(session);
        staged
            .accept(receive(
                session,
                batch,
                0,
                GrantRecord::Begin {
                    grant_count: 1,
                    record_count: 3,
                    bookmark_bytes: 0,
                },
                None,
            ))
            .expect("begin should stage");
        staged
            .accept(receive(
                session,
                batch,
                1,
                GrantRecord::Descriptor {
                    id: id.clone(),
                    role: ResourceRole::KernelImage,
                    access: GrantAccess::ReadOnly,
                    kind: GrantObjectKind::RegularFile,
                    identity: ObjectIdentity {
                        device: normalized_device(stat.st_dev),
                        inode: stat.st_ino,
                    },
                    status_flags: u32::try_from(flags).expect("flags should fit"),
                    block_device: None,
                },
                Some(descriptor),
            ))
            .expect("descriptor should stage");
        let mut registry = staged
            .accept(receive(
                session,
                batch,
                2,
                GrantRecord::Commit {
                    grant_count: 1,
                    record_count: 3,
                    bookmark_bytes: 0,
                },
                None,
            ))
            .expect("commit should validate")
            .expect("registry should commit")
            .registry;
        let grant = registry
            .take_file(&id, ResourceRole::KernelImage, GrantAccess::ReadOnly)
            .expect("matching grant should adopt");
        assert_eq!(grant.access(), GrantAccess::ReadOnly);
        assert!(registry.is_empty());
        assert!(
            registry
                .take_file(&id, ResourceRole::KernelImage, GrantAccess::ReadOnly)
                .is_err()
        );
    }

    #[test]
    fn connected_stream_is_revalidated_committed_and_adopted_once() {
        let session = SessionId::from_bytes([33; 32]);
        let batch = BatchId::from_bytes([34; 16]);
        let id = GrantId::parse("snapshot-pager").expect("ID should parse");
        let (stream, peer_stream) = UnixStream::pair().expect("stream pair should open");
        stream
            .set_nonblocking(true)
            .expect("granted stream should become nonblocking");
        let descriptor: OwnedFd = stream.into();
        let stat = descriptor_stat(descriptor.as_raw_fd()).expect("stream stat should read");
        // SAFETY: F_GETFL inspects the live connected stream.
        let flags = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFL) };
        let status_flags = u32::try_from(flags & (libc::O_ACCMODE | libc::O_NONBLOCK))
            .expect("stream flags should fit");
        let identity = ObjectIdentity {
            device: normalized_device(stat.st_dev),
            inode: stat.st_ino,
        };
        let actual_peer =
            peer_identity(descriptor.as_raw_fd()).expect("stream peer identity should read");
        let peer = ConnectedUnixPeer::new(
            actual_peer.uid,
            actual_peer.gid,
            u32::try_from(actual_peer.pid).expect("peer PID should fit"),
        )
        .expect("peer identity should validate");
        let source_identity = ObjectIdentity {
            device: 91,
            inode: 92,
        };
        let mut staged = StagedGrantBatch::new(session);
        staged
            .accept(receive(
                session,
                batch,
                0,
                GrantRecord::Begin {
                    grant_count: 1,
                    record_count: 3,
                    bookmark_bytes: 0,
                },
                None,
            ))
            .expect("begin should stage");
        staged
            .accept(receive(
                session,
                batch,
                1,
                GrantRecord::ConnectedStream {
                    id: id.clone(),
                    role: ResourceRole::SnapshotPagerStream,
                    access: GrantAccess::ReadWrite,
                    identity,
                    source_identity,
                    status_flags,
                    peer,
                },
                Some(descriptor),
            ))
            .expect("connected stream should stage");
        let mut registry = staged
            .accept(receive(
                session,
                batch,
                2,
                GrantRecord::Commit {
                    grant_count: 1,
                    record_count: 3,
                    bookmark_bytes: 0,
                },
                None,
            ))
            .expect("commit should validate")
            .expect("registry should commit")
            .registry;
        assert_eq!(registry.len(), 1);
        let mut streams = registry.take_stream_registry();
        assert!(registry.is_empty());
        assert_eq!(streams.len(), 1);
        assert_eq!(
            format!("{streams:?}"),
            "ConnectedStreamGrantRegistry { entries: \"<redacted>\" }"
        );
        assert!(streams.validates_connected_stream(&id, ResourceRole::SnapshotPagerStream));
        assert!(!streams.validates_connected_stream(&id, ResourceRole::SnapshotStateInput));
        assert_eq!(streams.len(), 1);
        let granted = streams
            .take_connected_stream(&id, ResourceRole::SnapshotPagerStream)
            .expect("matching stream should adopt");
        assert_eq!(granted.identity(), identity);
        assert_eq!(granted.source_identity(), source_identity);
        assert_eq!(granted.peer(), peer);
        assert_eq!(granted.status_flags(), status_flags);
        assert!(streams.is_empty());
        assert!(!streams.validates_connected_stream(&id, ResourceRole::SnapshotPagerStream));
        assert!(
            streams
                .take_connected_stream(&id, ResourceRole::SnapshotPagerStream)
                .is_err()
        );
        let adopted = granted.into_stream();
        adopted
            .set_write_timeout(Some(std::time::Duration::from_secs(1)))
            .expect("adopted stream timeout should configure");
        drop(peer_stream);
    }

    #[test]
    fn connected_stream_record_rejects_a_regular_descriptor() {
        let session = SessionId::from_bytes([35; 32]);
        let batch = BatchId::from_bytes([36; 16]);
        let file = unlinked_file();
        let descriptor = duplicate(&file);
        let stat = descriptor_stat(descriptor.as_raw_fd()).expect("file stat should read");
        let identity = ObjectIdentity {
            device: normalized_device(stat.st_dev),
            inode: stat.st_ino,
        };
        let (peer_stream, _other) = UnixStream::pair().expect("peer fixture should open");
        let actual_peer =
            peer_identity(peer_stream.as_raw_fd()).expect("peer identity should read");
        let peer = ConnectedUnixPeer::new(
            actual_peer.uid,
            actual_peer.gid,
            u32::try_from(actual_peer.pid).expect("peer PID should fit"),
        )
        .expect("peer identity should validate");
        let mut staged = StagedGrantBatch::new(session);
        staged
            .accept(receive(
                session,
                batch,
                0,
                GrantRecord::Begin {
                    grant_count: 1,
                    record_count: 3,
                    bookmark_bytes: 0,
                },
                None,
            ))
            .expect("begin should stage");
        assert!(
            staged
                .accept(receive(
                    session,
                    batch,
                    1,
                    GrantRecord::ConnectedStream {
                        id: GrantId::parse("wrong-stream").expect("ID should parse"),
                        role: ResourceRole::SnapshotPagerStream,
                        access: GrantAccess::ReadWrite,
                        identity,
                        source_identity: ObjectIdentity {
                            device: 101,
                            inode: 102,
                        },
                        status_flags: u32::try_from(libc::O_RDWR | libc::O_NONBLOCK)
                            .expect("flags should fit"),
                        peer,
                    },
                    Some(descriptor),
                ))
                .is_err()
        );
    }

    #[test]
    fn connected_stream_record_rejects_listener_datagram_disconnected_and_mismatched_metadata() {
        static NEXT_SOCKET: AtomicU64 = AtomicU64::new(0);

        let (peer_fixture, _peer_other) =
            UnixStream::pair().expect("peer identity fixture should open");
        let actual_peer =
            peer_identity(peer_fixture.as_raw_fd()).expect("peer identity should read");
        let peer = ConnectedUnixPeer::new(
            actual_peer.uid,
            actual_peer.gid,
            u32::try_from(actual_peer.pid).expect("peer PID should fit"),
        )
        .expect("peer identity should validate");

        let listener_path = std::env::temp_dir().join(format!(
            "bangbang-grant-listener-{}-{}",
            std::process::id(),
            NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
        ));
        let listener = UnixListener::bind(&listener_path).expect("listener fixture should bind");
        fs::remove_file(&listener_path).expect("listener fixture name should unlink");
        normalize_test_stream(listener.as_raw_fd());
        let listener: OwnedFd = listener.into();
        let record = stream_record(listener.as_raw_fd(), peer);
        assert_connected_stream_record_rejected(listener, record);

        let (datagram, _datagram_peer) =
            UnixDatagram::pair().expect("datagram fixture should open");
        normalize_test_stream(datagram.as_raw_fd());
        let datagram: OwnedFd = datagram.into();
        let record = stream_record(datagram.as_raw_fd(), peer);
        assert_connected_stream_record_rejected(datagram, record);

        // SAFETY: A successful socket result is immediately uniquely owned.
        let disconnected = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        assert!(disconnected >= 0);
        // SAFETY: `disconnected` is the fresh descriptor returned above.
        let disconnected = unsafe { OwnedFd::from_raw_fd(disconnected) };
        normalize_test_stream(disconnected.as_raw_fd());
        let record = stream_record(disconnected.as_raw_fd(), peer);
        assert_connected_stream_record_rejected(disconnected, record);

        let (stream, _stream_peer) =
            UnixStream::pair().expect("identity mismatch fixture should open");
        normalize_test_stream(stream.as_raw_fd());
        let descriptor: OwnedFd = stream.into();
        let mut record = stream_record(descriptor.as_raw_fd(), peer);
        let GrantRecord::ConnectedStream { identity, .. } = &mut record else {
            panic!("helper should return a connected-stream record");
        };
        identity.inode = identity.inode.checked_add(1).unwrap_or(1);
        assert_connected_stream_record_rejected(descriptor, record);

        let (stream, _stream_peer) = UnixStream::pair().expect("peer mismatch fixture should open");
        normalize_test_stream(stream.as_raw_fd());
        let descriptor: OwnedFd = stream.into();
        let mut record = stream_record(descriptor.as_raw_fd(), peer);
        let GrantRecord::ConnectedStream { status_flags, .. } = &mut record else {
            panic!("helper should return a connected-stream record");
        };
        *status_flags = u32::try_from(libc::O_RDWR).expect("access flags should fit");
        assert_connected_stream_record_rejected(descriptor, record);

        let (stream, _stream_peer) = UnixStream::pair().expect("peer mismatch fixture should open");
        normalize_test_stream(stream.as_raw_fd());
        let descriptor: OwnedFd = stream.into();
        let mut record = stream_record(descriptor.as_raw_fd(), peer);
        let GrantRecord::ConnectedStream {
            peer: expected_peer,
            ..
        } = &mut record
        else {
            panic!("helper should return a connected-stream record");
        };
        let wrong_pid = if peer.process_id() == i32::MAX as u32 {
            peer.process_id() - 1
        } else {
            peer.process_id() + 1
        };
        *expected_peer = ConnectedUnixPeer::new(peer.user_id(), peer.group_id(), wrong_pid)
            .expect("wrong positive PID should validate");
        assert_connected_stream_record_rejected(descriptor, record);

        let (stream, _stream_peer) =
            UnixStream::pair().expect("access mismatch fixture should open");
        normalize_test_stream(stream.as_raw_fd());
        let descriptor: OwnedFd = stream.into();
        let mut record = stream_record(descriptor.as_raw_fd(), peer);
        let GrantRecord::ConnectedStream { access, .. } = &mut record else {
            panic!("helper should return a connected-stream record");
        };
        *access = GrantAccess::ReadOnly;
        assert_connected_stream_record_rejected(descriptor, record);
    }

    #[test]
    fn dedicated_drive_operations_preserve_complete_block_metadata() {
        let source = File::open("/dev/null").expect("descriptor fixture should open");
        let descriptor = duplicate(&source);
        let stat = descriptor_stat(descriptor.as_raw_fd()).expect("fixture stat should read");
        // SAFETY: F_GETFL only observes the live fixture descriptor.
        let flags = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFL) };
        let status_flags = normalized_block_status_flags(flags)
            .expect("normalized status flags should fit the wire");
        let identity = ObjectIdentity {
            device: normalized_device(stat.st_dev),
            inode: stat.st_ino,
        };
        let block_device =
            BlockDeviceGrant::new(77, 4096, 16).expect("block metadata should validate");
        let id = GrantId::parse("block-drive").expect("grant ID should parse");
        let mut registry = FileGrantRegistry {
            entries: HashMap::from([(
                id.clone(),
                GrantedFile {
                    role: ResourceRole::DriveBacking,
                    access: GrantAccess::ReadOnly,
                    kind: GrantObjectKind::BlockDevice,
                    identity,
                    status_flags,
                    block_device: Some(block_device),
                    descriptor,
                },
            )]),
        };

        assert!(
            registry
                .take_file(&id, ResourceRole::DriveBacking, GrantAccess::ReadOnly)
                .is_err(),
            "generic adoption must not erase drive authority"
        );
        let duplicated = registry
            .duplicate_drive_backing(&id, GrantAccess::ReadOnly)
            .expect("dedicated duplication should succeed");
        assert_ne!(
            duplicated.as_raw_fd(),
            registry
                .entries
                .get(&id)
                .expect("original block grant should remain")
                .as_raw_fd()
        );
        assert_eq!(duplicated.kind(), GrantObjectKind::BlockDevice);
        assert_eq!(duplicated.identity(), identity);
        assert_eq!(duplicated.status_flags(), status_flags);
        assert_eq!(duplicated.block_device(), Some(block_device));
        drop(duplicated);

        let reserved = registry
            .take_drive_backing(&id, GrantAccess::ReadOnly)
            .expect("dedicated take should reserve the block grant");
        assert!(registry.is_empty());
        registry
            .restore_drive_backing(id.clone(), reserved)
            .expect("dedicated restore should preserve the block grant");
        let restored = registry
            .take_drive_backing(&id, GrantAccess::ReadOnly)
            .expect("restored block grant should remain adoptable");
        assert_eq!(restored.kind(), GrantObjectKind::BlockDevice);
        assert_eq!(restored.identity(), identity);
        assert_eq!(restored.status_flags(), status_flags);
        assert_eq!(restored.block_device(), Some(block_device));
    }

    #[test]
    fn file_registry_batch_adoption_is_failure_atomic() {
        let kernel_id = GrantId::parse("kernel").expect("kernel ID should parse");
        let initrd_id = GrantId::parse("initrd").expect("initrd ID should parse");
        let kernel = File::open(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("kernel fixture should open");
        let initrd = File::open(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"))
            .expect("initrd fixture should open");
        let kernel_descriptor = duplicate(&kernel);
        let initrd_descriptor = duplicate(&initrd);
        let kernel_stat =
            descriptor_stat(kernel_descriptor.as_raw_fd()).expect("kernel stat should read");
        let initrd_stat =
            descriptor_stat(initrd_descriptor.as_raw_fd()).expect("initrd stat should read");
        let mut registry = GrantRegistry {
            files: HashMap::from([
                (
                    kernel_id.clone(),
                    GrantedFile {
                        role: ResourceRole::KernelImage,
                        access: GrantAccess::ReadOnly,
                        kind: GrantObjectKind::RegularFile,
                        identity: ObjectIdentity {
                            device: normalized_device(kernel_stat.st_dev),
                            inode: kernel_stat.st_ino,
                        },
                        status_flags: 0,
                        block_device: None,
                        descriptor: kernel_descriptor,
                    },
                ),
                (
                    initrd_id.clone(),
                    GrantedFile {
                        role: ResourceRole::InitrdImage,
                        access: GrantAccess::ReadOnly,
                        kind: GrantObjectKind::RegularFile,
                        identity: ObjectIdentity {
                            device: normalized_device(initrd_stat.st_dev),
                            inode: initrd_stat.st_ino,
                        },
                        status_flags: 0,
                        block_device: None,
                        descriptor: initrd_descriptor,
                    },
                ),
            ]),
            directories: HashMap::new(),
            streams: HashMap::new(),
        };
        let mut files = registry.take_file_registry();
        assert!(registry.is_empty());
        assert_eq!(
            format!("{files:?}"),
            "FileGrantRegistry { entries: \"<redacted>\" }"
        );
        assert!(
            files
                .take_files(&[])
                .expect("empty adoption should succeed")
                .is_empty()
        );
        assert_eq!(files.len(), 2);
        assert!(
            files
                .take_file(
                    &kernel_id,
                    ResourceRole::KernelImage,
                    GrantAccess::WriteOnly,
                )
                .is_err()
        );
        assert_eq!(files.len(), 2);

        let wrong_second = [
            (
                kernel_id.clone(),
                ResourceRole::KernelImage,
                GrantAccess::ReadOnly,
            ),
            (
                initrd_id.clone(),
                ResourceRole::KernelImage,
                GrantAccess::ReadOnly,
            ),
        ];
        assert!(files.take_files(&wrong_second).is_err());
        assert_eq!(files.len(), 2);

        let duplicate = [
            (
                kernel_id.clone(),
                ResourceRole::KernelImage,
                GrantAccess::ReadOnly,
            ),
            (
                kernel_id.clone(),
                ResourceRole::KernelImage,
                GrantAccess::ReadOnly,
            ),
        ];
        assert!(files.take_files(&duplicate).is_err());
        assert_eq!(files.len(), 2);

        let inspected = files
            .duplicate_files(&[
                (
                    kernel_id.clone(),
                    ResourceRole::KernelImage,
                    GrantAccess::ReadOnly,
                ),
                (
                    initrd_id.clone(),
                    ResourceRole::InitrdImage,
                    GrantAccess::ReadOnly,
                ),
            ])
            .expect("matching files should duplicate without adoption");
        assert_eq!(inspected.len(), 2);
        assert_eq!(files.len(), 2);
        assert_ne!(inspected[0].as_raw_fd(), kernel.as_raw_fd());
        assert_ne!(inspected[1].as_raw_fd(), initrd.as_raw_fd());
        drop(inspected);

        assert!(files.duplicate_files(&wrong_second).is_err());
        assert!(files.duplicate_files(&duplicate).is_err());
        assert_eq!(files.len(), 2);

        let reserved = files
            .take_file(&kernel_id, ResourceRole::KernelImage, GrantAccess::ReadOnly)
            .expect("matching grant should reserve");
        assert_eq!(files.len(), 1);
        files
            .restore_file(kernel_id.clone(), reserved)
            .expect("aborted reservation should restore exact authority");
        assert_eq!(files.len(), 2);
        assert_eq!(
            files
                .duplicate_files(&[(
                    kernel_id.clone(),
                    ResourceRole::KernelImage,
                    GrantAccess::ReadOnly,
                )])
                .expect("restored authority should remain usable")
                .len(),
            1
        );

        let adopted = files
            .take_files(&[
                (
                    initrd_id.clone(),
                    ResourceRole::InitrdImage,
                    GrantAccess::ReadOnly,
                ),
                (
                    kernel_id.clone(),
                    ResourceRole::KernelImage,
                    GrantAccess::ReadOnly,
                ),
            ])
            .expect("matching reverse-order pair should adopt");
        assert_eq!(adopted.len(), 2);
        assert_eq!(adopted[0].role, ResourceRole::InitrdImage);
        assert_eq!(adopted[1].role, ResourceRole::KernelImage);
        assert!(files.is_empty());
        assert!(
            files
                .take_file(&kernel_id, ResourceRole::KernelImage, GrantAccess::ReadOnly,)
                .is_err()
        );
        assert!(
            files
                .take_file(&initrd_id, ResourceRole::InitrdImage, GrantAccess::ReadOnly,)
                .is_err()
        );
    }

    #[test]
    fn directory_registry_batch_adoption_is_failure_atomic() {
        let base = std::env::temp_dir().join(format!(
            "bangbang-grant-registry-pair-{}",
            std::process::id()
        ));
        let state_path = base.join("state");
        let memory_path = base.join("memory");
        fs::create_dir_all(&state_path).expect("state directory should create");
        fs::create_dir_all(&memory_path).expect("memory directory should create");

        let make_directory = |path: &Path| {
            let bookmark =
                create_implicit_bookmark(path, true).expect("directory bookmark should create");
            let scope = ScopedBookmark::resolve(&bookmark).expect("bookmark should resolve");
            let anchor_file = File::open(path).expect("directory anchor should open");
            let anchor = duplicate(&anchor_file);
            let stat = descriptor_stat(anchor.as_raw_fd()).expect("directory stat should read");
            GrantedDirectory {
                role: ResourceRole::SnapshotOutputDirectory,
                access: GrantAccess::CreateChildren,
                identity: ObjectIdentity {
                    device: normalized_device(stat.st_dev),
                    inode: stat.st_ino,
                },
                anchor,
                scope,
            }
        };

        let state_id = GrantId::parse("snapshot-state-output").expect("state ID should parse");
        let memory_id = GrantId::parse("snapshot-memory-output").expect("memory ID should parse");
        let mut directories = DirectoryGrantRegistry {
            entries: HashMap::from([
                (state_id.clone(), make_directory(&state_path)),
                (memory_id.clone(), make_directory(&memory_path)),
            ]),
        };

        assert!(
            directories
                .take_scoped_directories(&[
                    (state_id.clone(), ResourceRole::SnapshotOutputDirectory),
                    (memory_id.clone(), ResourceRole::ApiSocketDirectory),
                ])
                .is_err()
        );
        assert_eq!(directories.len(), 2);
        assert!(
            directories
                .take_scoped_directories(&[
                    (state_id.clone(), ResourceRole::SnapshotOutputDirectory),
                    (state_id.clone(), ResourceRole::SnapshotOutputDirectory),
                ])
                .is_err()
        );
        assert_eq!(directories.len(), 2);

        let adopted = directories
            .take_scoped_directories(&[
                (memory_id, ResourceRole::SnapshotOutputDirectory),
                (state_id, ResourceRole::SnapshotOutputDirectory),
            ])
            .expect("matching directories should adopt atomically");
        assert_eq!(adopted.len(), 2);
        assert!(directories.is_empty());
        drop(adopted);

        fs::remove_dir(memory_path).expect("memory directory should clean up");
        fs::remove_dir(state_path).expect("state directory should clean up");
        fs::remove_dir(base).expect("base directory should clean up");
    }

    #[test]
    fn rejects_cross_session_sequence_and_identity_mismatch() {
        let session = SessionId::from_bytes([5; 32]);
        let batch = BatchId::from_bytes([6; 16]);
        let mut staged = StagedGrantBatch::new(session);
        assert!(
            staged
                .accept(receive(
                    SessionId::from_bytes([7; 32]),
                    batch,
                    0,
                    GrantRecord::Begin {
                        grant_count: 0,
                        record_count: 2,
                        bookmark_bytes: 0,
                    },
                    None,
                ))
                .is_err()
        );

        let _ = ProtocolError::InvalidFrame;
    }

    #[test]
    fn rejection_poison_closes_staged_authority_and_rejects_late_records() {
        let session = SessionId::from_bytes([8; 32]);
        let batch = BatchId::from_bytes([9; 16]);
        let file = unlinked_file();
        let descriptor = duplicate(&file);
        let received_fd = descriptor.as_raw_fd();
        let stat = descriptor_stat(received_fd).expect("stat should read");
        let identity = ObjectIdentity {
            device: normalized_device(stat.st_dev),
            inode: stat.st_ino,
        };
        // SAFETY: F_GETFL inspects the live descriptor.
        let flags = unsafe { libc::fcntl(received_fd, libc::F_GETFL) };
        let id = GrantId::parse("kernel").expect("ID should parse");
        let mut staged = StagedGrantBatch::new(session);
        staged
            .accept(receive(
                session,
                batch,
                0,
                GrantRecord::Begin {
                    grant_count: 1,
                    record_count: 3,
                    bookmark_bytes: 0,
                },
                None,
            ))
            .expect("begin should stage");
        staged
            .accept(receive(
                session,
                batch,
                1,
                GrantRecord::Descriptor {
                    id,
                    role: ResourceRole::KernelImage,
                    access: GrantAccess::ReadOnly,
                    kind: GrantObjectKind::RegularFile,
                    identity,
                    status_flags: u32::try_from(flags).expect("flags should fit"),
                    block_device: None,
                },
                Some(descriptor),
            ))
            .expect("descriptor should stage");
        assert!(
            staged
                .accept(receive(
                    session,
                    batch,
                    1,
                    GrantRecord::Commit {
                        grant_count: 1,
                        record_count: 3,
                        bookmark_bytes: 0,
                    },
                    None,
                ))
                .is_err(),
            "replayed sequence must reject the batch"
        );
        assert_descriptor_released(received_fd, identity);
        assert!(
            staged
                .accept(receive(
                    session,
                    batch,
                    2,
                    GrantRecord::Commit {
                        grant_count: 1,
                        record_count: 3,
                        bookmark_bytes: 0,
                    },
                    None,
                ))
                .is_err(),
            "poisoned batches must reject every late record"
        );
    }

    #[test]
    fn rejects_cross_batch_partial_commit_and_descriptor_identity_mismatch() {
        let session = SessionId::from_bytes([10; 32]);
        let batch = BatchId::from_bytes([11; 16]);
        let mut cross_batch = StagedGrantBatch::new(session);
        cross_batch
            .accept(receive(
                session,
                batch,
                0,
                GrantRecord::Begin {
                    grant_count: 0,
                    record_count: 2,
                    bookmark_bytes: 0,
                },
                None,
            ))
            .expect("begin should stage");
        assert!(
            cross_batch
                .accept(receive(
                    session,
                    BatchId::from_bytes([12; 16]),
                    1,
                    GrantRecord::Commit {
                        grant_count: 0,
                        record_count: 2,
                        bookmark_bytes: 0,
                    },
                    None,
                ))
                .is_err()
        );

        let mut partial = StagedGrantBatch::new(session);
        partial
            .accept(receive(
                session,
                batch,
                0,
                GrantRecord::Begin {
                    grant_count: 1,
                    record_count: 3,
                    bookmark_bytes: 0,
                },
                None,
            ))
            .expect("begin should stage");
        assert!(
            partial
                .accept(receive(
                    session,
                    batch,
                    1,
                    GrantRecord::Commit {
                        grant_count: 1,
                        record_count: 3,
                        bookmark_bytes: 0,
                    },
                    None,
                ))
                .is_err()
        );

        let file = unlinked_file();
        let descriptor = duplicate(&file);
        let descriptor_fd = descriptor.as_raw_fd();
        let stat = descriptor_stat(descriptor_fd).expect("stat should read");
        let identity = ObjectIdentity {
            device: normalized_device(stat.st_dev),
            inode: stat.st_ino,
        };
        // SAFETY: F_GETFL inspects the live descriptor.
        let flags = unsafe { libc::fcntl(descriptor_fd, libc::F_GETFL) };
        let mut mismatched = StagedGrantBatch::new(session);
        mismatched
            .accept(receive(
                session,
                batch,
                0,
                GrantRecord::Begin {
                    grant_count: 1,
                    record_count: 3,
                    bookmark_bytes: 0,
                },
                None,
            ))
            .expect("begin should stage");
        assert!(
            mismatched
                .accept(receive(
                    session,
                    batch,
                    1,
                    GrantRecord::Descriptor {
                        id: GrantId::parse("kernel").expect("ID should parse"),
                        role: ResourceRole::KernelImage,
                        access: GrantAccess::ReadOnly,
                        kind: GrantObjectKind::RegularFile,
                        identity: ObjectIdentity {
                            device: u64::MAX,
                            inode: u64::MAX,
                        },
                        status_flags: u32::try_from(flags).expect("flags should fit"),
                        block_device: None,
                    },
                    Some(descriptor),
                ))
                .is_err()
        );
        assert_descriptor_released(descriptor_fd, identity);
    }

    #[test]
    fn receiver_rejects_descriptor_aliases_and_closes_the_whole_batch() {
        let session = SessionId::from_bytes([15; 32]);
        let batch = BatchId::from_bytes([16; 16]);
        let file = unlinked_file();
        let first = duplicate(&file);
        let second = duplicate(&file);
        let first_fd = first.as_raw_fd();
        let second_fd = second.as_raw_fd();
        let stat = descriptor_stat(first_fd).expect("stat should read");
        // SAFETY: F_GETFL inspects the live descriptor.
        let flags = unsafe { libc::fcntl(first_fd, libc::F_GETFL) };
        let identity = ObjectIdentity {
            device: normalized_device(stat.st_dev),
            inode: stat.st_ino,
        };
        let mut staged = StagedGrantBatch::new(session);
        staged
            .accept(receive(
                session,
                batch,
                0,
                GrantRecord::Begin {
                    grant_count: 2,
                    record_count: 4,
                    bookmark_bytes: 0,
                },
                None,
            ))
            .expect("begin should stage");
        staged
            .accept(receive(
                session,
                batch,
                1,
                GrantRecord::Descriptor {
                    id: GrantId::parse("drive-one").expect("ID should parse"),
                    role: ResourceRole::DriveBacking,
                    access: GrantAccess::ReadOnly,
                    kind: GrantObjectKind::RegularFile,
                    identity,
                    status_flags: u32::try_from(flags).expect("flags should fit"),
                    block_device: None,
                },
                Some(first),
            ))
            .expect("first descriptor should stage");
        assert!(
            staged
                .accept(receive(
                    session,
                    batch,
                    2,
                    GrantRecord::Descriptor {
                        id: GrantId::parse("drive-two").expect("ID should parse"),
                        role: ResourceRole::DriveBacking,
                        access: GrantAccess::ReadOnly,
                        kind: GrantObjectKind::RegularFile,
                        identity,
                        status_flags: u32::try_from(flags).expect("flags should fit"),
                        block_device: None,
                    },
                    Some(second),
                ))
                .is_err()
        );
        assert_descriptor_released(first_fd, identity);
        assert_descriptor_released(second_fd, identity);
    }

    #[test]
    fn fragmented_directory_bookmark_commits_with_exact_anchor_and_active_scope() {
        let directory = std::env::temp_dir().join(format!(
            "bangbang-grant-registry-directory-{}",
            std::process::id()
        ));
        let _ = fs::create_dir(&directory);
        let directory = fs::canonicalize(directory).expect("directory should canonicalize");
        let bookmark = create_implicit_bookmark(&directory, true).expect("bookmark should create");
        let anchor_file = File::open(&directory).expect("directory anchor should open");
        let anchor = duplicate(&anchor_file);
        let stat = descriptor_stat(anchor.as_raw_fd()).expect("anchor stat should read");
        let identity = ObjectIdentity {
            device: normalized_device(stat.st_dev),
            inode: stat.st_ino,
        };
        let session = SessionId::from_bytes([13; 32]);
        let batch = BatchId::from_bytes([14; 16]);
        let id = GrantId::parse("api-directory").expect("ID should parse");
        let split = bookmark.len().div_ceil(2);
        let fragments = bookmark.chunks(split).collect::<Vec<_>>();
        let record_count = u16::try_from(fragments.len() + 3).expect("count should fit");
        let bookmark_bytes = u32::try_from(bookmark.len()).expect("bookmark should fit");
        let mut staged = StagedGrantBatch::new(session);
        staged
            .accept(receive(
                session,
                batch,
                0,
                GrantRecord::Begin {
                    grant_count: 1,
                    record_count,
                    bookmark_bytes,
                },
                None,
            ))
            .expect("begin should stage");
        staged
            .accept(receive(
                session,
                batch,
                1,
                GrantRecord::ScopedDirectory {
                    id: id.clone(),
                    role: ResourceRole::ApiSocketDirectory,
                    access: GrantAccess::CreateChildren,
                    identity,
                    bookmark_bytes,
                    fragment_count: u16::try_from(fragments.len())
                        .expect("fragment count should fit"),
                },
                Some(anchor),
            ))
            .expect("directory should stage");
        for (index, fragment) in fragments.into_iter().enumerate() {
            staged
                .accept(receive(
                    session,
                    batch,
                    u64::try_from(index + 2).expect("sequence should fit"),
                    GrantRecord::BookmarkFragment {
                        id: id.clone(),
                        offset: u32::try_from(index * split).expect("offset should fit"),
                        bytes: fragment.to_vec(),
                    },
                    None,
                ))
                .expect("fragment should stage");
        }
        let mut registry = staged
            .accept(receive(
                session,
                batch,
                u64::from(record_count - 1),
                GrantRecord::Commit {
                    grant_count: 1,
                    record_count,
                    bookmark_bytes,
                },
                None,
            ))
            .expect("commit should validate")
            .expect("registry should commit")
            .registry;
        let granted = registry
            .take_scoped_directory(&id, ResourceRole::ApiSocketDirectory)
            .expect("directory should adopt once");
        assert_eq!(granted.identity(), identity);
        let child = granted.path().join("scope-proof");
        fs::write(&child, b"scope").expect("active scope should permit child creation");
        assert!(!format!("{granted:?}").contains(directory.to_string_lossy().as_ref()));
        drop(granted);
        fs::remove_file(child).expect("scope proof should clean up");
        fs::remove_dir(directory).expect("directory fixture should clean up");
    }
}
