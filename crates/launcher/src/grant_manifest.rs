use std::collections::HashSet;
use std::ffi::{CString, OsStr, OsString};
use std::fs::OpenOptions;
use std::io::Read;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bangbang_session::macos::bookmark::create_implicit_bookmark;
use bangbang_session::macos::peer_identity;
use bangbang_session::{
    BatchId, BlockDeviceGrant, ConnectedUnixPeer, GRANT_HEADER_BYTES, GrantAccess, GrantFrame,
    GrantId, GrantObjectKind, GrantRecord, MAX_BATCH_BOOKMARK_BYTES, MAX_BOOKMARK_BYTES,
    MAX_GRANT_DATAGRAM_BYTES, MAX_GRANT_RECORDS, MAX_GRANTS, ObjectIdentity, ResourceRole,
    SessionId,
};
use serde::Deserialize;

use crate::LauncherError;

const GRANT_OPTION: &str = "--bangbang-grant-manifest";
const ENVELOPE_DELIMITER: &str = "--";
const MANIFEST_VERSION: u16 = 1;
const MAX_MANIFEST_BYTES: u64 = 256 * 1024;
const MAX_SOURCE_PATH_BYTES: usize = 4096;
const PAGER_CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

/// Parsed launcher-only input and byte-preserved worker arguments.
pub(crate) struct LaunchInput {
    pub(crate) worker_args: Vec<OsString>,
    manifest: Option<PathBuf>,
}

impl std::fmt::Debug for LaunchInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LaunchInput")
            .field("worker_args", &"<redacted>")
            .field("manifest", &self.manifest.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl LaunchInput {
    pub(crate) fn parse(args: Vec<OsString>) -> Result<Self, LauncherError> {
        let mut arguments = args.into_iter();
        let Some(first) = arguments.next() else {
            return Ok(Self {
                worker_args: Vec::new(),
                manifest: None,
            });
        };
        if first != OsStr::new(GRANT_OPTION) {
            return Ok(Self {
                worker_args: std::iter::once(first).chain(arguments).collect(),
                manifest: None,
            });
        }
        let manifest = arguments
            .next()
            .filter(|value| !value.is_empty())
            .ok_or(LauncherError::InvalidGrantInput)?;
        if arguments.next().as_deref() != Some(OsStr::new(ENVELOPE_DELIMITER)) {
            return Err(LauncherError::InvalidGrantInput);
        }
        Ok(Self {
            worker_args: arguments.collect(),
            manifest: Some(PathBuf::from(manifest)),
        })
    }

    pub(crate) fn prepare(self) -> Result<(Vec<OsString>, PreparedGrantBatch), LauncherError> {
        let grants = self
            .manifest
            .as_deref()
            .map(load_manifest)
            .transpose()?
            .unwrap_or_default();
        let batch = PreparedGrantBatch::prepare(grants)?;
        Ok((self.worker_args, batch))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    version: u16,
    grants: Vec<RawGrant>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGrant {
    id: String,
    role: String,
    access: String,
    source: String,
}

struct ManifestGrant {
    id: GrantId,
    role: ResourceRole,
    access: GrantAccess,
    source: PathBuf,
}

impl std::fmt::Debug for ManifestGrant {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManifestGrant")
            .field("id", &"<redacted>")
            .field("role", &self.role)
            .field("access", &self.access)
            .field("source", &"<redacted>")
            .finish()
    }
}

fn load_manifest(path: &Path) -> Result<Vec<ManifestGrant>, LauncherError> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW_ANY);
    let file = options
        .open(path)
        .map_err(|_| LauncherError::InvalidGrantInput)?;
    let metadata = file
        .metadata()
        .map_err(|_| LauncherError::InvalidGrantInput)?;
    if !metadata.is_file() || metadata.len() > MAX_MANIFEST_BYTES {
        return Err(LauncherError::InvalidGrantInput);
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len()).map_err(|_| LauncherError::InvalidGrantInput)?,
    );
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| LauncherError::InvalidGrantInput)?;
    if u64::try_from(bytes.len())
        .ok()
        .is_none_or(|length| length > MAX_MANIFEST_BYTES)
    {
        return Err(LauncherError::InvalidGrantInput);
    }
    parse_manifest(&bytes)
}

fn parse_manifest(bytes: &[u8]) -> Result<Vec<ManifestGrant>, LauncherError> {
    let raw: RawManifest =
        serde_json::from_slice(bytes).map_err(|_| LauncherError::InvalidGrantInput)?;
    if raw.version != MANIFEST_VERSION || raw.grants.len() > usize::from(MAX_GRANTS) {
        return Err(LauncherError::InvalidGrantInput);
    }
    let mut ids = HashSet::new();
    let mut singleton_roles = HashSet::new();
    raw.grants
        .into_iter()
        .map(|grant| {
            let id = GrantId::parse(&grant.id).map_err(|_| LauncherError::InvalidGrantInput)?;
            if !ids.insert(id.clone()) {
                return Err(LauncherError::InvalidGrantInput);
            }
            let role = parse_role(&grant.role)?;
            let access = parse_access(&grant.access)?;
            if !role.permits(access) || (!role.is_repeatable() && !singleton_roles.insert(role)) {
                return Err(LauncherError::InvalidGrantInput);
            }
            let source = PathBuf::from(grant.source);
            let source_bytes = source.as_os_str().as_bytes();
            if !source.is_absolute()
                || source_bytes.is_empty()
                || source_bytes.len() > MAX_SOURCE_PATH_BYTES
                || source_bytes.contains(&0)
                || resource_path_components(&source).is_err()
            {
                return Err(LauncherError::InvalidGrantInput);
            }
            Ok(ManifestGrant {
                id,
                role,
                access,
                source,
            })
        })
        .collect()
}

