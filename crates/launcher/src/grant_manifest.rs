use std::collections::HashSet;
use std::ffi::{CString, OsStr, OsString};
use std::fs::OpenOptions;
#[cfg(feature = "elevated-bootstrap-probe")]
use std::io;
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
const CONNECTED_STREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

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
        "vmnet-provider-stream" => Ok(ResourceRole::VmnetProviderStream),
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
    #[cfg(feature = "elevated-bootstrap-probe")]
    evidence_readback: Option<OwnedFd>,
}

impl std::fmt::Debug for PreparedRecord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = formatter.debug_struct("PreparedRecord");
        debug
            .field("record", &self.record)
            .field("descriptor", &self.descriptor.as_ref().map(|_| "<owned>"));
        #[cfg(feature = "elevated-bootstrap-probe")]
        debug.field(
            "evidence_readback",
            &self.evidence_readback.as_ref().map(|_| "<owned>"),
        );
        debug.finish()
    }
}

/// Fully opened, failure-atomic launcher batch.
pub(crate) struct PreparedGrantBatch {
    batch: BatchId,
    grant_count: u16,
    records: Vec<PreparedRecord>,
}

#[cfg(feature = "elevated-bootstrap-probe")]
#[derive(Clone, Copy)]
pub(crate) struct ElevatedGuestContract {
    workload: bangbang_session::elevated_probe::RuntimeWorkload,
    api_anchor: Option<SocketDirectoryAnchor>,
    serial_evidence_descriptor: RawFd,
    serial_evidence_identity: ObjectIdentity,
}

#[cfg(feature = "elevated-bootstrap-probe")]
impl std::fmt::Debug for ElevatedGuestContract {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ElevatedGuestContract(<redacted>)")
    }
}

#[cfg(feature = "elevated-bootstrap-probe")]
impl ElevatedGuestContract {
    pub(crate) const fn workload(self) -> bangbang_session::elevated_probe::RuntimeWorkload {
        self.workload
    }

    pub(crate) const fn api_anchor(self) -> Option<SocketDirectoryAnchor> {
        self.api_anchor
    }

    pub(crate) const fn serial_evidence_descriptor(self) -> RawFd {
        self.serial_evidence_descriptor
    }

