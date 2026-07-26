//! Process-owned destination resources for native-v2 snapshot restore.

use std::fmt;
use std::fs::File;
use std::path::{Path, PathBuf};

use bangbang_runtime::block::{BlockFileBacking, SnapshotBlockFileBackingError};
use bangbang_runtime::snapshot_device_v2::SnapshotV2DeviceGraph;
use bangbang_runtime::snapshot_restore::{
    PreparedSnapshotRestoreBindings, SnapshotRestoreBindingAllocationError,
    SnapshotRestoreBindingRejectionReason, SnapshotRestoreBindings, SnapshotRestoreManifest,
    SnapshotRestoreManifestError, SnapshotRestoreResourceClass, SnapshotRestoreResourceKey,
    SnapshotRestoreTakeError,
};
use bangbang_session::GrantAccess;

use crate::contained_session::{
    GrantAuthority, GrantClaimError, PreparedDriveBackingClaim, grant_reference_id,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnapshotRestoreResourceDisposition {
    Retryable,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnapshotRestoreResourceStage {
    Manifest,
    BindingAllocation,
    Cancellation,
    RootPreparation,
    Binding,
    Completion,
    Take,
    Finish,
}

pub(crate) enum SnapshotRestoreResourceErrorKind {
    Manifest(SnapshotRestoreManifestError),
    BindingAllocation(SnapshotRestoreBindingAllocationError),
    RootBacking(SnapshotRootBackingLeaseError),
    Binding(SnapshotRestoreBindingRejectionReason),
    Incomplete { missing_count: usize },
    Take(SnapshotRestoreTakeError),
    Unconsumed { unconsumed_count: usize },
    OwnerClassMismatch,
    Cancelled,
}

impl fmt::Debug for SnapshotRestoreResourceErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(source) => formatter
                .debug_tuple("SnapshotRestoreResourceErrorKind::Manifest")
                .field(source)
                .finish(),
            Self::BindingAllocation(source) => formatter
                .debug_tuple("SnapshotRestoreResourceErrorKind::BindingAllocation")
                .field(source)
                .finish(),
            Self::RootBacking(source) => formatter
                .debug_tuple("SnapshotRestoreResourceErrorKind::RootBacking")
                .field(source)
                .finish(),
            Self::Binding(reason) => formatter
                .debug_tuple("SnapshotRestoreResourceErrorKind::Binding")
                .field(reason)
                .finish(),
            Self::Incomplete { missing_count } => formatter
                .debug_struct("SnapshotRestoreResourceErrorKind::Incomplete")
                .field("missing_count", missing_count)
                .finish(),
            Self::Take(source) => formatter
                .debug_tuple("SnapshotRestoreResourceErrorKind::Take")
                .field(source)
                .finish(),
            Self::Unconsumed { unconsumed_count } => formatter
                .debug_struct("SnapshotRestoreResourceErrorKind::Unconsumed")
                .field("unconsumed_count", unconsumed_count)
                .finish(),
            Self::OwnerClassMismatch => {
                formatter.write_str("SnapshotRestoreResourceErrorKind::OwnerClassMismatch")
            }
            Self::Cancelled => formatter.write_str("SnapshotRestoreResourceErrorKind::Cancelled"),
        }
    }
}

impl fmt::Display for SnapshotRestoreResourceErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(source) => source.fmt(formatter),
            Self::BindingAllocation(source) => source.fmt(formatter),
            Self::RootBacking(source) => source.fmt(formatter),
            Self::Binding(reason) => reason.fmt(formatter),
            Self::Incomplete { .. } => {
                formatter.write_str("snapshot restore resource set is incomplete")
            }
            Self::Take(source) => source.fmt(formatter),
            Self::Unconsumed { .. } => {
                formatter.write_str("snapshot restore resource set is unconsumed")
            }
            Self::OwnerClassMismatch => {
                formatter.write_str("snapshot restore owner has the wrong resource class")
            }
            Self::Cancelled => {
                formatter.write_str("snapshot restore resource preparation cancelled")
            }
        }
    }
}

pub(crate) struct SnapshotRestoreResourceError {
    stage: SnapshotRestoreResourceStage,
    kind: SnapshotRestoreResourceErrorKind,
    disposition: SnapshotRestoreResourceDisposition,
    cleanup_failed: bool,
}

impl SnapshotRestoreResourceError {
    fn retryable(
        stage: SnapshotRestoreResourceStage,
        kind: SnapshotRestoreResourceErrorKind,
    ) -> Self {
        Self {
            stage,
            kind,
            disposition: SnapshotRestoreResourceDisposition::Retryable,
            cleanup_failed: false,
        }
    }

    fn terminal(
        stage: SnapshotRestoreResourceStage,
        kind: SnapshotRestoreResourceErrorKind,
    ) -> Self {
        Self {
            stage,
            kind,
            disposition: SnapshotRestoreResourceDisposition::Terminal,
            cleanup_failed: false,
        }
    }