fn parse_role(value: &str) -> Result<ResourceRole, LauncherError> {
    match value {
        "startup-config" => Ok(ResourceRole::StartupConfig),
        "startup-metadata" => Ok(ResourceRole::StartupMetadata),
        "kernel-image" => Ok(ResourceRole::KernelImage),
        "initrd-image" => Ok(ResourceRole::InitrdImage),
        "drive-backing" => Ok(ResourceRole::DriveBacking),
        "pmem-backing" => Ok(ResourceRole::PmemBacking),
        "api-socket-directory" => Ok(ResourceRole::ApiSocketDirectory),
        "vsock-socket-directory" => Ok(ResourceRole::VsockSocketDirectory),
        "logger-sink" => Ok(ResourceRole::LoggerSink),
        "metrics-sink" => Ok(ResourceRole::MetricsSink),
        "serial-sink" => Ok(ResourceRole::SerialSink),
        "snapshot-describe-input" => Ok(ResourceRole::SnapshotDescribeInput),
        "snapshot-state-input" => Ok(ResourceRole::SnapshotStateInput),
        "snapshot-memory-input" => Ok(ResourceRole::SnapshotMemoryInput),
        "snapshot-output-directory" => Ok(ResourceRole::SnapshotOutputDirectory),
        "vhost-user-socket-directory" => Ok(ResourceRole::VhostUserSocketDirectory),
        "snapshot-pager-stream" => Ok(ResourceRole::SnapshotPagerStream),
        _ => Err(LauncherError::InvalidGrantInput),
    }
}

fn parse_access(value: &str) -> Result<GrantAccess, LauncherError> {
    match value {
        "read-only" => Ok(GrantAccess::ReadOnly),
        "write-only" => Ok(GrantAccess::WriteOnly),
        "read-write" => Ok(GrantAccess::ReadWrite),
        "create-children" => Ok(GrantAccess::CreateChildren),
        "connect-children" => Ok(GrantAccess::ConnectChildren),
        _ => Err(LauncherError::InvalidGrantInput),
    }
}

struct PreparedRecord {
    record: GrantRecord,
    descriptor: Option<OwnedFd>,
}

impl std::fmt::Debug for PreparedRecord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedRecord")
            .field("record", &self.record)
            .field("descriptor", &self.descriptor.as_ref().map(|_| "<owned>"))
            .finish()
    }
}

/// Fully opened, failure-atomic launcher batch.
pub(crate) struct PreparedGrantBatch {
    batch: BatchId,
    grant_count: u16,
    records: Vec<PreparedRecord>,
}

/// Borrowed exact anchor metadata for one socket-directory grant.
#[derive(Clone, Copy)]
pub(crate) struct SocketDirectoryAnchor {
    descriptor: RawFd,
    identity: ObjectIdentity,
}

/// Borrowed exact anchor metadata for one snapshot-output directory grant.
#[derive(Clone, Copy)]
pub(crate) struct SnapshotDirectoryAnchor {
    descriptor: RawFd,
    identity: ObjectIdentity,
}

/// Borrowed exact launcher authority for one block-special drive grant.
#[derive(Clone, Copy)]
pub(crate) struct BlockDriveAnchor {
    descriptor: RawFd,
    access: GrantAccess,
    identity: ObjectIdentity,
    status_flags: u32,
    block_device: BlockDeviceGrant,
}

impl std::fmt::Debug for SocketDirectoryAnchor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SocketDirectoryAnchor")
            .field("descriptor", &"<borrowed>")
            .field("identity", &"<redacted>")
            .finish()
    }
}

impl SocketDirectoryAnchor {
    #[cfg(test)]
    pub(crate) const fn for_test(descriptor: RawFd, identity: ObjectIdentity) -> Self {
        Self {
            descriptor,
            identity,
        }
    }

    pub(crate) const fn descriptor(self) -> RawFd {
        self.descriptor
    }

    pub(crate) const fn identity(self) -> ObjectIdentity {
        self.identity
    }
}

impl std::fmt::Debug for SnapshotDirectoryAnchor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SnapshotDirectoryAnchor")
            .field("descriptor", &"<borrowed>")
            .field("identity", &"<redacted>")
            .finish()
    }
}

impl std::fmt::Debug for BlockDriveAnchor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BlockDriveAnchor")
            .field("descriptor", &"<borrowed>")
            .field("access", &self.access)
            .field("identity", &"<redacted>")
            .field("status_flags", &"<redacted>")
            .field("block_device", &"<redacted>")
            .finish()
    }
}

impl SnapshotDirectoryAnchor {
    pub(crate) const fn descriptor(self) -> RawFd {
        self.descriptor
    }

    pub(crate) const fn identity(self) -> ObjectIdentity {
        self.identity
    }
}

impl BlockDriveAnchor {
    #[cfg(test)]
    pub(crate) const fn for_test(
        descriptor: RawFd,
        access: GrantAccess,
        identity: ObjectIdentity,
        status_flags: u32,
        block_device: BlockDeviceGrant,
    ) -> Self {
        Self {
            descriptor,
            access,
            identity,
            status_flags,
            block_device,
        }
    }

    pub(crate) const fn descriptor(self) -> RawFd {
        self.descriptor
    }

    pub(crate) const fn access(self) -> GrantAccess {
        self.access
    }

    pub(crate) const fn identity(self) -> ObjectIdentity {
        self.identity
    }

    pub(crate) const fn status_flags(self) -> u32 {
        self.status_flags
    }

    pub(crate) const fn block_device(self) -> BlockDeviceGrant {
        self.block_device
    }
}

impl std::fmt::Debug for PreparedGrantBatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedGrantBatch")
            .field("batch", &self.batch)
            .field("grant_count", &"<redacted>")
            .field("records", &"<redacted>")
            .finish()
    }
}

