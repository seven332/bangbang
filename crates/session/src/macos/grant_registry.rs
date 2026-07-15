//! Failure-atomic worker grant staging and one-time typed adoption.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::OpenOptions;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::macos::bookmark::{BookmarkError, ScopedBookmark};
use crate::macos::grant_transport::ReceivedGrant;
use crate::{
    BatchId, GrantAccess, GrantId, GrantObjectKind, GrantRecord, MAX_BATCH_BOOKMARK_BYTES,
    MAX_BOOKMARK_BYTES, MAX_GRANT_RECORDS, MAX_GRANTS, ObjectIdentity, ResourceRole, SessionId,
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
    entries: HashMap<GrantId, GrantedResource>,
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
        self.entries.len()
    }

    /// Returns whether no unadopted authority remains.
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
        let matches = matches!(
            self.entries.get(id),
            Some(GrantedResource::File(file)) if file.role == role && file.access == access
        );
        if !matches {
            return Err(GrantRegistryError);
        }
        match self.entries.remove(id) {
            Some(GrantedResource::File(file)) => Ok(file),
            Some(GrantedResource::Directory(_)) | None => Err(GrantRegistryError),
        }
    }

    /// Adopts one exact active directory scope once.
    pub fn take_scoped_directory(
        &mut self,
        id: &GrantId,
        role: ResourceRole,
    ) -> Result<GrantedDirectory, GrantRegistryError> {
        let matches = matches!(
            self.entries.get(id),
            Some(GrantedResource::Directory(directory))
                if directory.role == role && directory.access == GrantAccess::CreateChildren
        );
        if !matches {
            return Err(GrantRegistryError);
        }
        match self.entries.remove(id) {
            Some(GrantedResource::Directory(directory)) => Ok(directory),
            Some(GrantedResource::File(_)) | None => Err(GrantRegistryError),
        }
    }
}

enum GrantedResource {
    File(GrantedFile),
    Directory(GrantedDirectory),
}

/// Adopted existing-file capability.
pub struct GrantedFile {
    role: ResourceRole,
    access: GrantAccess,
    identity: ObjectIdentity,
    descriptor: OwnedFd,
}

impl fmt::Debug for GrantedFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrantedFile")
            .field("role", &self.role)
            .field("access", &self.access)
            .field("identity", &"<redacted>")
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

    /// Returns the verified stable identity without exposing a path.
    #[must_use]
    pub const fn identity(&self) -> ObjectIdentity {
        self.identity
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
            } => {
                self.require_open_batch()?;
                if kind != GrantObjectKind::RegularFile
                    || role.is_scoped_directory()
                    || !role.permits(access)
                {
                    return Err(GrantRegistryError);
                }
                let descriptor = descriptor.ok_or(GrantRegistryError)?;
                validate_descriptor(
                    descriptor.as_raw_fd(),
                    GrantObjectKind::RegularFile,
                    access,
                    identity,
                    Some(status_flags),
                )?;
                self.insert_identity_role(&id, role, identity)?;
                self.entries.insert(
                    id,
                    StagedResource::File {
                        role,
                        access,
                        identity,
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
                    || access != GrantAccess::CreateChildren
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
        let mut entries = HashMap::with_capacity(staged.len());
        for (id, resource) in staged {
            let granted = match resource {
                StagedResource::File {
                    role,
                    access,
                    identity,
                    descriptor,
                } => GrantedResource::File(GrantedFile {
                    role,
                    access,
                    identity,
                    descriptor,
                }),
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
                    validate_scoped_path(scope.path(), identity)?;
                    GrantedResource::Directory(GrantedDirectory {
                        role,
                        access,
                        identity,
                        anchor,
                        scope,
                    })
                }
            };
            entries.insert(id, granted);
        }
        Ok(CommittedGrantBatch {
            registry: GrantRegistry { entries },
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
        identity: ObjectIdentity,
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
) -> Result<(), GrantRegistryError> {
    // SAFETY: F_GETFD and F_GETFL inspect the live received descriptor.
    let descriptor_flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    // SAFETY: F_GETFL inspects the same live descriptor.
    let status_flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if descriptor_flags < 0
        || status_flags < 0
        || descriptor_flags & libc::FD_CLOEXEC == 0
        || !access_matches(status_flags, access)
        || expected_status_flags
            .is_some_and(|expected| u32::try_from(status_flags).ok() != Some(expected))
    {
        return Err(GrantRegistryError);
    }
    let stat = descriptor_stat(descriptor)?;
    let actual_kind = match stat.st_mode & libc::S_IFMT {
        libc::S_IFREG => GrantObjectKind::RegularFile,
        libc::S_IFDIR => GrantObjectKind::Directory,
        _ => return Err(GrantRegistryError),
    };
    let actual_identity = ObjectIdentity {
        device: normalized_device(stat.st_dev),
        inode: stat.st_ino,
    };
    if actual_kind != kind || actual_identity != identity {
        return Err(GrantRegistryError);
    }
    Ok(())
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

fn validate_scoped_path(path: &Path, expected: ObjectIdentity) -> Result<(), GrantRegistryError> {
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
    // SAFETY: The reopened verified directory remains live, the fixed dot
    // component is NUL-terminated, and faccessat performs no mutation.
    if unsafe {
        libc::faccessat(
            directory.as_raw_fd(),
            c".".as_ptr(),
            libc::W_OK | libc::X_OK,
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
        GrantAccess::ReadOnly | GrantAccess::CreateChildren => actual == libc::O_RDONLY,
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
            // SAFETY: Rejection must drop the directly constructed received fd.
            assert_eq!(unsafe { libc::fcntl(anchor_fd, libc::F_GETFD) }, -1);
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
        let file = File::open(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("fixture should open");
        let descriptor = duplicate(&file);
        let received_fd = descriptor.as_raw_fd();
        let stat = descriptor_stat(received_fd).expect("stat should read");
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
                    identity: ObjectIdentity {
                        device: normalized_device(stat.st_dev),
                        inode: stat.st_ino,
                    },
                    status_flags: u32::try_from(flags).expect("flags should fit"),
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
        // SAFETY: F_GETFD checks that poisoning dropped the staged owned fd.
        assert_eq!(unsafe { libc::fcntl(received_fd, libc::F_GETFD) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EBADF)
        );
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

        let file = File::open(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("fixture should open");
        let descriptor = duplicate(&file);
        let descriptor_fd = descriptor.as_raw_fd();
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
                    },
                    Some(descriptor),
                ))
                .is_err()
        );
        // SAFETY: Rejection must drop the received descriptor immediately.
        assert_eq!(unsafe { libc::fcntl(descriptor_fd, libc::F_GETFD) }, -1);
    }

    #[test]
    fn receiver_rejects_descriptor_aliases_and_closes_the_whole_batch() {
        let session = SessionId::from_bytes([15; 32]);
        let batch = BatchId::from_bytes([16; 16]);
        let file = File::open(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("fixture should open");
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
                    },
                    Some(second),
                ))
                .is_err()
        );
        // SAFETY: F_GETFD verifies both the earlier staged fd and the triggering
        // fd were dropped when the alias poisoned the batch.
        assert_eq!(unsafe { libc::fcntl(first_fd, libc::F_GETFD) }, -1);
        // SAFETY: Same ownership check for the triggering descriptor.
        assert_eq!(unsafe { libc::fcntl(second_fd, libc::F_GETFD) }, -1);
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