    pub(crate) const fn serial_evidence_identity(self) -> ObjectIdentity {
        self.serial_evidence_identity
    }
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
            let prepared = if grant.role.is_connected_stream() {
                connect_resource(&grant)?
            } else {
                open_resource(&grant)?
            };
            #[cfg(feature = "elevated-bootstrap-probe")]
            let evidence_readback = if grant.role == ResourceRole::SerialSink
                && grant.access == GrantAccess::WriteOnly
            {
                Some(open_evidence_readback(&grant, prepared.identity)?)
            } else {
                None
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
                    #[cfg(feature = "elevated-bootstrap-probe")]
                    evidence_readback,
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
                        #[cfg(feature = "elevated-bootstrap-probe")]
                        evidence_readback: None,
                    });
                }
            } else if grant.role.is_connected_stream() {
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
                    #[cfg(feature = "elevated-bootstrap-probe")]
                    evidence_readback,
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
                    #[cfg(feature = "elevated-bootstrap-probe")]
                    evidence_readback,
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
                #[cfg(feature = "elevated-bootstrap-probe")]
                evidence_readback: None,
            },
        );
        records.push(PreparedRecord {
            record: GrantRecord::Commit {
                grant_count,
                record_count,
                bookmark_bytes,
            },
            descriptor: None,
            #[cfg(feature = "elevated-bootstrap-probe")]
            evidence_readback: None,
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

    #[cfg(feature = "elevated-bootstrap-probe")]
    pub(crate) fn validate_elevated_runtime_contract(
        &self,
        workload: bangbang_session::elevated_probe::RuntimeWorkload,
        target_uid: u32,
        target_gid: u32,
    ) -> Result<Option<ElevatedGuestContract>, LauncherError> {
        self.validate_elevated_runtime_contract_inner(workload, target_uid, target_gid, true)
    }

    #[cfg(all(feature = "elevated-bootstrap-probe", test))]
    fn validate_elevated_runtime_contract_for_test(
        &self,
        workload: bangbang_session::elevated_probe::RuntimeWorkload,
        target_uid: u32,
        target_gid: u32,
    ) -> Result<Option<ElevatedGuestContract>, LauncherError> {
        self.validate_elevated_runtime_contract_inner(workload, target_uid, target_gid, false)
    }

    #[cfg(feature = "elevated-bootstrap-probe")]
    fn validate_elevated_runtime_contract_inner(
        &self,
        workload: bangbang_session::elevated_probe::RuntimeWorkload,
        target_uid: u32,
        target_gid: u32,
        validate_resource_seals: bool,
    ) -> Result<Option<ElevatedGuestContract>, LauncherError> {
        use bangbang_session::elevated_probe::{
            GUEST_API_DIRECTORY_GRANT_ID, GUEST_CONFIG_GRANT_ID, GUEST_INITRD_GRANT_ID,
            GUEST_KERNEL_GRANT_ID, GUEST_LOGGER_GRANT_ID, GUEST_METRICS_GRANT_ID,
            GUEST_ROOTFS_GRANT_ID, GUEST_SERIAL_GRANT_ID, RuntimeWorkload,
        };

        const CONFIG_SEAL: ElevatedResourceSeal = ElevatedResourceSeal::new(
            655,
            [
                0x19, 0x6f, 0xc8, 0xe9, 0x22, 0xc2, 0x20, 0x81, 0x7f, 0x2d, 0x30, 0x97, 0xb2, 0x5d,
                0xe2, 0x43, 0xe3, 0xfd, 0xcf, 0xd6, 0xe1, 0xe5, 0xb7, 0xaf, 0x22, 0x33, 0x71, 0xe3,
                0x53, 0x10, 0x96, 0xa6,
            ],
        );
        const KERNEL_SEAL: ElevatedResourceSeal = ElevatedResourceSeal::new(
            17_111_552,
            [
                0xe3, 0x54, 0x4b, 0x10, 0x60, 0x3a, 0xcb, 0xf3, 0xdb, 0x49, 0x2c, 0xb5, 0x2e, 0x00,
                0x0d, 0x22, 0xba, 0x20, 0x2c, 0xb4, 0xb6, 0x3b, 0x9a, 0xdd, 0x02, 0x75, 0x65, 0x68,
                0x3e, 0x11, 0xc5, 0x91,
            ],
        );
        const INITRD_SEAL: ElevatedResourceSeal = ElevatedResourceSeal::new(
            54_272,
            [
                0x10, 0x57, 0x07, 0x9b, 0x07, 0x24, 0x52, 0xa7, 0x62, 0x39, 0x61, 0x13, 0x86, 0x7e,
                0xbc, 0x5a, 0xfa, 0x69, 0x9a, 0x0b, 0x5c, 0x31, 0x21, 0xe2, 0x89, 0x70, 0xec, 0xad,
                0xd4, 0xba, 0x11, 0xd0,
            ],
        );
        const ROOTFS_SEAL: ElevatedResourceSeal = ElevatedResourceSeal::new(
            105_332_736,
            [
                0x0e, 0xfb, 0x6a, 0x3f, 0xf2, 0x98, 0x2b, 0xaa, 0x6c, 0xa7, 0xe3, 0xd9, 0x40, 0x96,
                0x65, 0x16, 0xba, 0x7d, 0xdd, 0x2d, 0xf5, 0xde, 0xb3, 0xe6, 0xc2, 0x16, 0x1d, 0x36,
                0x9a, 0x15, 0xd6, 0x08,
            ],
        );

        const REPRESENTATIVE: &[ElevatedGrantSpec] = &[
            ElevatedGrantSpec::file(
                "probe-read-target-runtime",
                ResourceRole::KernelImage,
                GrantAccess::ReadOnly,
            ),
            ElevatedGrantSpec::file(
                "probe-write-target-runtime",
                ResourceRole::LoggerSink,
                GrantAccess::WriteOnly,
            ),
            ElevatedGrantSpec::directory(
                "probe-dir-target-runtime",
                ResourceRole::ApiSocketDirectory,
                GrantAccess::CreateChildren,
            ),
        ];
        const NO_API: &[ElevatedGrantSpec] = &[
            ElevatedGrantSpec::sealed_file(
                GUEST_CONFIG_GRANT_ID,
                ResourceRole::StartupConfig,
                GrantAccess::ReadOnly,
                CONFIG_SEAL,
            ),
            ElevatedGrantSpec::sealed_file(
                GUEST_KERNEL_GRANT_ID,
                ResourceRole::KernelImage,
                GrantAccess::ReadOnly,
                KERNEL_SEAL,
            ),
            ElevatedGrantSpec::sealed_file(
                GUEST_INITRD_GRANT_ID,
                ResourceRole::InitrdImage,
                GrantAccess::ReadOnly,
                INITRD_SEAL,
            ),
            ElevatedGrantSpec::sealed_file(
                GUEST_ROOTFS_GRANT_ID,
                ResourceRole::DriveBacking,
                GrantAccess::ReadOnly,
                ROOTFS_SEAL,
            ),
            ElevatedGrantSpec::file(
                GUEST_LOGGER_GRANT_ID,
                ResourceRole::LoggerSink,
                GrantAccess::WriteOnly,
            ),
            ElevatedGrantSpec::file(
                GUEST_METRICS_GRANT_ID,
                ResourceRole::MetricsSink,
                GrantAccess::WriteOnly,
            ),
            ElevatedGrantSpec::file(
                GUEST_SERIAL_GRANT_ID,
                ResourceRole::SerialSink,
                GrantAccess::WriteOnly,
            ),
        ];
        const API: &[ElevatedGrantSpec] = &[
            ElevatedGrantSpec::directory(
                GUEST_API_DIRECTORY_GRANT_ID,
                ResourceRole::ApiSocketDirectory,
                GrantAccess::CreateChildren,
            ),
            ElevatedGrantSpec::sealed_file(
                GUEST_KERNEL_GRANT_ID,
                ResourceRole::KernelImage,
                GrantAccess::ReadOnly,
                KERNEL_SEAL,
            ),
            ElevatedGrantSpec::sealed_file(
                GUEST_INITRD_GRANT_ID,
                ResourceRole::InitrdImage,
                GrantAccess::ReadOnly,
                INITRD_SEAL,
            ),
            ElevatedGrantSpec::sealed_file(
                GUEST_ROOTFS_GRANT_ID,
                ResourceRole::DriveBacking,
                GrantAccess::ReadOnly,
                ROOTFS_SEAL,
            ),
            ElevatedGrantSpec::file(
                GUEST_LOGGER_GRANT_ID,
                ResourceRole::LoggerSink,
                GrantAccess::WriteOnly,
            ),
            ElevatedGrantSpec::file(
                GUEST_METRICS_GRANT_ID,
                ResourceRole::MetricsSink,
                GrantAccess::WriteOnly,
            ),
            ElevatedGrantSpec::file(
                GUEST_SERIAL_GRANT_ID,
                ResourceRole::SerialSink,
                GrantAccess::WriteOnly,
            ),
        ];

        let (specs, strict_guest) = match workload {
            RuntimeWorkload::RepresentativeGrants => (REPRESENTATIVE, false),
            RuntimeWorkload::GuestNoApi => (NO_API, true),
            RuntimeWorkload::GuestApi => (API, true),
        };
        self.validate_elevated_specs(
            specs,
            strict_guest,
            validate_resource_seals,
            target_uid,
            target_gid,
        )?;
        if workload == RuntimeWorkload::RepresentativeGrants {
            return Ok(None);
        }
        let (serial_evidence_descriptor, serial_evidence_identity) = self
            .elevated_evidence_readback(GUEST_SERIAL_GRANT_ID)
            .ok_or(LauncherError::InvalidLaunchPolicy)?;
        if self
            .records
            .iter()
            .filter(|prepared| prepared.evidence_readback.is_some())
            .count()
            != 1
        {
            return Err(LauncherError::InvalidLaunchPolicy);
        }
        let api_anchor = if workload == RuntimeWorkload::GuestApi {
            Some(
                self.socket_directory_anchor(ResourceRole::ApiSocketDirectory)
                    .ok_or(LauncherError::InvalidLaunchPolicy)?,
            )
        } else {
            if self
                .socket_directory_anchor(ResourceRole::ApiSocketDirectory)
                .is_some()
            {
                return Err(LauncherError::InvalidLaunchPolicy);
            }
            None
        };
        Ok(Some(ElevatedGuestContract {
            workload,
            api_anchor,
            serial_evidence_descriptor,
            serial_evidence_identity,
        }))
    }

    #[cfg(feature = "elevated-bootstrap-probe")]
    fn elevated_evidence_readback(&self, expected_id: &str) -> Option<(RawFd, ObjectIdentity)> {
        let expected_id = GrantId::parse(expected_id).ok()?;
        self.records.iter().find_map(|prepared| {
            let GrantRecord::Descriptor {
                id,
                role: ResourceRole::SerialSink,
                access: GrantAccess::WriteOnly,
                kind: GrantObjectKind::RegularFile,
                identity,
                block_device: None,
                ..
            } = &prepared.record
            else {
                return None;
            };
            if id != &expected_id {
                return None;
            }
            Some((prepared.evidence_readback.as_ref()?.as_raw_fd(), *identity))
        })
    }

    #[cfg(feature = "elevated-bootstrap-probe")]
    fn validate_elevated_specs(
        &self,
        specs: &[ElevatedGrantSpec],
        strict_guest: bool,
        validate_resource_seals: bool,
        target_uid: u32,
        target_gid: u32,
    ) -> Result<(), LauncherError> {
        if usize::from(self.grant_count) != specs.len() {
            return Err(LauncherError::InvalidLaunchPolicy);
        }
        let semantic_records = self
            .records
            .iter()
            .filter(|prepared| {
                matches!(
                    prepared.record,
                    GrantRecord::Descriptor { .. }
                        | GrantRecord::ConnectedStream { .. }
                        | GrantRecord::ScopedDirectory { .. }
                )
            })
            .collect::<Vec<_>>();
        if semantic_records.len() != specs.len() {
            return Err(LauncherError::InvalidLaunchPolicy);
        }
        for spec in specs {
            let expected_id =
                GrantId::parse(spec.id).map_err(|_| LauncherError::InvalidLaunchPolicy)?;
            let prepared = semantic_records
                .iter()
                .copied()
                .find(|prepared| match &prepared.record {
                    GrantRecord::Descriptor { id, .. }
                    | GrantRecord::ConnectedStream { id, .. }
                    | GrantRecord::ScopedDirectory { id, .. } => id == &expected_id,
                    GrantRecord::Begin { .. }
                    | GrantRecord::BookmarkFragment { .. }
                    | GrantRecord::Commit { .. } => false,
                })
                .ok_or(LauncherError::InvalidLaunchPolicy)?;
            match (&prepared.record, spec.kind) {
                (
                    GrantRecord::Descriptor {
                        role,
                        access,
                        kind: GrantObjectKind::RegularFile,
                        identity,
                        status_flags,
                        block_device: None,
                        ..
                    },
                    ElevatedGrantKind::File,
                ) if *role == spec.role && *access == spec.access => {
                    let descriptor = prepared
                        .descriptor
                        .as_ref()
                        .ok_or(LauncherError::InvalidLaunchPolicy)?;
                    validate_elevated_regular_file(
                        descriptor.as_raw_fd(),
                        *identity,
                        *status_flags,
                        ElevatedRegularFilePolicy {
                            access: spec.access,
                            strict_guest,
                            seal: validate_resource_seals.then_some(spec.seal).flatten(),
                            target_uid,
                            target_gid,
                        },
                    )?;
                }
                (
                    GrantRecord::ScopedDirectory {
                        role,
                        access,
                        identity,
                        bookmark_bytes,
                        fragment_count,
                        ..
                    },
                    ElevatedGrantKind::Directory,
                ) if *role == spec.role
                    && *access == spec.access
                    && *bookmark_bytes > 0
                    && *fragment_count > 0 =>
                {
                    let descriptor = prepared
                        .descriptor
                        .as_ref()
                        .ok_or(LauncherError::InvalidLaunchPolicy)?;
                    validate_elevated_directory(
                        descriptor.as_raw_fd(),
                        *identity,
                        strict_guest,
                        target_uid,
                        target_gid,
                    )?;
                }
                _ => return Err(LauncherError::InvalidLaunchPolicy),
            }
        }
        Ok(())
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

    /// Returns whether the exact singleton remote-provider authority is retained.
    pub(crate) fn has_vmnet_provider_stream(&self) -> bool {
        self.records
            .iter()
            .filter(|prepared| {
                matches!(
                    &prepared.record,
                    GrantRecord::ConnectedStream {
                        role: ResourceRole::VmnetProviderStream,
                        access: GrantAccess::ReadWrite,
                        ..
                    }
                )
            })
            .count()
            == 1
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

#[cfg(feature = "elevated-bootstrap-probe")]
#[derive(Clone, Copy)]
enum ElevatedGrantKind {
    File,
    Directory,
}

#[cfg(feature = "elevated-bootstrap-probe")]
#[derive(Clone, Copy)]
struct ElevatedResourceSeal {
    size_bytes: u64,
    sha256: [u8; 32],
}

#[cfg(feature = "elevated-bootstrap-probe")]
impl ElevatedResourceSeal {
    const fn new(size_bytes: u64, sha256: [u8; 32]) -> Self {
        Self { size_bytes, sha256 }
    }
}

#[cfg(feature = "elevated-bootstrap-probe")]
#[derive(Clone, Copy)]
struct ElevatedGrantSpec {
    id: &'static str,
    role: ResourceRole,
    access: GrantAccess,
    kind: ElevatedGrantKind,
    seal: Option<ElevatedResourceSeal>,
}

#[cfg(feature = "elevated-bootstrap-probe")]
#[derive(Clone, Copy)]
struct ElevatedRegularFilePolicy {
    access: GrantAccess,
    strict_guest: bool,
    seal: Option<ElevatedResourceSeal>,
    target_uid: u32,
    target_gid: u32,
}

#[cfg(feature = "elevated-bootstrap-probe")]
impl ElevatedGrantSpec {
    const fn file(id: &'static str, role: ResourceRole, access: GrantAccess) -> Self {
        Self {
            id,
            role,
            access,
            kind: ElevatedGrantKind::File,
            seal: None,
        }
    }

    const fn sealed_file(
        id: &'static str,
        role: ResourceRole,
        access: GrantAccess,
        seal: ElevatedResourceSeal,
    ) -> Self {
        Self {
            id,
            role,
            access,
            kind: ElevatedGrantKind::File,
            seal: Some(seal),
        }
    }

    const fn directory(id: &'static str, role: ResourceRole, access: GrantAccess) -> Self {
        Self {
            id,
            role,
            access,
            kind: ElevatedGrantKind::Directory,
            seal: None,
        }
    }
}

#[cfg(feature = "elevated-bootstrap-probe")]
fn validate_elevated_regular_file(
    descriptor: RawFd,
    expected_identity: ObjectIdentity,
    expected_status_flags: u32,
    policy: ElevatedRegularFilePolicy,
) -> Result<(), LauncherError> {
    let stat = descriptor_stat(descriptor)?;
    let status = libc::c_int::try_from(expected_status_flags)
        .ok()
        .and_then(bangbang_session::macos::normalized_regular_file_status_flags);
    let expected_access = u32::try_from(match policy.access {
        GrantAccess::ReadOnly => libc::O_RDONLY,
        GrantAccess::WriteOnly => libc::O_WRONLY,
        GrantAccess::ReadWrite => libc::O_RDWR,
        GrantAccess::CreateChildren | GrantAccess::ConnectChildren => {
            return Err(LauncherError::InvalidLaunchPolicy);
        }
    })
    .map_err(|_| LauncherError::InvalidLaunchPolicy)?;
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG
        || normalized_stat_identity(&stat) != expected_identity
        || status != Some(expected_access)
    {
        return Err(LauncherError::InvalidLaunchPolicy);
    }
    if policy.strict_guest {
        let expected_mode = if policy.access == GrantAccess::ReadOnly {
            0o400
        } else {
            0o600
        };
        // Immutable evidence inputs are sealed bundle resources prepared by
        // one ordinary builder and reused unchanged across mapped, retained-
        // root, and unmapped identities. Their authority is the exact opened
        // identity plus read-only status, mode, and singleton link. Writable
        // outputs remain owned by the transitioned target identity.
        if stat.st_mode & 0o7777 != expected_mode
            || stat.st_nlink != 1
            || (policy.access != GrantAccess::ReadOnly
                && (stat.st_uid != policy.target_uid || stat.st_gid != policy.target_gid))
        {
            return Err(LauncherError::InvalidLaunchPolicy);
        }
    }
    if let Some(seal) = policy.seal {
        validate_elevated_resource_seal(descriptor, expected_identity, seal)?;
    }
    Ok(())
}

#[cfg(feature = "elevated-bootstrap-probe")]
fn validate_elevated_resource_seal(
    descriptor: RawFd,
    expected_identity: ObjectIdentity,
    seal: ElevatedResourceSeal,
) -> Result<(), LauncherError> {
    use sha2::{Digest, Sha256};

    let initial = descriptor_stat(descriptor)?;
    if normalized_stat_identity(&initial) != expected_identity
        || u64::try_from(initial.st_size).ok() != Some(seal.size_bytes)
    {
        return Err(LauncherError::InvalidLaunchPolicy);
    }
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut offset = 0_u64;
    while offset < seal.size_bytes {
        let remaining = seal.size_bytes - offset;
        let requested = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| LauncherError::InvalidLaunchPolicy)?;
        let file_offset =
            libc::off_t::try_from(offset).map_err(|_| LauncherError::InvalidLaunchPolicy)?;
        // SAFETY: The bounded prefix of `buffer` is writable and the borrowed
        // descriptor remains open for the complete pre-transition validation.
        let destination = buffer
            .get_mut(..requested)
            .ok_or(LauncherError::InvalidLaunchPolicy)?;
        // SAFETY: The bounded destination is writable and the borrowed
        // descriptor remains open for the complete pre-transition validation.
        let read = unsafe {
            libc::pread(
                descriptor,
                destination.as_mut_ptr().cast(),
                requested,
                file_offset,
            )
        };
        if read > 0 {
            let read = usize::try_from(read).map_err(|_| LauncherError::InvalidLaunchPolicy)?;
            digest.update(
                buffer
                    .get(..read)
                    .ok_or(LauncherError::InvalidLaunchPolicy)?,
            );
            offset = offset
                .checked_add(u64::try_from(read).map_err(|_| LauncherError::InvalidLaunchPolicy)?)
                .ok_or(LauncherError::InvalidLaunchPolicy)?;
        } else if read < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        } else {
            return Err(LauncherError::InvalidLaunchPolicy);
        }
    }
    let final_stat = descriptor_stat(descriptor)?;
    let actual: [u8; 32] = digest.finalize().into();
    if normalized_stat_identity(&final_stat) != expected_identity
        || u64::try_from(final_stat.st_size).ok() != Some(seal.size_bytes)
        || actual != seal.sha256
    {
        return Err(LauncherError::InvalidLaunchPolicy);
    }
    Ok(())
}

#[cfg(feature = "elevated-bootstrap-probe")]
fn validate_elevated_directory(
    descriptor: RawFd,
    expected_identity: ObjectIdentity,
    strict_guest: bool,
    target_uid: u32,
    target_gid: u32,
) -> Result<(), LauncherError> {
    let stat = descriptor_stat(descriptor)?;
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR
        || normalized_stat_identity(&stat) != expected_identity
    {
        return Err(LauncherError::InvalidLaunchPolicy);
    }
    if strict_guest
        && (stat.st_uid != target_uid
            || stat.st_gid != target_gid
            || stat.st_mode & 0o7777 != 0o700
            || stat.st_nlink < 2)
    {
        return Err(LauncherError::InvalidLaunchPolicy);
    }
    Ok(())
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

#[cfg(feature = "elevated-bootstrap-probe")]
fn open_evidence_readback(
    grant: &ManifestGrant,
    expected_identity: ObjectIdentity,
) -> Result<OwnedFd, LauncherError> {
    let read_grant = ManifestGrant {
        id: grant.id.clone(),
        role: ResourceRole::KernelImage,
        access: GrantAccess::ReadOnly,
        source: grant.source.clone(),
    };
    let prepared = open_resource(&read_grant)?;
    let flags = libc::c_int::try_from(prepared.status_flags)
        .map_err(|_| LauncherError::GrantPreparation)?;
    let read_only = u32::try_from(libc::O_RDONLY).map_err(|_| LauncherError::GrantPreparation)?;
    if prepared.kind != GrantObjectKind::RegularFile
        || prepared.identity != expected_identity
        || prepared.block_device.is_some()
        || bangbang_session::macos::normalized_regular_file_status_flags(flags) != Some(read_only)
    {
        return Err(LauncherError::GrantPreparation);
    }
    Ok(prepared.descriptor)
}

fn connect_resource(grant: &ManifestGrant) -> Result<PreparedResource, LauncherError> {
    if !grant.role.is_connected_stream() || grant.access != GrantAccess::ReadWrite {
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
        CONNECTED_STREAM_CONNECT_TIMEOUT,
    )
    .map_err(|_| LauncherError::GrantPreparation)?;
    let source_identity = connected.source_identity();
    // SAFETY: Effective identity calls have no pointer or ownership contract.
    let expected_uid = unsafe { libc::geteuid() };
    // SAFETY: Effective identity calls have no pointer or ownership contract.
    let expected_gid = unsafe { libc::getegid() };
    prepare_connected_stream(
        connected.into_stream(),
        source_identity,
        Some((expected_uid, expected_gid)),
    )
}

fn prepare_connected_stream(
    stream: std::os::unix::net::UnixStream,
    source_identity: ObjectIdentity,
    expected_peer: Option<(u32, u32)>,
) -> Result<PreparedResource, LauncherError> {
    if source_identity.inode == 0 {
        return Err(LauncherError::GrantPreparation);
    }
    let peer = peer_identity(stream.as_raw_fd()).map_err(|_| LauncherError::GrantPreparation)?;
    if expected_peer.is_some_and(|(uid, gid)| peer.uid != uid || peer.gid != gid) {
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

#[cfg(feature = "elevated-bootstrap-probe")]
fn normalized_stat_identity(stat: &libc::stat) -> ObjectIdentity {
    ObjectIdentity {
        device: normalized_device(stat.st_dev),
        inode: stat.st_ino,
    }
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
    use std::os::unix::fs::{PermissionsExt, symlink};
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

    #[cfg(feature = "elevated-bootstrap-probe")]
    fn elevated_guest_batch(
        root: &TestDir,
        workload: bangbang_session::elevated_probe::RuntimeWorkload,
        wrong_kernel_id: bool,
    ) -> PreparedGrantBatch {
        use bangbang_session::elevated_probe::{
            GUEST_API_DIRECTORY_GRANT_ID, GUEST_CONFIG_GRANT_ID, GUEST_INITRD_GRANT_ID,
            GUEST_KERNEL_GRANT_ID, GUEST_LOGGER_GRANT_ID, GUEST_METRICS_GRANT_ID,
            GUEST_ROOTFS_GRANT_ID, GUEST_SERIAL_GRANT_ID, RuntimeWorkload,
        };

        let file = |name: &str, mode: u32| {
            let path = root.path().join(name);
            fs::write(&path, name.as_bytes()).expect("guest fixture should write");
            fs::set_permissions(&path, fs::Permissions::from_mode(mode))
                .expect("guest fixture mode should set");
            path
        };
        let kernel_id = if wrong_kernel_id {
            "evidence-guest-wrong-kernel"
        } else {
            GUEST_KERNEL_GRANT_ID
        };
        let mut grants = Vec::new();
        match workload {
            RuntimeWorkload::GuestNoApi => grants.push(manifest_grant(
                GUEST_CONFIG_GRANT_ID,
                ResourceRole::StartupConfig,
                GrantAccess::ReadOnly,
                file("config", 0o400),
            )),
            RuntimeWorkload::GuestApi => {
                let directory = root.path().join("api");
                fs::create_dir(&directory).expect("API fixture should create");
                fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
                    .expect("API fixture mode should set");
                grants.push(manifest_grant(
                    GUEST_API_DIRECTORY_GRANT_ID,
                    ResourceRole::ApiSocketDirectory,
                    GrantAccess::CreateChildren,
                    directory,
                ));
            }
            RuntimeWorkload::RepresentativeGrants => {
                panic!("guest fixture requires a guest workload")
            }
        }
        grants.extend([
            manifest_grant(
                kernel_id,
                ResourceRole::KernelImage,
                GrantAccess::ReadOnly,
                file("kernel", 0o400),
            ),
            manifest_grant(
                GUEST_INITRD_GRANT_ID,
                ResourceRole::InitrdImage,
                GrantAccess::ReadOnly,
                file("initrd", 0o400),
            ),
            manifest_grant(
                GUEST_ROOTFS_GRANT_ID,
                ResourceRole::DriveBacking,
                GrantAccess::ReadOnly,
                file("rootfs", 0o400),
            ),
            manifest_grant(
                GUEST_LOGGER_GRANT_ID,
                ResourceRole::LoggerSink,
                GrantAccess::WriteOnly,
                file("logger", 0o600),
            ),
            manifest_grant(
                GUEST_METRICS_GRANT_ID,
                ResourceRole::MetricsSink,
                GrantAccess::WriteOnly,
                file("metrics", 0o600),
            ),
            manifest_grant(
                GUEST_SERIAL_GRANT_ID,
                ResourceRole::SerialSink,
                GrantAccess::WriteOnly,
                file("serial", 0o600),
            ),
        ]);
        PreparedGrantBatch::prepare(grants).expect("guest grants should prepare")
    }

    #[cfg(feature = "elevated-bootstrap-probe")]
    fn elevated_guest_record_mut<'a>(
        batch: &'a mut PreparedGrantBatch,
        expected_id: &str,
    ) -> &'a mut PreparedRecord {
        let expected_id = GrantId::parse(expected_id).expect("guest grant ID should parse");
        batch
            .records
            .iter_mut()
            .find(|prepared| match &prepared.record {
                GrantRecord::Descriptor { id, .. }
                | GrantRecord::ConnectedStream { id, .. }
                | GrantRecord::ScopedDirectory { id, .. } => id == &expected_id,
                GrantRecord::Begin { .. }
                | GrantRecord::BookmarkFragment { .. }
                | GrantRecord::Commit { .. } => false,
            })
            .expect("guest semantic grant should exist")
    }

    #[cfg(feature = "elevated-bootstrap-probe")]
    fn assert_elevated_guest_mutation_rejected(
        context: &str,
        mutate: impl FnOnce(&mut PreparedGrantBatch),
    ) {
        use bangbang_session::elevated_probe::RuntimeWorkload;

        let root = TestDir::new();
        let mut batch = elevated_guest_batch(&root, RuntimeWorkload::GuestNoApi, false);
        mutate(&mut batch);
        // SAFETY: Effective identity calls have no pointer or ownership contract.
        let uid = unsafe { libc::geteuid() };
        // SAFETY: Effective identity calls have no pointer or ownership contract.
        let gid = unsafe { libc::getegid() };
        assert!(
            batch
                .validate_elevated_runtime_contract_for_test(RuntimeWorkload::GuestNoApi, uid, gid,)
                .is_err(),
            "{context} must fail before transition"
        );
    }

    #[cfg(feature = "elevated-bootstrap-probe")]
    #[test]
    fn elevated_guest_contract_is_exact_rootless_and_redacted() {
        use bangbang_session::elevated_probe::RuntimeWorkload;

        // SAFETY: Effective identity calls have no pointer or ownership contract.
        let uid = unsafe { libc::geteuid() };
        // SAFETY: Effective identity calls have no pointer or ownership contract.
        let gid = unsafe { libc::getegid() };
        for workload in [RuntimeWorkload::GuestNoApi, RuntimeWorkload::GuestApi] {
            let root = TestDir::new();
            let batch = elevated_guest_batch(&root, workload, false);
            let contract = batch
                .validate_elevated_runtime_contract_for_test(workload, uid, gid)
                .expect("ordinary-user guest contract should validate")
                .expect("guest workload should retain a contract");
            assert_eq!(contract.workload(), workload);
            assert_eq!(
                contract.api_anchor().is_some(),
                workload == RuntimeWorkload::GuestApi
            );
            let serial_evidence = contract.serial_evidence_descriptor();
            let serial_stat = descriptor_stat(serial_evidence)
                .expect("serial evidence descriptor should remain live");
            assert_eq!(
                normalized_stat_identity(&serial_stat),
                contract.serial_evidence_identity()
            );
            // SAFETY: both fcntl operations inspect the live contract descriptor.
            let status = unsafe { libc::fcntl(serial_evidence, libc::F_GETFL) };
            // SAFETY: F_GETFD has no pointer or ownership contract.
            let descriptor_flags = unsafe { libc::fcntl(serial_evidence, libc::F_GETFD) };
            assert_eq!(status & libc::O_ACCMODE, libc::O_RDONLY);
            assert_ne!(descriptor_flags & libc::FD_CLOEXEC, 0);
            assert_eq!(format!("{contract:?}"), "ElevatedGuestContract(<redacted>)");
        }

        let wrong_root = TestDir::new();
        let wrong = elevated_guest_batch(&wrong_root, RuntimeWorkload::GuestNoApi, true);
        assert!(
            wrong
                .validate_elevated_runtime_contract_for_test(RuntimeWorkload::GuestNoApi, uid, gid)
                .is_err(),
            "a substituted grant ID must fail before transition"
        );
        let owner_root = TestDir::new();
        let owner = elevated_guest_batch(&owner_root, RuntimeWorkload::GuestNoApi, false);
        assert!(
            owner
                .validate_elevated_runtime_contract_for_test(
                    RuntimeWorkload::GuestNoApi,
                    uid.wrapping_add(1),
                    gid,
                )
                .is_err(),
            "the contract must bind ordinary-user ownership"
        );
        let workload_root = TestDir::new();
        let workload = elevated_guest_batch(&workload_root, RuntimeWorkload::GuestNoApi, false);
        assert!(
            workload
                .validate_elevated_runtime_contract_for_test(RuntimeWorkload::GuestApi, uid, gid,)
                .is_err(),
            "the contract must bind its exact workload"
        );
    }

    #[cfg(feature = "elevated-bootstrap-probe")]
    #[test]
    fn elevated_guest_contract_rejects_hostile_record_shapes() {
        use bangbang_session::elevated_probe::{GUEST_INITRD_GRANT_ID, GUEST_KERNEL_GRANT_ID};

        assert_elevated_guest_mutation_rejected("missing grant", |batch| {
            let expected = GrantId::parse(GUEST_INITRD_GRANT_ID).expect("grant ID should parse");
            let index = batch
                .records
                .iter()
                .position(|prepared| {
                    matches!(&prepared.record, GrantRecord::Descriptor { id, .. } if id == &expected)
                })
                .expect("initrd grant should exist");
            drop(batch.records.remove(index));
        });
        assert_elevated_guest_mutation_rejected("extra grant", |batch| {
            let record = elevated_guest_record_mut(batch, GUEST_KERNEL_GRANT_ID)
                .record
                .clone();
            let index = batch.records.len() - 1;
            batch.records.insert(
                index,
                PreparedRecord {
                    record,
                    descriptor: None,
                    evidence_readback: None,
                },
            );
        });
        assert_elevated_guest_mutation_rejected("duplicate grant ID", |batch| {
            let record = &mut elevated_guest_record_mut(batch, GUEST_INITRD_GRANT_ID).record;
            let GrantRecord::Descriptor { id, .. } = record else {
                panic!("initrd grant should be descriptor-backed");
            };
            *id = GrantId::parse(GUEST_KERNEL_GRANT_ID).expect("grant ID should parse");
        });
        assert_elevated_guest_mutation_rejected("wrong grant role", |batch| {
            let record = &mut elevated_guest_record_mut(batch, GUEST_KERNEL_GRANT_ID).record;
            let GrantRecord::Descriptor { role, .. } = record else {
                panic!("kernel grant should be descriptor-backed");
            };
            *role = ResourceRole::InitrdImage;
        });
        assert_elevated_guest_mutation_rejected("wrong grant access", |batch| {
            let record = &mut elevated_guest_record_mut(batch, GUEST_KERNEL_GRANT_ID).record;
            let GrantRecord::Descriptor { access, .. } = record else {
                panic!("kernel grant should be descriptor-backed");
            };
            *access = GrantAccess::ReadWrite;
        });
        assert_elevated_guest_mutation_rejected("wrong grant kind", |batch| {
            let record = &mut elevated_guest_record_mut(batch, GUEST_KERNEL_GRANT_ID).record;
            let GrantRecord::Descriptor { kind, .. } = record else {
                panic!("kernel grant should be descriptor-backed");
            };
            *kind = GrantObjectKind::Directory;
        });
        assert_elevated_guest_mutation_rejected("wrong descriptor flags", |batch| {
            let record = &mut elevated_guest_record_mut(batch, GUEST_KERNEL_GRANT_ID).record;
            let GrantRecord::Descriptor { status_flags, .. } = record else {
                panic!("kernel grant should be descriptor-backed");
            };
            *status_flags = u32::try_from(libc::O_RDWR).expect("status flags should fit");
        });
    }

    #[cfg(feature = "elevated-bootstrap-probe")]
    #[test]
    fn elevated_resource_seal_rejects_same_inode_content_and_size_mutation() {
        use sha2::{Digest, Sha256};

        let root = TestDir::new();
        let path = root.path().join("sealed-input");
        fs::write(&path, b"fixed").expect("sealed fixture should write");
        let descriptor = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&path)
            .expect("sealed fixture should open");
        let stat = descriptor_stat(descriptor.as_raw_fd()).expect("fixture should stat");
        let identity = normalized_stat_identity(&stat);
        let seal = ElevatedResourceSeal::new(5, Sha256::digest(b"fixed").into());
        validate_elevated_resource_seal(descriptor.as_raw_fd(), identity, seal)
            .expect("exact size and digest should validate");

        fs::write(&path, b"fazed").expect("same-size mutation should write");
        assert!(
            validate_elevated_resource_seal(descriptor.as_raw_fd(), identity, seal).is_err(),
            "same-inode content replacement must fail"
        );
        fs::write(&path, b"fixed!").expect("size mutation should write");
        assert!(
            validate_elevated_resource_seal(descriptor.as_raw_fd(), identity, seal).is_err(),
            "same-inode size replacement must fail"
        );
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
    fn provider_stream_manifest_connects_once_and_is_classified_for_remote_routing() {
        let root = TestDir::new();
        let socket = root.path().join("provider.sock");
        let listener = UnixListener::bind(&socket).expect("provider listener should bind");
        let manifest = serde_json::json!({
            "version": 1,
            "grants": [{
                "id": "vmnet-provider",
                "role": "vmnet-provider-stream",
                "access": "read-write",
                "source": socket,
            }]
        });
        let grants = parse_manifest(
            &serde_json::to_vec(&manifest).expect("provider manifest should serialize"),
        )
        .expect("provider manifest should parse");
        assert_eq!(grants[0].role, ResourceRole::VmnetProviderStream);
        let batch = PreparedGrantBatch::prepare(grants).expect("provider stream should prepare");
        let (_accepted, _) = listener.accept().expect("provider stream should connect");
        assert!(batch.has_vmnet_provider_stream());
        assert_eq!(
            batch
                .records
                .iter()
                .filter(|record| matches!(
                    record.record,
                    GrantRecord::ConnectedStream {
                        role: ResourceRole::VmnetProviderStream,
                        ..
                    }
                ))
                .count(),
            1
        );
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