impl PreparedGrantBatch {
    fn prepare(grants: Vec<ManifestGrant>) -> Result<Self, LauncherError> {
        let batch = BatchId::generate().map_err(|_| LauncherError::GrantPreparation)?;
        let grant_count =
            u16::try_from(grants.len()).map_err(|_| LauncherError::GrantPreparation)?;
        let mut identities = HashSet::new();
        let mut records = Vec::new();
        let mut bookmark_bytes = 0_u32;
        for grant in grants {
            let prepared = if grant.role == ResourceRole::SnapshotPagerStream {
                connect_resource(&grant)?
            } else {
                open_resource(&grant)?
            };
            if !identities.insert(prepared.identity) {
                return Err(LauncherError::GrantPreparation);
            }
            if grant.role.is_scoped_directory() {
                let bookmark = create_implicit_bookmark(&grant.source, true)
                    .map_err(|_| LauncherError::GrantPreparation)?;
                let rechecked = open_resource(&grant)?;
                if rechecked.identity != prepared.identity {
                    return Err(LauncherError::GrantPreparation);
                }
                let bookmark_length =
                    u32::try_from(bookmark.len()).map_err(|_| LauncherError::GrantPreparation)?;
                if bookmark_length == 0 || bookmark_length > MAX_BOOKMARK_BYTES {
                    return Err(LauncherError::GrantPreparation);
                }
                bookmark_bytes = bookmark_bytes
                    .checked_add(bookmark_length)
                    .filter(|bytes| *bytes <= MAX_BATCH_BOOKMARK_BYTES)
                    .ok_or(LauncherError::GrantPreparation)?;
                let chunk_bytes = fragment_capacity(&grant.id)?;
                let fragment_count = bookmark.len().div_ceil(chunk_bytes);
                let fragment_count =
                    u16::try_from(fragment_count).map_err(|_| LauncherError::GrantPreparation)?;
                records.push(PreparedRecord {
                    record: GrantRecord::ScopedDirectory {
                        id: grant.id.clone(),
                        role: grant.role,
                        access: grant.access,
                        identity: prepared.identity,
                        bookmark_bytes: bookmark_length,
                        fragment_count,
                    },
                    descriptor: Some(prepared.descriptor),
                });
                for (index, fragment) in bookmark.chunks(chunk_bytes).enumerate() {
                    let offset = index
                        .checked_mul(chunk_bytes)
                        .and_then(|offset| u32::try_from(offset).ok())
                        .ok_or(LauncherError::GrantPreparation)?;
                    records.push(PreparedRecord {
                        record: GrantRecord::BookmarkFragment {
                            id: grant.id.clone(),
                            offset,
                            bytes: fragment.to_vec(),
                        },
                        descriptor: None,
                    });
                }
            } else if grant.role == ResourceRole::SnapshotPagerStream {
                records.push(PreparedRecord {
                    record: GrantRecord::ConnectedStream {
                        id: grant.id,
                        role: grant.role,
                        access: grant.access,
                        identity: prepared.identity,
                        source_identity: prepared
                            .source_identity
                            .ok_or(LauncherError::GrantPreparation)?,
                        status_flags: prepared.status_flags,
                        peer: prepared.peer.ok_or(LauncherError::GrantPreparation)?,
                    },
                    descriptor: Some(prepared.descriptor),
                });
            } else {
                records.push(PreparedRecord {
                    record: GrantRecord::Descriptor {
                        id: grant.id,
                        role: grant.role,
                        access: grant.access,
                        kind: prepared.kind,
                        identity: prepared.identity,
                        status_flags: prepared.status_flags,
                        block_device: prepared.block_device,
                    },
                    descriptor: Some(prepared.descriptor),
                });
            }
        }
        let record_count = records
            .len()
            .checked_add(2)
            .and_then(|count| u16::try_from(count).ok())
            .filter(|count| *count <= MAX_GRANT_RECORDS)
            .ok_or(LauncherError::GrantPreparation)?;
        records.insert(
            0,
            PreparedRecord {
                record: GrantRecord::Begin {
                    grant_count,
                    record_count,
                    bookmark_bytes,
                },
                descriptor: None,
            },
        );
        records.push(PreparedRecord {
            record: GrantRecord::Commit {
                grant_count,
                record_count,
                bookmark_bytes,
            },
            descriptor: None,
        });
        Ok(Self {
            batch,
            grant_count,
            records,
        })
    }

    #[cfg(test)]
    pub(crate) fn empty_for_test() -> Self {
        Self::prepare(Vec::new()).expect("an empty test grant batch must prepare")
    }

    pub(crate) fn batch(&self) -> BatchId {
        self.batch
    }

    pub(crate) fn grant_count(&self) -> u16 {
        self.grant_count
    }

    pub(crate) fn final_sequence(&self) -> u64 {
        u64::try_from(self.records.len().saturating_sub(1)).unwrap_or(u64::MAX)
    }

    pub(crate) fn outbound(&self, session: SessionId) -> Vec<OutboundGrant> {
        self.records
            .iter()
            .enumerate()
            .map(|(sequence, record)| OutboundGrant {
                frame: GrantFrame {
                    session,
                    batch: self.batch,
                    sequence: u64::try_from(sequence).unwrap_or(u64::MAX),
                    descriptor_count: record.record.descriptor_count(),
                    record: record.record.clone(),
                },
                descriptor: record.descriptor.as_ref().map(AsRawFd::as_raw_fd),
            })
            .collect()
    }

    /// Borrows the exact retained anchor for one singleton socket-directory role.
    pub(crate) fn socket_directory_anchor(
        &self,
        role: ResourceRole,
    ) -> Option<SocketDirectoryAnchor> {
        if !matches!(
            role,
            ResourceRole::ApiSocketDirectory | ResourceRole::VsockSocketDirectory
        ) {
            return None;
        }
        self.records
            .iter()
            .find_map(|prepared| match &prepared.record {
                GrantRecord::ScopedDirectory {
                    role: record_role,
                    access: GrantAccess::CreateChildren,
                    identity,
                    ..
                } if *record_role == role => prepared
                    .descriptor
                    .as_ref()
                    .map(AsRawFd::as_raw_fd)
                    .map(|descriptor| SocketDirectoryAnchor {
                        descriptor,
                        identity: *identity,
                    }),
                _ => None,
            })
    }

    /// Borrows one exact connect-only vhost-user directory anchor by grant ID.
    pub(crate) fn vhost_user_directory_anchor(
        &self,
        requested_id: &GrantId,
    ) -> Option<SocketDirectoryAnchor> {
        self.records
            .iter()
            .find_map(|prepared| match &prepared.record {
                GrantRecord::ScopedDirectory {
                    id,
                    role: ResourceRole::VhostUserSocketDirectory,
                    access: GrantAccess::ConnectChildren,
                    identity,
                    ..
                } if id == requested_id => prepared
                    .descriptor
                    .as_ref()
                    .map(AsRawFd::as_raw_fd)
                    .map(|descriptor| SocketDirectoryAnchor {
                        descriptor,
                        identity: *identity,
                    }),
                _ => None,
            })
    }

    /// Borrows the exact retained snapshot-output anchor for one recorded identity.
    pub(crate) fn snapshot_directory_anchor(
        &self,
        requested_identity: ObjectIdentity,
    ) -> Option<SnapshotDirectoryAnchor> {
        self.records
            .iter()
            .find_map(|prepared| match &prepared.record {
                GrantRecord::ScopedDirectory {
                    role: ResourceRole::SnapshotOutputDirectory,
                    access: GrantAccess::CreateChildren,
                    identity,
                    ..
                } if *identity == requested_identity => prepared
                    .descriptor
                    .as_ref()
                    .map(AsRawFd::as_raw_fd)
                    .map(|descriptor| SnapshotDirectoryAnchor {
                        descriptor,
                        identity: *identity,
                    }),
                _ => None,
            })
    }