    fn with_cleanup_failed(mut self, cleanup_failed: bool) -> Self {
        if cleanup_failed {
            self.disposition = SnapshotRestoreResourceDisposition::Terminal;
            self.cleanup_failed = true;
        }
        self
    }

    pub(crate) const fn disposition(&self) -> SnapshotRestoreResourceDisposition {
        self.disposition
    }
}

impl fmt::Debug for SnapshotRestoreResourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotRestoreResourceError")
            .field("stage", &self.stage)
            .field("kind", &self.kind)
            .field("disposition", &self.disposition)
            .field("cleanup_failed", &self.cleanup_failed)
            .finish()
    }
}

impl fmt::Display for SnapshotRestoreResourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "snapshot restore resources failed at {:?}: {} ({:?})",
            self.stage, self.kind, self.disposition
        )?;
        if self.cleanup_failed {
            formatter.write_str("; resource cleanup also failed")?;
        }
        Ok(())
    }
}

impl std::error::Error for SnapshotRestoreResourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            SnapshotRestoreResourceErrorKind::Manifest(source) => Some(source),
            SnapshotRestoreResourceErrorKind::BindingAllocation(source) => Some(source),
            SnapshotRestoreResourceErrorKind::RootBacking(source) => Some(source),
            SnapshotRestoreResourceErrorKind::Take(source) => Some(source),
            SnapshotRestoreResourceErrorKind::Binding(_)
            | SnapshotRestoreResourceErrorKind::Incomplete { .. }
            | SnapshotRestoreResourceErrorKind::Unconsumed { .. }
            | SnapshotRestoreResourceErrorKind::OwnerClassMismatch
            | SnapshotRestoreResourceErrorKind::Cancelled => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnapshotRootSelectorPolicy {
    TreatAsPath,
    RejectGrantReference,
    RequireGrantWhenContained,
}

pub(crate) struct PreparedSnapshotRootBackingLease {
    selector: Option<PathBuf>,
    claim: Option<PreparedDriveBackingClaim>,
    consumed: bool,
}

impl PreparedSnapshotRootBackingLease {
    pub(crate) fn prepare(
        selector: &Path,
        authority: Option<&GrantAuthority>,
        selector_policy: SnapshotRootSelectorPolicy,
    ) -> Result<Self, GrantClaimError> {
        let claim = match authority {
            Some(authority) => {
                match authority.prepare_drive_backing_claim(selector, GrantAccess::ReadOnly)? {
                    Some(claim) => Some(claim),
                    None if selector_policy
                        == SnapshotRootSelectorPolicy::RequireGrantWhenContained =>
                    {
                        return Err(GrantClaimError);
                    }
                    None => None,
                }
            }
            None => {
                if selector_policy == SnapshotRootSelectorPolicy::RejectGrantReference
                    && grant_reference_id(selector)?.is_some()
                {
                    return Err(GrantClaimError);
                }
                None
            }
        };
        Ok(Self {
            selector: Some(selector.to_path_buf()),
            claim,
            consumed: false,
        })
    }

    pub(crate) fn take_snapshot_read_only_file(&mut self) -> Result<Option<File>, GrantClaimError> {
        let (_selector, file) = self.consume_inner()?;
        Ok(file)
    }

    #[cfg(test)]
    pub(crate) fn consume(&mut self) -> Result<(PathBuf, Option<File>), GrantClaimError> {
        self.consume_inner()
    }

    fn consume_inner(&mut self) -> Result<(PathBuf, Option<File>), GrantClaimError> {
        if self.consumed {
            return Err(GrantClaimError);
        }
        self.consumed = true;
        let selector = self.selector.take().ok_or(GrantClaimError)?;
        let file = self
            .claim
            .as_mut()
            .map(PreparedDriveBackingClaim::take_snapshot_read_only_file)
            .transpose()?;
        Ok((selector, file))
    }

    pub(crate) fn open_snapshot_read_only(
        &mut self,
    ) -> Result<BlockFileBacking, SnapshotRootBackingLeaseError> {
        let (selector, file) = self
            .consume_inner()
            .map_err(SnapshotRootBackingLeaseError::Grant)?;
        match file {
            Some(file) => BlockFileBacking::from_snapshot_read_only_file(file)
                .map(|(backing, _identity)| backing)
                .map_err(SnapshotRootBackingLeaseError::Backing),
            None => BlockFileBacking::open_snapshot_read_only(&selector)
                .map(|(backing, _identity)| backing)
                .map_err(SnapshotRootBackingLeaseError::Backing),
        }
    }

    pub(crate) fn commit(self) {
        if let Some(claim) = self.claim {
            claim.commit();
        }
    }

    pub(crate) fn abort(self) -> Result<(), GrantClaimError> {
        match self.claim {
            Some(claim) => claim.abort(),
            None => Ok(()),
        }
    }
}

impl fmt::Debug for PreparedSnapshotRootBackingLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotRootBackingLease")
            .field("selector", &self.selector.as_ref().map(|_| "<redacted>"))
            .field("claim", &self.claim.as_ref().map(|_| "<provisional>"))
            .field("consumed", &self.consumed)
            .finish()
    }
}

#[derive(Debug)]
pub(crate) enum SnapshotRootBackingLeaseError {
    Grant(GrantClaimError),
    Backing(SnapshotBlockFileBackingError),
}

impl fmt::Display for SnapshotRootBackingLeaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Grant(_) => formatter.write_str("snapshot root authority validation failed"),
            Self::Backing(_) => formatter.write_str("snapshot root backing validation failed"),
        }
    }
}

impl std::error::Error for SnapshotRootBackingLeaseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Grant(source) => Some(source),
            Self::Backing(source) => Some(source),
        }
    }
}

pub(crate) struct PreparedSnapshotRootRestoreCompletion {
    lease: PreparedSnapshotRootBackingLease,
}

impl PreparedSnapshotRootRestoreCompletion {
    pub(crate) fn commit(self) {
        self.lease.commit();
    }

    pub(crate) fn abort(self) -> Result<(), GrantClaimError> {
        self.lease.abort()
    }
}

impl fmt::Debug for PreparedSnapshotRootRestoreCompletion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotRootRestoreCompletion")
            .field("state", &"<provisional>")
            .finish()
    }
}

pub(crate) struct PreparedSnapshotRootRestoreResource {
    backing: BlockFileBacking,
    completion: PreparedSnapshotRootRestoreCompletion,
}

impl PreparedSnapshotRootRestoreResource {
    fn prepare(
        selector: &Path,
        authority: Option<&GrantAuthority>,
    ) -> Result<Self, SnapshotRestoreResourceError> {
        let mut lease = PreparedSnapshotRootBackingLease::prepare(
            selector,
            authority,
            SnapshotRootSelectorPolicy::RequireGrantWhenContained,
        )
        .map_err(|source| {
            let kind = SnapshotRestoreResourceErrorKind::RootBacking(
                SnapshotRootBackingLeaseError::Grant(source),
            );
            if authority.is_some_and(|authority| !authority.is_active()) {
                SnapshotRestoreResourceError::terminal(
                    SnapshotRestoreResourceStage::RootPreparation,
                    kind,
                )
            } else {
                SnapshotRestoreResourceError::retryable(
                    SnapshotRestoreResourceStage::RootPreparation,
                    kind,
                )
            }
        })?;
        let backing = match lease.open_snapshot_read_only() {
            Ok(backing) => backing,
            Err(source) => {
                let cleanup_failed = lease.abort().is_err();
                return Err(SnapshotRestoreResourceError::retryable(
                    SnapshotRestoreResourceStage::RootPreparation,
                    SnapshotRestoreResourceErrorKind::RootBacking(source),
                )
                .with_cleanup_failed(cleanup_failed));
            }
        };
        Ok(Self {
            backing,
            completion: PreparedSnapshotRootRestoreCompletion { lease },
        })
    }

    pub(crate) fn into_parts(self) -> (BlockFileBacking, PreparedSnapshotRootRestoreCompletion) {
        (self.backing, self.completion)
    }

    fn abort(self) -> Result<(), GrantClaimError> {
        let Self {
            backing,
            completion,
        } = self;
        drop(backing);
        completion.abort()
    }
}

impl fmt::Debug for PreparedSnapshotRootRestoreResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotRootRestoreResource")
            .field("backing", &"<owned>")
            .field("completion", &"<provisional>")
            .finish()
    }
}

enum PreparedSnapshotRestoreResource {
    Root(PreparedSnapshotRootRestoreResource),
}

impl PreparedSnapshotRestoreResource {
    const fn resource_class(&self) -> SnapshotRestoreResourceClass {
        match self {
            Self::Root(_) => SnapshotRestoreResourceClass::BlockBacking,
        }
    }

    fn abort(self) -> Result<(), GrantClaimError> {
        match self {
            Self::Root(root) => root.abort(),
        }
    }
}

impl fmt::Debug for PreparedSnapshotRestoreResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotRestoreResource")
            .field("class", &self.resource_class())
            .field("value", &"<owned>")
            .finish()
    }
}

pub(crate) struct RequestedSnapshotRestoreResources {
    root_key: SnapshotRestoreResourceKey,
    root_selector: PathBuf,
    bindings: SnapshotRestoreBindings<PreparedSnapshotRestoreResource>,
}