    /// Borrows one exact retained block-special drive descriptor by grant ID.
    pub(crate) fn block_drive_anchor(&self, requested_id: &GrantId) -> Option<BlockDriveAnchor> {
        self.records
            .iter()
            .find_map(|prepared| match &prepared.record {
                GrantRecord::Descriptor {
                    id,
                    role: ResourceRole::DriveBacking,
                    access,
                    kind: GrantObjectKind::BlockDevice,
                    identity,
                    status_flags,
                    block_device: Some(block_device),
                } if id == requested_id => {
                    prepared
                        .descriptor
                        .as_ref()
                        .map(|descriptor| BlockDriveAnchor {
                            descriptor: descriptor.as_raw_fd(),
                            access: *access,
                            identity: *identity,
                            status_flags: *status_flags,
                            block_device: *block_device,
                        })
                }
                _ => None,
            })
    }
}

/// One borrowed outbound record. The owning batch must remain live while sent.
pub(crate) struct OutboundGrant {
    pub(crate) frame: GrantFrame,
    pub(crate) descriptor: Option<RawFd>,
}

impl std::fmt::Debug for OutboundGrant {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OutboundGrant")
            .field("frame", &self.frame)
            .field("descriptor", &self.descriptor.map(|_| "<borrowed>"))
            .finish()
    }
}

struct PreparedResource {
    descriptor: OwnedFd,
    kind: GrantObjectKind,
    identity: ObjectIdentity,
    source_identity: Option<ObjectIdentity>,
    status_flags: u32,
    block_device: Option<BlockDeviceGrant>,
    peer: Option<ConnectedUnixPeer>,
}

fn open_resource(grant: &ManifestGrant) -> Result<PreparedResource, LauncherError> {
    let components = resource_path_components(&grant.source)?;
    let mut descriptor = open_root_directory()?;
    for (index, component) in components.iter().enumerate() {
        let is_final = index + 1 == components.len();
        let flags = if is_final {
            resource_open_flags(grant)
        } else {
            libc::O_RDONLY
                | libc::O_DIRECTORY
                | libc::O_NOFOLLOW
                | libc::O_NONBLOCK
                | libc::O_CLOEXEC
        };
        // SAFETY: `descriptor` remains live, `component` is a NUL-terminated
        // single pathname component, and no creation mode is requested.
        let opened = unsafe { libc::openat(descriptor.as_raw_fd(), component.as_ptr(), flags) };
        if opened < 0 {
            return Err(LauncherError::GrantPreparation);
        }
        // SAFETY: `opened` is the fresh descriptor returned by openat.
        descriptor = unsafe { OwnedFd::from_raw_fd(opened) };
    }
    let stat = descriptor_stat(descriptor.as_raw_fd())?;
    let object_kind = stat.st_mode & libc::S_IFMT;
    let (kind, block_device) = if grant.role.is_scoped_directory() {
        if object_kind != libc::S_IFDIR {
            return Err(LauncherError::GrantPreparation);
        }
        (GrantObjectKind::Directory, None)
    } else {
        match object_kind {
            libc::S_IFREG => (GrantObjectKind::RegularFile, None),
            libc::S_IFBLK
                if grant.role == ResourceRole::DriveBacking
                    && matches!(grant.access, GrantAccess::ReadOnly | GrantAccess::ReadWrite) =>
            {
                let target_device = normalized_device(stat.st_rdev);
                if target_device == 0 {
                    return Err(LauncherError::GrantPreparation);
                }
                let block =
                    crate::macos::block_device::inspect(descriptor.as_raw_fd(), target_device)
                        .map_err(|_| LauncherError::GrantPreparation)?;
                (GrantObjectKind::BlockDevice, Some(block))
            }
            _ => return Err(LauncherError::GrantPreparation),
        }
    };
    // The nonblocking probe prevents a malicious special file from stalling
    // preparation. Regular files and directories remove it before recording;
    // Darwin block descriptors retain it because F_SETFL rejects the change.
    // SAFETY: F_GETFL inspects the live owned descriptor.
    let probe_flags = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFL) };
    if probe_flags < 0 {
        return Err(LauncherError::GrantPreparation);
    }
    if kind != GrantObjectKind::BlockDevice {
        // SAFETY: F_SETFL updates status flags on the same live descriptor.
        if unsafe {
            libc::fcntl(
                descriptor.as_raw_fd(),
                libc::F_SETFL,
                probe_flags & !libc::O_NONBLOCK,
            )
        } < 0
        {
            return Err(LauncherError::GrantPreparation);
        }
    }
    // SAFETY: F_GETFL reads status flags from the same live descriptor.
    let flags = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || !access_matches(flags, grant.access)
        || (kind == GrantObjectKind::BlockDevice && flags & libc::O_NONBLOCK == 0)
    {
        return Err(LauncherError::GrantPreparation);
    }
    let status_flags = if kind == GrantObjectKind::BlockDevice {
        bangbang_session::macos::normalized_block_status_flags(flags)
            .ok_or(LauncherError::GrantPreparation)?
    } else {
        u32::try_from(flags).map_err(|_| LauncherError::GrantPreparation)?
    };
    Ok(PreparedResource {
        descriptor,
        kind,
        identity: ObjectIdentity {
            device: normalized_device(stat.st_dev),
            inode: stat.st_ino,
        },
        source_identity: None,
        status_flags,
        block_device,
        peer: None,
    })
}