impl RequestedSnapshotRestoreResources {
    pub(crate) fn try_from_native_v2_device_graph(
        graph: &SnapshotV2DeviceGraph,
    ) -> Result<Self, SnapshotRestoreResourceError> {
        let manifest = SnapshotRestoreManifest::try_from_native_v2_device_graph(graph, Vec::new())
            .map_err(|source| {
                SnapshotRestoreResourceError::retryable(
                    SnapshotRestoreResourceStage::Manifest,
                    SnapshotRestoreResourceErrorKind::Manifest(source),
                )
            })?;
        let root_key = manifest.resources().first().cloned().ok_or_else(|| {
            SnapshotRestoreResourceError::terminal(
                SnapshotRestoreResourceStage::Manifest,
                SnapshotRestoreResourceErrorKind::OwnerClassMismatch,
            )
        })?;
        if manifest.len() != 1
            || root_key.resource_class() != SnapshotRestoreResourceClass::BlockBacking
        {
            return Err(SnapshotRestoreResourceError::terminal(
                SnapshotRestoreResourceStage::Manifest,
                SnapshotRestoreResourceErrorKind::OwnerClassMismatch,
            ));
        }
        let bindings = manifest.try_into_bindings().map_err(|source| {
            SnapshotRestoreResourceError::retryable(
                SnapshotRestoreResourceStage::BindingAllocation,
                SnapshotRestoreResourceErrorKind::BindingAllocation(source),
            )
        })?;
        Ok(Self {
            root_key,
            root_selector: PathBuf::from(graph.record().config().selector()),
            bindings,
        })
    }

    pub(crate) fn prepare_root(
        self,
        authority: Option<&GrantAuthority>,
        cancelled: impl Fn() -> bool,
    ) -> Result<PreparedSnapshotRestoreResources, SnapshotRestoreResourceError> {
        self.prepare_root_with(cancelled, |selector| {
            PreparedSnapshotRootRestoreResource::prepare(selector, authority)
        })
    }

    fn prepare_root_with(
        self,
        cancelled: impl Fn() -> bool,
        provider: impl FnOnce(
            &Path,
        ) -> Result<
            PreparedSnapshotRootRestoreResource,
            SnapshotRestoreResourceError,
        >,
    ) -> Result<PreparedSnapshotRestoreResources, SnapshotRestoreResourceError> {
        let root_key = self.root_key.clone();
        self.prepare_root_with_key(root_key, cancelled, provider)
    }

    fn prepare_root_with_key(
        mut self,
        root_key: SnapshotRestoreResourceKey,
        cancelled: impl Fn() -> bool,
        provider: impl FnOnce(
            &Path,
        ) -> Result<
            PreparedSnapshotRootRestoreResource,
            SnapshotRestoreResourceError,
        >,
    ) -> Result<PreparedSnapshotRestoreResources, SnapshotRestoreResourceError> {
        if cancelled() {
            let cleanup_failed = abort_resources(self.bindings.into_values());
            return Err(SnapshotRestoreResourceError::retryable(
                SnapshotRestoreResourceStage::Cancellation,
                SnapshotRestoreResourceErrorKind::Cancelled,
            )
            .with_cleanup_failed(cleanup_failed));
        }
        let root = match provider(&self.root_selector) {
            Ok(root) => root,
            Err(source) => {
                let cleanup_failed = abort_resources(self.bindings.into_values());
                return Err(source.with_cleanup_failed(cleanup_failed));
            }
        };
        let owner = PreparedSnapshotRestoreResource::Root(root);
        if owner.resource_class() != root_key.resource_class() {
            let mut cleanup_failed = owner.abort().is_err();
            cleanup_failed |= abort_resources(self.bindings.into_values());
            return Err(SnapshotRestoreResourceError::terminal(
                SnapshotRestoreResourceStage::Binding,
                SnapshotRestoreResourceErrorKind::OwnerClassMismatch,
            )
            .with_cleanup_failed(cleanup_failed));
        }
        if let Err(rejection) = self.bindings.bind(&root_key, owner) {
            let reason = rejection.reason();
            let mut cleanup_failed = rejection.into_value().abort().is_err();
            cleanup_failed |= abort_resources(self.bindings.into_values());
            return Err(SnapshotRestoreResourceError::terminal(
                SnapshotRestoreResourceStage::Binding,
                SnapshotRestoreResourceErrorKind::Binding(reason),
            )
            .with_cleanup_failed(cleanup_failed));
        }
        let prepared = self.complete()?;
        if cancelled() {
            let cleanup_failed = prepared.abort();
            return Err(SnapshotRestoreResourceError::retryable(
                SnapshotRestoreResourceStage::Cancellation,
                SnapshotRestoreResourceErrorKind::Cancelled,
            )
            .with_cleanup_failed(cleanup_failed));
        }
        Ok(prepared)
    }

    fn complete(self) -> Result<PreparedSnapshotRestoreResources, SnapshotRestoreResourceError> {
        let bindings = match self.bindings.complete() {
            Ok(bindings) => bindings,
            Err(incomplete) => {
                let missing_count = incomplete.missing_count();
                let cleanup_failed = abort_resources(incomplete.into_bindings().into_values());
                return Err(SnapshotRestoreResourceError::terminal(
                    SnapshotRestoreResourceStage::Completion,
                    SnapshotRestoreResourceErrorKind::Incomplete { missing_count },
                )
                .with_cleanup_failed(cleanup_failed));
            }
        };
        Ok(PreparedSnapshotRestoreResources {
            root_key: self.root_key,
            bindings,
        })
    }
}