fn connect_resource(grant: &ManifestGrant) -> Result<PreparedResource, LauncherError> {
    if grant.role != ResourceRole::SnapshotPagerStream || grant.access != GrantAccess::ReadWrite {
        return Err(LauncherError::GrantPreparation);
    }
    let mut components = resource_path_components(&grant.source)?;
    let name = components.pop().ok_or(LauncherError::GrantPreparation)?;
    let mut anchor = open_root_directory()?;
    for component in components {
        // SAFETY: `anchor` remains live, `component` is one NUL-terminated
        // no-traversal component, and success returns a fresh descriptor.
        let opened = unsafe {
            libc::openat(
                anchor.as_raw_fd(),
                component.as_ptr(),
                libc::O_RDONLY
                    | libc::O_DIRECTORY
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK
                    | libc::O_CLOEXEC,
            )
        };
        if opened < 0 {
            return Err(LauncherError::GrantPreparation);
        }
        // SAFETY: `opened` is the fresh descriptor returned by openat.
        anchor = unsafe { OwnedFd::from_raw_fd(opened) };
    }
    let anchor_stat = descriptor_stat(anchor.as_raw_fd())?;
    if anchor_stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return Err(LauncherError::GrantPreparation);
    }
    let connected = crate::macos::local_socket::connect_anchored(
        anchor.as_raw_fd(),
        ObjectIdentity {
            device: normalized_device(anchor_stat.st_dev),
            inode: anchor_stat.st_ino,
        },
        &name,
        PAGER_CONNECT_TIMEOUT,
    )
    .map_err(|_| LauncherError::GrantPreparation)?;
    let source_identity = connected.source_identity();
    let stream = connected.into_stream();
    let peer = peer_identity(stream.as_raw_fd()).map_err(|_| LauncherError::GrantPreparation)?;
    // SAFETY: Effective identity calls have no pointer or ownership contract.
    let expected_uid = unsafe { libc::geteuid() };
    // SAFETY: Effective identity calls have no pointer or ownership contract.
    let expected_gid = unsafe { libc::getegid() };
    if peer.uid != expected_uid || peer.gid != expected_gid {
        return Err(LauncherError::GrantPreparation);
    }
    let process_id = u32::try_from(peer.pid).map_err(|_| LauncherError::GrantPreparation)?;
    let peer = ConnectedUnixPeer::new(peer.uid, peer.gid, process_id)
        .ok_or(LauncherError::GrantPreparation)?;
    let descriptor: OwnedFd = stream.into();
    let stat = descriptor_stat(descriptor.as_raw_fd())?;
    // SAFETY: F_GETFD and F_GETFL inspect the live connected stream.
    let descriptor_flags = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFD) };
    // SAFETY: F_GETFL inspects the same live stream.
    let flags = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFL) };
    if stat.st_mode & libc::S_IFMT != libc::S_IFSOCK
        || stat.st_ino == 0
        || descriptor_flags < 0
        || descriptor_flags & libc::FD_CLOEXEC == 0
        || flags < 0
        || flags & libc::O_ACCMODE != libc::O_RDWR
        || flags & libc::O_NONBLOCK == 0
    {
        return Err(LauncherError::GrantPreparation);
    }
    let status_flags = u32::try_from(flags & (libc::O_ACCMODE | libc::O_NONBLOCK))
        .map_err(|_| LauncherError::GrantPreparation)?;
    Ok(PreparedResource {
        descriptor,
        kind: GrantObjectKind::ConnectedUnixStream,
        identity: ObjectIdentity {
            device: normalized_device(stat.st_dev),
            inode: stat.st_ino,
        },
        source_identity: Some(source_identity),
        status_flags,
        block_device: None,
        peer: Some(peer),
    })
}

fn resource_path_components(path: &Path) -> Result<Vec<CString>, LauncherError> {
    let bytes = path.as_os_str().as_bytes();
    if bytes.first() != Some(&b'/') || bytes.len() > MAX_SOURCE_PATH_BYTES {
        return Err(LauncherError::InvalidGrantInput);
    }
    if bytes == b"/" {
        return Ok(Vec::new());
    }
    bytes
        .get(1..)
        .ok_or(LauncherError::InvalidGrantInput)?
        .split(|byte| *byte == b'/')
        .map(|component| {
            if component.is_empty() || matches!(component, b"." | b"..") {
                return Err(LauncherError::InvalidGrantInput);
            }
            CString::new(component).map_err(|_| LauncherError::InvalidGrantInput)
        })
        .collect()
}

fn open_root_directory() -> Result<OwnedFd, LauncherError> {
    // SAFETY: The static root path is NUL-terminated and open returns a fresh
    // descriptor on success.
    let descriptor = unsafe {
        libc::open(
            c"/".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        return Err(LauncherError::GrantPreparation);
    }
    // SAFETY: `descriptor` is the fresh result above.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
}

fn resource_open_flags(grant: &ManifestGrant) -> libc::c_int {
    let access = match grant.access {
        GrantAccess::ReadOnly | GrantAccess::CreateChildren | GrantAccess::ConnectChildren => {
            libc::O_RDONLY
        }
        GrantAccess::WriteOnly => libc::O_WRONLY,
        GrantAccess::ReadWrite => libc::O_RDWR,
    };
    let directory = if grant.role.is_scoped_directory() {
        libc::O_DIRECTORY
    } else {
        0
    };
    access | directory | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC
}

fn descriptor_stat(descriptor: RawFd) -> Result<libc::stat, LauncherError> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: stat is writable and descriptor remains live for the call.
    if unsafe { libc::fstat(descriptor, stat.as_mut_ptr()) } != 0 {
        return Err(LauncherError::GrantPreparation);
    }
    // SAFETY: successful fstat initialized the complete structure.
    Ok(unsafe { stat.assume_init() })
}

fn normalized_device(device: libc::dev_t) -> u64 {
    u64::from(u32::from_ne_bytes(device.to_ne_bytes()))
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

fn fragment_capacity(id: &GrantId) -> Result<usize, LauncherError> {
    MAX_GRANT_DATAGRAM_BYTES
        .checked_sub(GRANT_HEADER_BYTES)
        .and_then(|value| value.checked_sub(1 + id.as_bytes().len() + 4))
        .filter(|value| *value > 0)
        .ok_or(LauncherError::GrantPreparation)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "bangbang-grant-manifest-{}-{}",
                std::process::id(),
                NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).expect("test directory should create");
            Self(fs::canonicalize(path).expect("test directory should canonicalize"))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn manifest_grant(
        id: &str,
        role: ResourceRole,
        access: GrantAccess,
        source: PathBuf,
    ) -> ManifestGrant {
        ManifestGrant {
            id: GrantId::parse(id).expect("test grant ID should parse"),
            role,
            access,
            source,
        }
    }

    #[test]
    fn ordinary_arguments_remain_byte_preserved() {
        let opaque = OsString::from_vec(vec![0xff, 0xfe]);
        let input = LaunchInput::parse(vec![OsString::from("--version"), opaque.clone()])
            .expect("ordinary arguments should parse");
        assert_eq!(input.worker_args, vec![OsString::from("--version"), opaque]);
        assert!(input.manifest.is_none());
    }

    #[test]
    fn socket_directory_anchor_debug_redacts_descriptor_and_identity() {
        let anchor = SocketDirectoryAnchor {
            descriptor: 52,
            identity: ObjectIdentity {
                device: 53,
                inode: 59,
            },
        };
        let debug = format!("{anchor:?}");
        assert!(!debug.contains("52"));
        assert!(!debug.contains("53"));
        assert!(!debug.contains("59"));
    }

    #[test]
    fn snapshot_directory_anchor_is_selected_by_exact_granted_identity() {
        let root = TestDir::new();
        let state_path = root.path().join("state-output");
        let memory_path = root.path().join("memory-output");
        fs::create_dir(&state_path).expect("state output should create");
        fs::create_dir(&memory_path).expect("memory output should create");
        let state = manifest_grant(
            "state-output",
            ResourceRole::SnapshotOutputDirectory,
            GrantAccess::CreateChildren,
            state_path,
        );
        let memory = manifest_grant(
            "memory-output",
            ResourceRole::SnapshotOutputDirectory,
            GrantAccess::CreateChildren,
            memory_path,
        );
        let state_identity = open_resource(&state)
            .expect("state output should inspect")
            .identity;
        let memory_identity = open_resource(&memory)
            .expect("memory output should inspect")
            .identity;
        let batch = PreparedGrantBatch::prepare(vec![state, memory])
            .expect("snapshot outputs should prepare");

        let state_anchor = batch
            .snapshot_directory_anchor(state_identity)
            .expect("state anchor should be retained");
        let memory_anchor = batch
            .snapshot_directory_anchor(memory_identity)
            .expect("memory anchor should be retained");
        assert_eq!(state_anchor.identity(), state_identity);
        assert_eq!(memory_anchor.identity(), memory_identity);
        assert_ne!(state_anchor.descriptor(), memory_anchor.descriptor());
        assert!(
            batch
                .snapshot_directory_anchor(ObjectIdentity {
                    device: u64::MAX,
                    inode: u64::MAX,
                })
                .is_none()
        );
        let debug = format!("{state_anchor:?}");
        assert!(!debug.contains(&state_anchor.descriptor().to_string()));
        assert!(!debug.contains(&state_identity.device.to_string()));
        assert!(!debug.contains(&state_identity.inode.to_string()));
    }

    #[test]
    fn envelope_is_position_one_and_structurally_exact() {
        let input = LaunchInput::parse(vec![
            OsString::from(GRANT_OPTION),
            OsString::from("/private/tmp/manifest.json"),
            OsString::from(ENVELOPE_DELIMITER),
            OsString::from("--help"),
        ])
        .expect("valid envelope should parse");
        assert_eq!(input.worker_args, vec![OsString::from("--help")]);
        assert!(input.manifest.is_some());

        assert!(matches!(
            LaunchInput::parse(vec![OsString::from(GRANT_OPTION)]),
            Err(LauncherError::InvalidGrantInput)
        ));
    }

    #[test]
    fn manifest_enforces_roles_access_cardinality_and_bounds() {
        let valid = br#"{
            "version":1,
            "grants":[
                {"id":"kernel","role":"kernel-image","access":"read-only","source":"/private/tmp/kernel"},
                {"id":"drive.root","role":"drive-backing","access":"read-write","source":"/private/tmp/root"}
            ]
        }"#;
        assert_eq!(
            parse_manifest(valid).expect("manifest should parse").len(),
            2
        );

        let duplicate = br#"{
            "version":1,
            "grants":[
                {"id":"one","role":"kernel-image","access":"read-only","source":"/private/tmp/one"},
                {"id":"two","role":"kernel-image","access":"read-only","source":"/private/tmp/two"}
            ]
        }"#;
        assert!(matches!(
            parse_manifest(duplicate),
            Err(LauncherError::InvalidGrantInput)
        ));

        let wrong_access = br#"{
            "version":1,
            "grants":[
                {"id":"kernel","role":"kernel-image","access":"read-write","source":"/private/tmp/kernel"}
            ]
        }"#;
        assert!(matches!(
            parse_manifest(wrong_access),
            Err(LauncherError::InvalidGrantInput)
        ));

        let snapshot_outputs = br#"{
            "version":1,
            "grants":[
                {"id":"state-output","role":"snapshot-output-directory","access":"create-children","source":"/private/tmp/state"},
                {"id":"memory-output","role":"snapshot-output-directory","access":"create-children","source":"/private/tmp/memory"}
            ]
        }"#;
        assert_eq!(
            parse_manifest(snapshot_outputs)
                .expect("snapshot output role should be repeatable")
                .len(),
            2
        );

        let vhost_directories = br#"{
            "version":1,
            "grants":[
                {"id":"vhost-one","role":"vhost-user-socket-directory","access":"connect-children","source":"/private/tmp/vhost-one"},
                {"id":"vhost-two","role":"vhost-user-socket-directory","access":"connect-children","source":"/private/tmp/vhost-two"}
            ]
        }"#;
        assert_eq!(
            parse_manifest(vhost_directories)
                .expect("vhost-user directory role should be repeatable")
                .len(),
            2
        );

        let writable_vhost_directory = br#"{
            "version":1,
            "grants":[
                {"id":"vhost","role":"vhost-user-socket-directory","access":"create-children","source":"/private/tmp/vhost"}
            ]
        }"#;
        assert!(matches!(
            parse_manifest(writable_vhost_directory),
            Err(LauncherError::InvalidGrantInput)
        ));

        let duplicate_snapshot_input = br#"{
            "version":1,
            "grants":[
                {"id":"state-one","role":"snapshot-state-input","access":"read-only","source":"/private/tmp/state-one"},
                {"id":"state-two","role":"snapshot-state-input","access":"read-only","source":"/private/tmp/state-two"}
            ]
        }"#;
        assert!(matches!(
            parse_manifest(duplicate_snapshot_input),
            Err(LauncherError::InvalidGrantInput)
        ));
    }

    #[test]
    fn vhost_user_directory_anchor_is_selected_by_exact_grant_id() {
        let root = TestDir::new();
        let first_path = root.path().join("vhost-one");
        let second_path = root.path().join("vhost-two");
        fs::create_dir(&first_path).expect("first vhost directory should create");
        fs::create_dir(&second_path).expect("second vhost directory should create");
        let first_id = GrantId::parse("vhost-one").expect("first ID should parse");
        let second_id = GrantId::parse("vhost-two").expect("second ID should parse");
        let batch = PreparedGrantBatch::prepare(vec![
            manifest_grant(
                "vhost-one",
                ResourceRole::VhostUserSocketDirectory,
                GrantAccess::ConnectChildren,
                first_path,
            ),
            manifest_grant(
                "vhost-two",
                ResourceRole::VhostUserSocketDirectory,
                GrantAccess::ConnectChildren,
                second_path,
            ),
        ])
        .expect("vhost directory grants should prepare");

        let first = batch
            .vhost_user_directory_anchor(&first_id)
            .expect("first exact anchor should exist");
        let second = batch
            .vhost_user_directory_anchor(&second_id)
            .expect("second exact anchor should exist");
        assert_ne!(first.identity(), second.identity());
        assert_ne!(first.descriptor(), second.descriptor());
        assert!(
            batch
                .vhost_user_directory_anchor(
                    &GrantId::parse("missing-vhost").expect("missing ID should parse")
                )
                .is_none()
        );
    }

    #[test]
    fn manifest_rejects_ambiguous_paths_unknown_fields_and_trailing_data() {
        let root = TestDir::new();
        let parent = format!("{}/child/../resource", root.path().display());
        let repeated = format!("{}//resource", root.path().display());
        for source in [parent, repeated] {
            let manifest = serde_json::json!({
                "version": 1,
                "grants": [{
                    "id": "kernel",
                    "role": "kernel-image",
                    "access": "read-only",
                    "source": source,
                }]
            });
            assert!(matches!(
                parse_manifest(&serde_json::to_vec(&manifest).expect("fixture should serialize")),
                Err(LauncherError::InvalidGrantInput)
            ));
        }

        let unknown = r#"{"version":1,"unknown":true,"grants":[]} trailing"#;
        assert!(matches!(
            parse_manifest(unknown.as_bytes()),
            Err(LauncherError::InvalidGrantInput)
        ));
    }

    #[test]
    fn manifest_accepts_exact_count_and_path_limits_then_rejects_one_over() {
        let grants = (0..usize::from(MAX_GRANTS))
            .map(|index| {
                serde_json::json!({
                    "id": format!("drive-{index}"),
                    "role": "drive-backing",
                    "access": "read-only",
                    "source": format!("/private/tmp/drive-{index}"),
                })
            })
            .collect::<Vec<_>>();
        let manifest = serde_json::json!({"version": 1, "grants": grants});
        assert_eq!(
            parse_manifest(&serde_json::to_vec(&manifest).expect("fixture should serialize"))
                .expect("exact grant count should parse")
                .len(),
            usize::from(MAX_GRANTS)
        );

        let mut excessive = manifest;
        excessive["grants"]
            .as_array_mut()
            .expect("grants should be an array")
            .push(serde_json::json!({
                "id": "drive-over",
                "role": "drive-backing",
                "access": "read-only",
                "source": "/private/tmp/drive-over",
            }));
        assert!(matches!(
            parse_manifest(
                &serde_json::to_vec(&excessive).expect("excessive fixture should serialize")
            ),
            Err(LauncherError::InvalidGrantInput)
        ));

        for (length, accepted) in [
            (MAX_SOURCE_PATH_BYTES, true),
            (MAX_SOURCE_PATH_BYTES + 1, false),
        ] {
            let source = format!("/{}", "a".repeat(length - 1));
            let manifest = serde_json::json!({
                "version": 1,
                "grants": [{
                    "id": "kernel",
                    "role": "kernel-image",
                    "access": "read-only",
                    "source": source,
                }]
            });
            assert_eq!(
                parse_manifest(
                    &serde_json::to_vec(&manifest).expect("path fixture should serialize")
                )
                .is_ok(),
                accepted
            );
        }
    }

    #[test]
    fn safe_open_rejects_symlinks_types_missing_resources_and_aliases() {
        let root = TestDir::new();
        let regular = root.path().join("regular");
        let directory = root.path().join("directory");
        let missing = root.path().join("missing");
        fs::write(&regular, b"fixture").expect("regular fixture should write");
        fs::create_dir(&directory).expect("directory fixture should create");

        let opened = open_resource(&manifest_grant(
            "kernel",
            ResourceRole::KernelImage,
            GrantAccess::ReadOnly,
            regular.clone(),
        ))
        .expect("regular resource should open");
        // SAFETY: F_GETFL inspects the live prepared descriptor.
        let flags = unsafe { libc::fcntl(opened.descriptor.as_raw_fd(), libc::F_GETFL) };
        assert_eq!(flags & libc::O_ACCMODE, libc::O_RDONLY);
        assert_eq!(flags & libc::O_NONBLOCK, 0);

        assert!(
            open_resource(&manifest_grant(
                "wrong-type",
                ResourceRole::KernelImage,
                GrantAccess::ReadOnly,
                directory.clone(),
            ))
            .is_err()
        );
        assert!(
            open_resource(&manifest_grant(
                "missing",
                ResourceRole::LoggerSink,
                GrantAccess::WriteOnly,
                missing.clone(),
            ))
            .is_err()
        );
        assert!(!missing.exists(), "preparation must not create a resource");

        let final_link = root.path().join("final-link");
        symlink(&regular, &final_link).expect("final symlink should create");
        assert!(
            open_resource(&manifest_grant(
                "final-link",
                ResourceRole::KernelImage,
                GrantAccess::ReadOnly,
                final_link,
            ))
            .is_err()
        );

        let component_link = root.path().join("component-link");
        symlink(&directory, &component_link).expect("component symlink should create");
        let nested = component_link.join("nested");
        fs::write(directory.join("nested"), b"nested").expect("nested fixture should write");
        assert!(
            open_resource(&manifest_grant(
                "component-link",
                ResourceRole::KernelImage,
                GrantAccess::ReadOnly,
                nested,
            ))
            .is_err()
        );

        let alias = root.path().join("alias");
        fs::hard_link(&regular, &alias).expect("hard-link alias should create");
        assert!(
            PreparedGrantBatch::prepare(vec![
                manifest_grant(
                    "drive-one",
                    ResourceRole::DriveBacking,
                    GrantAccess::ReadOnly,
                    regular,
                ),
                manifest_grant(
                    "drive-two",
                    ResourceRole::DriveBacking,
                    GrantAccess::ReadOnly,
                    alias,
                ),
            ])
            .is_err()
        );
    }

    #[test]
    fn pager_stream_preparation_connects_exactly_and_records_redacted_identity() {
        let root = TestDir::new();
        let socket = root.path().join("pager.sock");
        let listener = UnixListener::bind(&socket).expect("pager listener should bind");
        let batch = PreparedGrantBatch::prepare(vec![manifest_grant(
            "pager",
            ResourceRole::SnapshotPagerStream,
            GrantAccess::ReadWrite,
            socket.clone(),
        )])
        .expect("pager stream grant should prepare");
        let (mut accepted, _) = listener.accept().expect("pager stream should connect");
        assert_eq!(batch.grant_count(), 1);
        let prepared = batch
            .records
            .iter()
            .find(|record| matches!(record.record, GrantRecord::ConnectedStream { .. }))
            .expect("connected stream record should exist");
        let GrantRecord::ConnectedStream {
            role,
            access,
            identity,
            source_identity,
            status_flags,
            peer,
            ..
        } = &prepared.record
        else {
            panic!("record should be connected stream");
        };
        assert_eq!(*role, ResourceRole::SnapshotPagerStream);
        assert_eq!(*access, GrantAccess::ReadWrite);
        assert_ne!(identity.inode, 0);
        assert_ne!(source_identity.inode, 0);
        assert_eq!(
            *status_flags,
            u32::try_from(libc::O_RDWR | libc::O_NONBLOCK).expect("flags should fit")
        );
        // SAFETY: Effective identity calls have no pointer or ownership contract.
        assert_eq!(peer.user_id(), unsafe { libc::geteuid() });
        // SAFETY: Effective identity calls have no pointer or ownership contract.
        assert_eq!(peer.group_id(), unsafe { libc::getegid() });
        assert_eq!(peer.process_id(), std::process::id());
        let descriptor = prepared
            .descriptor
            .as_ref()
            .expect("connected stream descriptor should remain owned");
        // SAFETY: F_GETFD inspects the live retained descriptor.
        let flags = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
        fs::remove_file(&socket).expect("original socket name should unlink");
        let _replacement =
            UnixListener::bind(&socket).expect("replacement socket name should bind");
        let marker = [0x5a_u8];
        // SAFETY: The retained descriptor remains connected to `accepted`, and
        // the one-byte marker is readable for the complete synchronous write.
        let written =
            unsafe { libc::write(descriptor.as_raw_fd(), marker.as_ptr().cast(), marker.len()) };
        assert_eq!(written, 1);
        let mut observed = [0_u8; 1];
        accepted
            .read_exact(&mut observed)
            .expect("prepared stream should survive pathname replacement");
        assert_eq!(observed, marker);
        let debug = format!("{batch:?} {prepared:?}");
        assert!(!debug.contains(path_text_for_test(&socket)));
        assert!(!debug.contains("pager"));
    }

    #[test]
    fn pager_stream_preparation_rejects_missing_refused_regular_and_symlink_targets() {
        let root = TestDir::new();
        let missing = root.path().join("missing.sock");
        let regular = root.path().join("regular.sock");
        fs::write(&regular, b"not a socket").expect("regular fixture should write");
        for (id, path) in [("missing", missing), ("regular", regular.clone())] {
            assert!(
                connect_resource(&manifest_grant(
                    id,
                    ResourceRole::SnapshotPagerStream,
                    GrantAccess::ReadWrite,
                    path,
                ))
                .is_err()
            );
        }
        let refused = root.path().join("refused.sock");
        drop(UnixListener::bind(&refused).expect("refused listener should bind then close"));
        assert!(
            connect_resource(&manifest_grant(
                "refused",
                ResourceRole::SnapshotPagerStream,
                GrantAccess::ReadWrite,
                refused,
            ))
            .is_err()
        );
        let target = root.path().join("target.sock");
        let _listener = UnixListener::bind(&target).expect("target listener should bind");
        let link = root.path().join("link.sock");
        symlink(&target, &link).expect("socket symlink should create");
        assert!(
            connect_resource(&manifest_grant(
                "link",
                ResourceRole::SnapshotPagerStream,
                GrantAccess::ReadWrite,
                link,
            ))
            .is_err()
        );
    }

    fn path_text_for_test(path: &Path) -> &str {
        path.to_str().expect("test path should be UTF-8")
    }

    #[test]
    fn manifest_file_is_opened_once_without_following_its_final_symlink() {
        let root = TestDir::new();
        let manifest = root.path().join("manifest.json");
        let link = root.path().join("manifest-link.json");
        fs::write(&manifest, br#"{"version":1,"grants":[]}"#)
            .expect("manifest fixture should write");
        symlink(&manifest, &link).expect("manifest link should create");
        assert!(matches!(
            load_manifest(&link),
            Err(LauncherError::InvalidGrantInput)
        ));
        assert!(load_manifest(&manifest).is_ok());
    }

    #[test]
    fn prepared_regular_descriptor_survives_later_path_replacement() {
        let root = TestDir::new();
        let source = root.path().join("replaceable");
        let old_name = root.path().join("old-object");
        fs::write(&source, b"old").expect("original fixture should write");
        let prepared = open_resource(&manifest_grant(
            "kernel",
            ResourceRole::KernelImage,
            GrantAccess::ReadOnly,
            source.clone(),
        ))
        .expect("original descriptor should prepare");
        fs::rename(&source, &old_name).expect("original fixture should move");
        fs::write(&source, b"new").expect("replacement fixture should write");
        let replacement = open_resource(&manifest_grant(
            "replacement",
            ResourceRole::KernelImage,
            GrantAccess::ReadOnly,
            source,
        ))
        .expect("replacement descriptor should prepare");
        assert_ne!(prepared.identity, replacement.identity);
        let mut bytes = [0_u8; 3];
        // SAFETY: The buffer is writable and the prepared descriptor remains
        // live after its original pathname was replaced.
        let length = unsafe {
            libc::pread(
                prepared.descriptor.as_raw_fd(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
                0,
            )
        };
        assert_eq!(usize::try_from(length).ok(), Some(bytes.len()));
        assert_eq!(&bytes, b"old");
    }
}