impl fmt::Debug for RequestedSnapshotRestoreResources {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RequestedSnapshotRestoreResources")
            .field("resource_count", &self.bindings.manifest().len())
            .field("values", &"<redacted>")
            .finish()
    }
}

pub(crate) struct PreparedSnapshotRestoreResources {
    root_key: SnapshotRestoreResourceKey,
    bindings: PreparedSnapshotRestoreBindings<PreparedSnapshotRestoreResource>,
}

impl PreparedSnapshotRestoreResources {
    fn take_root(
        &mut self,
        key: &SnapshotRestoreResourceKey,
    ) -> Result<PreparedSnapshotRootRestoreResource, SnapshotRestoreResourceError> {
        let owner = self.bindings.take(key).map_err(|source| {
            SnapshotRestoreResourceError::terminal(
                SnapshotRestoreResourceStage::Take,
                SnapshotRestoreResourceErrorKind::Take(source),
            )
        })?;
        match owner {
            PreparedSnapshotRestoreResource::Root(root) => Ok(root),
        }
    }

    fn finish(self) -> Result<(), SnapshotRestoreResourceError> {
        match self.bindings.finish() {
            Ok(()) => Ok(()),
            Err(unconsumed) => {
                let unconsumed_count = unconsumed.unconsumed_count();
                let cleanup_failed = abort_resources(unconsumed.into_bindings().into_values());
                Err(SnapshotRestoreResourceError::terminal(
                    SnapshotRestoreResourceStage::Finish,
                    SnapshotRestoreResourceErrorKind::Unconsumed { unconsumed_count },
                )
                .with_cleanup_failed(cleanup_failed))
            }
        }
    }

    fn consume_root_with<T>(
        mut self,
        consumer: impl FnOnce(PreparedSnapshotRootRestoreResource) -> T,
    ) -> Result<T, SnapshotRestoreResourceError> {
        let key = self.root_key.clone();
        let root = match self.take_root(&key) {
            Ok(root) => root,
            Err(source) => {
                let cleanup_failed = self.abort();
                return Err(source.with_cleanup_failed(cleanup_failed));
            }
        };
        if let Err(source) = self.finish() {
            let cleanup_failed = root.abort().is_err();
            return Err(source.with_cleanup_failed(cleanup_failed));
        }
        Ok(consumer(root))
    }

    pub(crate) fn into_root(
        self,
    ) -> Result<PreparedSnapshotRootRestoreResource, SnapshotRestoreResourceError> {
        self.consume_root_with(|root| root)
    }

    fn abort(self) -> bool {
        abort_resources(self.bindings.into_values())
    }
}

impl fmt::Debug for PreparedSnapshotRestoreResources {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotRestoreResources")
            .field("resource_count", &self.bindings.manifest().len())
            .field("remaining_count", &self.bindings.remaining_count())
            .field("values", &"<redacted>")
            .finish()
    }
}

fn abort_resources(
    resources: impl DoubleEndedIterator<Item = PreparedSnapshotRestoreResource>,
) -> bool {
    let mut failed = false;
    for resource in resources.rev() {
        failed |= resource.abort().is_err();
    }
    failed
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::env;
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    use bangbang_runtime::snapshot_device_v2::{
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION, SnapshotV2DeviceTransportKind,
    };
    use bangbang_runtime::snapshot_restore::{
        SnapshotRestoreBindingRejectionReason, SnapshotRestorePublicId,
    };

    use crate::contained_session::{GrantAuthority, root_file_grant_authority_for_test};

    use super::*;

    const ROOT_REFERENCE: &str = "bangbang-grant:drive-ro";

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    struct TempRoot {
        path: PathBuf,
    }

    impl TempRoot {
        fn new(name: &str, bytes: &[u8]) -> Self {
            let path = unique_path(name);
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .expect("temporary root should be created once");
            file.write_all(bytes)
                .expect("temporary root bytes should write");
            file.sync_all().expect("temporary root should synchronize");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    fn unique_path(name: &str) -> PathBuf {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        env::temp_dir().join(format!(
            "bangbang-snapshot-restore-{name}-{}-{id}",
            std::process::id()
        ))
    }

    fn fixture_graph(transport: SnapshotV2DeviceTransportKind) -> SnapshotV2DeviceGraph {
        let fixture = match transport {
            SnapshotV2DeviceTransportKind::Mmio => {
                include_str!("../../runtime/src/snapshot_device_v2/fixtures/mmio.hex")
            }
            SnapshotV2DeviceTransportKind::Pci => {
                include_str!("../../runtime/src/snapshot_device_v2/fixtures/pci.hex")
            }
        };
        let bytes = fixture
            .trim()
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let pair = std::str::from_utf8(pair).expect("fixture hex should be UTF-8");
                u8::from_str_radix(pair, 16).expect("fixture hex should decode")
            })
            .collect::<Vec<_>>();
        SnapshotV2DeviceGraph::decode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION, &bytes)
            .expect("fixture graph should decode")
    }

    fn requested() -> RequestedSnapshotRestoreResources {
        RequestedSnapshotRestoreResources::try_from_native_v2_device_graph(&fixture_graph(
            SnapshotV2DeviceTransportKind::Mmio,
        ))
        .expect("fixture request should build")
    }

    fn contained_root(
        authority: &GrantAuthority,
    ) -> Result<PreparedSnapshotRootRestoreResource, SnapshotRestoreResourceError> {
        PreparedSnapshotRootRestoreResource::prepare(Path::new(ROOT_REFERENCE), Some(authority))
    }

    fn assert_root_claim_restored(authority: &GrantAuthority) {
        let claim = authority
            .prepare_drive_backing_claim(Path::new(ROOT_REFERENCE), GrantAccess::ReadOnly)
            .expect("restored root authority should remain usable")
            .expect("restored root claim should be present");
        claim
            .abort()
            .expect("observed restored claim should return to authority");
    }

    #[test]
    fn current_graphs_build_one_exact_host_free_root_request() {
        for transport in [
            SnapshotV2DeviceTransportKind::Mmio,
            SnapshotV2DeviceTransportKind::Pci,
        ] {
            let graph = fixture_graph(transport);
            let request =
                RequestedSnapshotRestoreResources::try_from_native_v2_device_graph(&graph)
                    .expect("current graph should produce a request");

            assert_eq!(request.root_key.device_key(), graph.root_key());
            assert_eq!(
                request.root_key.public_id().as_str(),
                graph.record().config().drive_id()
            );
            assert_eq!(
                request.root_key.resource_class(),
                SnapshotRestoreResourceClass::BlockBacking
            );
            assert_eq!(request.bindings.manifest().len(), 1);
            assert_eq!(request.bindings.missing_count(), 1);
            assert_eq!(
                request.root_selector,
                PathBuf::from(graph.record().config().selector())
            );

            let diagnostics = format!("{request:?} {:?}", request.root_key);
            assert!(!diagnostics.contains(graph.record().config().drive_id()));
            assert!(!diagnostics.contains(graph.record().config().selector()));
        }
    }

    #[test]
    fn complete_and_finish_gate_host_access_before_construction() {
        let root = TempRoot::new("ordering", b"root");
        let events = RefCell::new(vec!["logical-request"]);
        let request = requested();
        let prepared = request
            .prepare_root_with(
                || false,
                |selector| {
                    assert_eq!(selector, Path::new("root-selector"));
                    events.borrow_mut().push("host-prepare");
                    PreparedSnapshotRootRestoreResource::prepare(root.path(), None)
                },
            )
            .expect("root batch should prepare");
        events.borrow_mut().push("complete-batch");

        let resource = prepared
            .consume_root_with(|resource| {
                events.borrow_mut().push("construction");
                resource
            })
            .expect("exact take and finish should precede construction");
        assert_eq!(
            events.into_inner(),
            [
                "logical-request",
                "host-prepare",
                "complete-batch",
                "construction",
            ]
        );
        resource
            .abort()
            .expect("direct completion should abort infallibly");
    }

    #[test]
    fn cancellation_brackets_host_preparation_and_reports_cleanup_evidence() {
        let host_called = Cell::new(false);
        let error = requested()
            .prepare_root_with(
                || true,
                |_| {
                    host_called.set(true);
                    panic!("cancelled request must not invoke its provider");
                },
            )
            .expect_err("pre-provider cancellation should stop preparation");
        assert!(!host_called.get());
        assert_eq!(error.stage, SnapshotRestoreResourceStage::Cancellation);
        assert!(matches!(
            error.kind,
            SnapshotRestoreResourceErrorKind::Cancelled
        ));
        assert_eq!(
            error.disposition,
            SnapshotRestoreResourceDisposition::Retryable
        );
        assert!(!error.cleanup_failed);

        let root = TempRoot::new("cancel-after-prepare", b"root");
        let authority = root_file_grant_authority_for_test(root.path());
        let checks = Cell::new(0);
        let error = requested()
            .prepare_root_with(
                || {
                    checks.set(checks.get() + 1);
                    checks.get() == 2
                },
                |_| contained_root(&authority),
            )
            .expect_err("post-provider cancellation should abort the complete batch");
        assert_eq!(checks.get(), 2);
        assert_eq!(error.stage, SnapshotRestoreResourceStage::Cancellation);
        assert!(matches!(
            error.kind,
            SnapshotRestoreResourceErrorKind::Cancelled
        ));
        assert_eq!(
            error.disposition,
            SnapshotRestoreResourceDisposition::Retryable
        );
        assert!(!error.cleanup_failed);
        assert_root_claim_restored(&authority);

        let poisoned_root = TempRoot::new("cancel-cleanup-failure", b"root");
        let poisoned_authority = root_file_grant_authority_for_test(poisoned_root.path());
        let checks = Cell::new(0);
        let error = requested()
            .prepare_root_with(
                || {
                    checks.set(checks.get() + 1);
                    if checks.get() == 2 {
                        poisoned_authority.invalidate_for_test();
                        true
                    } else {
                        false
                    }
                },
                |_| contained_root(&poisoned_authority),
            )
            .expect_err("inactive cleanup authority should fail closed");
        assert_eq!(error.stage, SnapshotRestoreResourceStage::Cancellation);
        assert_eq!(
            error.disposition,
            SnapshotRestoreResourceDisposition::Terminal
        );
        assert!(error.cleanup_failed);
    }

    #[test]
    fn direct_and_contained_resources_share_one_pathless_shape() {
        let root = TempRoot::new("direct-contained-shape", b"root-shape");
        let direct = PreparedSnapshotRootRestoreResource::prepare(root.path(), None)
            .expect("direct root should prepare");
        let (direct_backing, direct_completion) = direct.into_parts();
        assert!(direct_backing.kind().is_regular_file());
        assert!(direct_backing.is_read_only());
        assert_eq!(direct_backing.len(), 10);
        drop(direct_backing);
        direct_completion
            .abort()
            .expect("direct completion should abort infallibly");

        let authority = root_file_grant_authority_for_test(root.path());
        let observer = authority.clone();
        let contained = contained_root(&authority).expect("contained root should prepare");
        let diagnostics = format!("{contained:?}");
        assert!(!diagnostics.contains(ROOT_REFERENCE));
        assert!(!diagnostics.contains(&root.path().display().to_string()));
        let (contained_backing, contained_completion) = contained.into_parts();
        assert!(contained_backing.kind().is_regular_file());
        assert!(contained_backing.is_read_only());
        assert_eq!(contained_backing.len(), 10);
        drop(contained_backing);
        contained_completion
            .abort()
            .expect("contained completion should restore its exact claim");
        assert_root_claim_restored(&observer);

        let committed_authority = root_file_grant_authority_for_test(root.path());
        let committed_observer = committed_authority.clone();
        let committed =
            contained_root(&committed_authority).expect("committed root should prepare");
        let (backing, completion) = committed.into_parts();
        drop(backing);
        completion.commit();
        assert!(
            committed_observer
                .prepare_drive_backing_claim(Path::new(ROOT_REFERENCE), GrantAccess::ReadOnly,)
                .is_err(),
            "commit should consume the contained claim exactly once"
        );
    }

    #[test]
    fn contained_root_rejects_ambient_paths_without_fallback() {
        let root = TempRoot::new("contained-no-fallback", b"root");
        let authority = root_file_grant_authority_for_test(root.path());
        let error = PreparedSnapshotRootRestoreResource::prepare(
            Path::new("/private/ambient-root"),
            Some(&authority),
        )
        .expect_err("contained root must require an exact grant reference");
        assert_eq!(
            error.disposition,
            SnapshotRestoreResourceDisposition::Retryable
        );
        assert_eq!(error.stage, SnapshotRestoreResourceStage::RootPreparation);
        assert!(matches!(
            error.kind,
            SnapshotRestoreResourceErrorKind::RootBacking(SnapshotRootBackingLeaseError::Grant(_))
        ));
        assert_root_claim_restored(&authority);
    }

    #[test]
    fn hostile_binding_and_consumption_states_abort_every_root_owner() {
        let root = TempRoot::new("hostile-bindings", b"root");

        let missing = requested()
            .complete()
            .expect_err("unbound request should remain incomplete");
        assert_eq!(missing.stage, SnapshotRestoreResourceStage::Completion);
        assert!(matches!(
            missing.kind,
            SnapshotRestoreResourceErrorKind::Incomplete { missing_count: 1 }
        ));

        let extra_authority = root_file_grant_authority_for_test(root.path());
        let extra_request = requested();
        let extra_key = SnapshotRestoreResourceKey::new(
            extra_request.root_key.device_key(),
            SnapshotRestorePublicId::try_from("other-root").expect("extra ID should validate"),
            SnapshotRestoreResourceClass::BlockBacking,
        );
        let extra = extra_request
            .prepare_root_with_key(extra_key, || false, |_| contained_root(&extra_authority))
            .expect_err("swapped public identity should be extra");
        assert_eq!(extra.stage, SnapshotRestoreResourceStage::Binding);
        assert!(matches!(
            extra.kind,
            SnapshotRestoreResourceErrorKind::Binding(
                SnapshotRestoreBindingRejectionReason::ExtraBinding
            )
        ));
        assert_root_claim_restored(&extra_authority);

        let wrong_class_authority = root_file_grant_authority_for_test(root.path());
        let wrong_class_request = requested();
        let wrong_class_key = SnapshotRestoreResourceKey::new(
            wrong_class_request.root_key.device_key(),
            wrong_class_request.root_key.public_id().clone(),
            SnapshotRestoreResourceClass::VsockEndpoint,
        );
        let wrong_class = wrong_class_request
            .prepare_root_with_key(
                wrong_class_key,
                || false,
                |_| contained_root(&wrong_class_authority),
            )
            .expect_err("root owner under a vsock key should fail");
        assert_eq!(wrong_class.stage, SnapshotRestoreResourceStage::Binding);
        assert!(matches!(
            wrong_class.kind,
            SnapshotRestoreResourceErrorKind::OwnerClassMismatch
        ));
        assert_root_claim_restored(&wrong_class_authority);

        let first_authority = root_file_grant_authority_for_test(root.path());
        let second_authority = root_file_grant_authority_for_test(root.path());
        let mut duplicate_request = requested();
        let duplicate_key = duplicate_request.root_key.clone();
        duplicate_request
            .bindings
            .bind(
                &duplicate_key,
                PreparedSnapshotRestoreResource::Root(
                    contained_root(&first_authority).expect("first duplicate owner should prepare"),
                ),
            )
            .expect("first exact owner should bind");
        let duplicate = duplicate_request
            .prepare_root_with_key(
                duplicate_key,
                || false,
                |_| contained_root(&second_authority),
            )
            .expect_err("second exact owner should be duplicate");
        assert_eq!(duplicate.stage, SnapshotRestoreResourceStage::Binding);
        assert!(matches!(
            duplicate.kind,
            SnapshotRestoreResourceErrorKind::Binding(
                SnapshotRestoreBindingRejectionReason::DuplicateBinding
            )
        ));
        assert_root_claim_restored(&first_authority);
        assert_root_claim_restored(&second_authority);

        let swapped_take_authority = root_file_grant_authority_for_test(root.path());
        let mut swapped_take = requested()
            .prepare_root_with(|| false, |_| contained_root(&swapped_take_authority))
            .expect("swapped-take batch should prepare");
        swapped_take.root_key = SnapshotRestoreResourceKey::new(
            swapped_take.root_key.device_key(),
            SnapshotRestorePublicId::try_from("swapped-root")
                .expect("swapped take ID should validate"),
            SnapshotRestoreResourceClass::BlockBacking,
        );
        let construction_called = Cell::new(false);
        let error = swapped_take
            .consume_root_with(|_| {
                construction_called.set(true);
            })
            .expect_err("swapped exact take should fail before construction");
        assert!(!construction_called.get());
        assert_eq!(error.stage, SnapshotRestoreResourceStage::Take);
        assert!(matches!(
            error.kind,
            SnapshotRestoreResourceErrorKind::Take(SnapshotRestoreTakeError::UnknownResource)
        ));
        assert_root_claim_restored(&swapped_take_authority);

        let repeated_authority = root_file_grant_authority_for_test(root.path());
        let mut repeated = requested()
            .prepare_root_with(|| false, |_| contained_root(&repeated_authority))
            .expect("repeated-take batch should prepare");
        let repeated_key = repeated.root_key.clone();
        let repeated_root = repeated
            .take_root(&repeated_key)
            .expect("first exact take should succeed");
        let error = repeated
            .take_root(&repeated_key)
            .expect_err("second exact take should fail");
        assert_eq!(error.stage, SnapshotRestoreResourceStage::Take);
        assert!(matches!(
            error.kind,
            SnapshotRestoreResourceErrorKind::Take(SnapshotRestoreTakeError::AlreadyTaken)
        ));
        repeated
            .finish()
            .expect("taken singleton should leave no unconsumed owner");
        repeated_root
            .abort()
            .expect("taken owner should still abort explicitly");
        assert_root_claim_restored(&repeated_authority);

        let unconsumed_authority = root_file_grant_authority_for_test(root.path());
        let unconsumed = requested()
            .prepare_root_with(|| false, |_| contained_root(&unconsumed_authority))
            .expect("unconsumed batch should prepare")
            .finish()
            .expect_err("finish should reject an unconsumed root");
        assert_eq!(unconsumed.stage, SnapshotRestoreResourceStage::Finish);
        assert!(matches!(
            unconsumed.kind,
            SnapshotRestoreResourceErrorKind::Unconsumed {
                unconsumed_count: 1
            }
        ));
        assert_root_claim_restored(&unconsumed_authority);
    }

    #[test]
    fn provider_and_batch_diagnostics_redact_private_values() {
        let private_path = unique_path("private-selector-do-not-render");
        let error = PreparedSnapshotRootRestoreResource::prepare(&private_path, None)
            .expect_err("missing private selector should fail");
        let diagnostics = format!("{error:?} {error} {:?}", requested());
        assert!(!diagnostics.contains(&private_path.display().to_string()));
        assert!(!diagnostics.contains("private-selector-do-not-render"));
        assert!(!diagnostics.contains(ROOT_REFERENCE));
        assert!(!diagnostics.contains("root-selector"));
        assert!(!diagnostics.contains("rootfs"));
    }
}
