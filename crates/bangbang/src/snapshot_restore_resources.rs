//! Process-owned destination resources for native-v2 snapshot restore.

use std::collections::{HashSet, TryReserveError};
use std::fmt;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Instant;

use bangbang_runtime::block::async_executor::BlockAsyncRuntimeError;
use bangbang_runtime::block::{
    BlockFileBacking, BlockFileBackingIdentity, DriveConfigs, SnapshotBlockFileBackingError,
    SnapshotBlockFileBackingReservation,
};
use bangbang_runtime::memory::GuestMemory;
use bangbang_runtime::pmem::{
    PmemFileBacking, SnapshotPmemFileBackingError, SnapshotPmemFileBackingReservation,
    aligned_pmem_mapping_len,
};
use bangbang_runtime::snapshot_device_v2::SnapshotV2DeviceGraph;
use bangbang_runtime::snapshot_device_v2_5::{
    PreparedSnapshotV2MultiBlockBundle, SnapshotV2MultiBlockBundleError,
    SnapshotV2MultiBlockCleanupError, SnapshotV2MultiBlockDeviceGraph,
    SnapshotV2MultiBlockRestorePlan, SnapshotV2MultiBlockRestorePlanError,
};
use bangbang_runtime::snapshot_device_v2_6::{
    PreparedSnapshotV2StorageBundle, SnapshotV2StorageBundleError, SnapshotV2StorageCleanupError,
    SnapshotV2StorageDeviceGraph, SnapshotV2StorageRestorePlan, SnapshotV2StorageRestorePlanError,
};
use bangbang_runtime::snapshot_restore::{
    PreparedSnapshotRestoreBindings, SnapshotRestoreBindingAllocationError,
    SnapshotRestoreBindingRejectionReason, SnapshotRestoreBindings, SnapshotRestoreManifest,
    SnapshotRestoreManifestError, SnapshotRestorePublicId, SnapshotRestoreResourceClass,
    SnapshotRestoreResourceKey, SnapshotRestoreTakeError,
};
use bangbang_session::GrantAccess;
use bangbang_session::macos::runtime::WorkerSocketNamespace;

use crate::contained_session::{
    ContainedSnapshotRestoreAuthority, ContainedSnapshotRestoreError,
    ContainedSnapshotRestoreFileRequest, ContainedSnapshotRestoreTransaction, GrantAuthority,
    GrantClaimError, PreparedDriveBackingClaim, PreparedSnapshotBackingClaim, grant_reference_id,
};
#[cfg(test)]
use crate::contained_session::{
    ContainedSnapshotRestoreDriveRequest, DirectoryGrantAuthority, SocketBrokerAuthority,
};
use crate::vsock_restore::{
    LocallyPreparedVsockRestoreResource, PreparedVsockRestoreResource,
    RequestedVsockRestoreResource, ReservedVsockRestoreResource, VsockRestoreDisposition,
    VsockRestoreError, VsockRestoreStage,
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
    ContainedReservation,
    DrivePreflight,
    DrivePreparation,
    PmemPreparation,
    StoragePreflight,
    RootPreparation,
    VsockPreparation,
    Binding,
    Completion,
    Take,
    Finish,
}

pub(crate) enum SnapshotRestoreResourceErrorKind {
    Manifest(SnapshotRestoreManifestError),
    BindingAllocation(SnapshotRestoreBindingAllocationError),
    Contained(ContainedSnapshotRestoreError),
    RootBacking(SnapshotRootBackingLeaseError),
    DriveBacking(SnapshotDriveBackingPreparationError),
    PmemBacking(SnapshotPmemFileBackingError),
    Vsock(VsockRestoreError),
    Binding(SnapshotRestoreBindingRejectionReason),
    Incomplete { missing_count: usize },
    Take(SnapshotRestoreTakeError),
    Unconsumed { unconsumed_count: usize },
    OwnerClassMismatch,
    InvalidDriveSet,
    InvalidStorageSet,
    DriveProjection,
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
            Self::Contained(source) => formatter
                .debug_tuple("SnapshotRestoreResourceErrorKind::Contained")
                .field(source)
                .finish(),
            Self::RootBacking(source) => formatter
                .debug_tuple("SnapshotRestoreResourceErrorKind::RootBacking")
                .field(source)
                .finish(),
            Self::DriveBacking(source) => formatter
                .debug_tuple("SnapshotRestoreResourceErrorKind::DriveBacking")
                .field(source)
                .finish(),
            Self::PmemBacking(source) => formatter
                .debug_tuple("SnapshotRestoreResourceErrorKind::PmemBacking")
                .field(source)
                .finish(),
            Self::Vsock(source) => formatter
                .debug_tuple("SnapshotRestoreResourceErrorKind::Vsock")
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
            Self::InvalidDriveSet => {
                formatter.write_str("SnapshotRestoreResourceErrorKind::InvalidDriveSet")
            }
            Self::InvalidStorageSet => {
                formatter.write_str("SnapshotRestoreResourceErrorKind::InvalidStorageSet")
            }
            Self::DriveProjection => {
                formatter.write_str("SnapshotRestoreResourceErrorKind::DriveProjection")
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
            Self::Contained(source) => source.fmt(formatter),
            Self::RootBacking(source) => source.fmt(formatter),
            Self::DriveBacking(source) => source.fmt(formatter),
            Self::PmemBacking(source) => source.fmt(formatter),
            Self::Vsock(source) => source.fmt(formatter),
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
            Self::InvalidDriveSet => {
                formatter.write_str("snapshot restore drive resource set is invalid")
            }
            Self::InvalidStorageSet => {
                formatter.write_str("snapshot restore storage resource set is invalid")
            }
            Self::DriveProjection => {
                formatter.write_str("snapshot restore drive projection is invalid")
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

    fn with_abort_outcome(mut self, outcome: SnapshotRestoreAbortOutcome) -> Self {
        if outcome.disposition == SnapshotRestoreResourceDisposition::Terminal {
            self.disposition = SnapshotRestoreResourceDisposition::Terminal;
        }
        if outcome.cleanup_failed {
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
            SnapshotRestoreResourceErrorKind::Contained(source) => Some(source),
            SnapshotRestoreResourceErrorKind::RootBacking(source) => Some(source),
            SnapshotRestoreResourceErrorKind::DriveBacking(source) => Some(source),
            SnapshotRestoreResourceErrorKind::PmemBacking(source) => Some(source),
            SnapshotRestoreResourceErrorKind::Vsock(source) => Some(source),
            SnapshotRestoreResourceErrorKind::Take(source) => Some(source),
            SnapshotRestoreResourceErrorKind::Binding(_)
            | SnapshotRestoreResourceErrorKind::Incomplete { .. }
            | SnapshotRestoreResourceErrorKind::Unconsumed { .. }
            | SnapshotRestoreResourceErrorKind::OwnerClassMismatch
            | SnapshotRestoreResourceErrorKind::InvalidDriveSet
            | SnapshotRestoreResourceErrorKind::InvalidStorageSet
            | SnapshotRestoreResourceErrorKind::DriveProjection
            | SnapshotRestoreResourceErrorKind::Cancelled => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SnapshotRestoreAbortOutcome {
    disposition: SnapshotRestoreResourceDisposition,
    cleanup_failed: bool,
}

impl SnapshotRestoreAbortOutcome {
    const fn retryable() -> Self {
        Self {
            disposition: SnapshotRestoreResourceDisposition::Retryable,
            cleanup_failed: false,
        }
    }

    const fn terminal(cleanup_failed: bool) -> Self {
        Self {
            disposition: SnapshotRestoreResourceDisposition::Terminal,
            cleanup_failed,
        }
    }

    fn merge(self, other: Self) -> Self {
        Self {
            disposition: if self.disposition == SnapshotRestoreResourceDisposition::Terminal
                || other.disposition == SnapshotRestoreResourceDisposition::Terminal
            {
                SnapshotRestoreResourceDisposition::Terminal
            } else {
                SnapshotRestoreResourceDisposition::Retryable
            },
            cleanup_failed: self.cleanup_failed || other.cleanup_failed,
        }
    }
}

fn snapshot_vsock_error(source: VsockRestoreError) -> SnapshotRestoreResourceError {
    let disposition = match source.disposition() {
        VsockRestoreDisposition::Retryable => SnapshotRestoreResourceDisposition::Retryable,
        VsockRestoreDisposition::Terminal => SnapshotRestoreResourceDisposition::Terminal,
    };
    SnapshotRestoreResourceError {
        stage: SnapshotRestoreResourceStage::VsockPreparation,
        kind: SnapshotRestoreResourceErrorKind::Vsock(source),
        disposition,
        cleanup_failed: source.stage() == VsockRestoreStage::Cleanup,
    }
}

fn snapshot_contained_error(source: ContainedSnapshotRestoreError) -> SnapshotRestoreResourceError {
    SnapshotRestoreResourceError {
        stage: SnapshotRestoreResourceStage::ContainedReservation,
        kind: SnapshotRestoreResourceErrorKind::Contained(source),
        disposition: if source.is_terminal() {
            SnapshotRestoreResourceDisposition::Terminal
        } else {
            SnapshotRestoreResourceDisposition::Retryable
        },
        cleanup_failed: source.cleanup_failed(),
    }
}

fn vsock_abort_outcome(
    result: Result<VsockRestoreDisposition, VsockRestoreError>,
) -> SnapshotRestoreAbortOutcome {
    match result {
        Ok(VsockRestoreDisposition::Retryable) => SnapshotRestoreAbortOutcome::retryable(),
        Ok(VsockRestoreDisposition::Terminal) => SnapshotRestoreAbortOutcome::terminal(false),
        Err(_) => SnapshotRestoreAbortOutcome::terminal(true),
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

    fn from_prepared_claim(selector: PathBuf, claim: PreparedDriveBackingClaim) -> Self {
        Self {
            selector: Some(selector),
            claim: Some(claim),
            consumed: false,
        }
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

#[derive(Debug)]
pub(crate) enum SnapshotDriveBackingPreparationError {
    Authority(GrantClaimError),
    Backing(SnapshotBlockFileBackingError),
}

impl fmt::Display for SnapshotDriveBackingPreparationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Authority(_) => {
                formatter.write_str("snapshot drive backing authority validation failed")
            }
            Self::Backing(_) => {
                formatter.write_str("snapshot drive backing descriptor validation failed")
            }
        }
    }
}

impl std::error::Error for SnapshotDriveBackingPreparationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Authority(source) => Some(source),
            Self::Backing(source) => Some(source),
        }
    }
}

pub(crate) struct PreparedSnapshotRootRestoreCompletion {
    lease: PreparedSnapshotRootBackingLease,
    contained_transaction: Option<ContainedSnapshotRestoreTransaction>,
}

impl PreparedSnapshotRootRestoreCompletion {
    pub(crate) fn commit(self) -> Result<(), PreparedSnapshotRootRestoreCompletionError> {
        let Self {
            lease,
            contained_transaction,
        } = self;
        lease.commit();
        match contained_transaction {
            Some(transaction) => {
                transaction
                    .commit()
                    .map_err(|source| PreparedSnapshotRootRestoreCompletionError {
                        grant: None,
                        contained: Some(source),
                    })
            }
            None => Ok(()),
        }
    }

    pub(crate) fn abort(self) -> Result<(), PreparedSnapshotRootRestoreCompletionError> {
        let Self {
            lease,
            contained_transaction,
        } = self;
        let grant = lease.abort().err();
        let contained = contained_transaction.and_then(|transaction| transaction.abort().err());
        if grant.is_some() || contained.is_some() {
            Err(PreparedSnapshotRootRestoreCompletionError { grant, contained })
        } else {
            Ok(())
        }
    }

    fn with_contained_transaction(
        mut self,
        transaction: ContainedSnapshotRestoreTransaction,
    ) -> Self {
        debug_assert!(self.contained_transaction.is_none());
        self.contained_transaction = Some(transaction);
        self
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

#[derive(Debug)]
pub(crate) struct PreparedSnapshotRootRestoreCompletionError {
    grant: Option<GrantClaimError>,
    contained: Option<ContainedSnapshotRestoreError>,
}

impl fmt::Display for PreparedSnapshotRootRestoreCompletionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("snapshot root completion authority failed")
    }
}

impl std::error::Error for PreparedSnapshotRootRestoreCompletionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.contained
            .as_ref()
            .map(|source| source as &(dyn std::error::Error + 'static))
            .or_else(|| {
                self.grant
                    .as_ref()
                    .map(|source| source as &(dyn std::error::Error + 'static))
            })
    }
}

struct ReservedSnapshotRootRestoreResource {
    lease: PreparedSnapshotRootBackingLease,
}

impl ReservedSnapshotRootRestoreResource {
    fn reserve(
        selector: &Path,
        authority: Option<&GrantAuthority>,
    ) -> Result<Self, SnapshotRestoreResourceError> {
        let lease = PreparedSnapshotRootBackingLease::prepare(
            selector,
            authority,
            if authority.is_some() {
                SnapshotRootSelectorPolicy::RequireGrantWhenContained
            } else {
                SnapshotRootSelectorPolicy::RejectGrantReference
            },
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
        Ok(Self { lease })
    }

    fn from_prepared_claim(selector: PathBuf, claim: PreparedDriveBackingClaim) -> Self {
        Self {
            lease: PreparedSnapshotRootBackingLease::from_prepared_claim(selector, claim),
        }
    }

    fn prepare_local(
        mut self,
    ) -> Result<
        PreparedSnapshotRootRestoreResource,
        Box<(
            SnapshotRootBackingLeaseError,
            ReservedSnapshotRootRestoreResource,
        )>,
    > {
        match self.lease.open_snapshot_read_only() {
            Ok(backing) => Ok(PreparedSnapshotRootRestoreResource {
                backing,
                completion: PreparedSnapshotRootRestoreCompletion {
                    lease: self.lease,
                    contained_transaction: None,
                },
            }),
            Err(source) => Err(Box::new((source, self))),
        }
    }

    fn abort(self) -> SnapshotRestoreAbortOutcome {
        match self.lease.abort() {
            Ok(()) => SnapshotRestoreAbortOutcome::retryable(),
            Err(_) => SnapshotRestoreAbortOutcome::terminal(true),
        }
    }
}

impl fmt::Debug for ReservedSnapshotRootRestoreResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReservedSnapshotRootRestoreResource")
            .field("state", &"<reserved>")
            .finish()
    }
}

pub(crate) struct PreparedSnapshotRootRestoreResource {
    backing: BlockFileBacking,
    completion: PreparedSnapshotRootRestoreCompletion,
}

impl PreparedSnapshotRootRestoreResource {
    #[cfg(test)]
    fn prepare(
        selector: &Path,
        authority: Option<&GrantAuthority>,
    ) -> Result<Self, SnapshotRestoreResourceError> {
        let reserved = ReservedSnapshotRootRestoreResource::reserve(selector, authority)?;
        reserved.prepare_local().map_err(|failure| {
            let (source, reserved) = *failure;
            SnapshotRestoreResourceError::retryable(
                SnapshotRestoreResourceStage::RootPreparation,
                SnapshotRestoreResourceErrorKind::RootBacking(source),
            )
            .with_abort_outcome(reserved.abort())
        })
    }

    pub(crate) fn into_parts(self) -> (BlockFileBacking, PreparedSnapshotRootRestoreCompletion) {
        (self.backing, self.completion)
    }

    fn with_contained_transaction(
        mut self,
        transaction: ContainedSnapshotRestoreTransaction,
    ) -> Self {
        self.completion = self.completion.with_contained_transaction(transaction);
        self
    }

    fn abort(self) -> Result<(), PreparedSnapshotRootRestoreCompletionError> {
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
    Vsock(PreparedVsockRestoreResource),
}

impl PreparedSnapshotRestoreResource {
    const fn resource_class(&self) -> SnapshotRestoreResourceClass {
        match self {
            Self::Root(_) => SnapshotRestoreResourceClass::BlockBacking,
            Self::Vsock(_) => SnapshotRestoreResourceClass::VsockEndpoint,
        }
    }

    fn abort(self) -> SnapshotRestoreAbortOutcome {
        match self {
            Self::Root(root) => match root.abort() {
                Ok(()) => SnapshotRestoreAbortOutcome::retryable(),
                Err(_) => SnapshotRestoreAbortOutcome::terminal(true),
            },
            Self::Vsock(vsock) => vsock_abort_outcome(vsock.abort()),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotRestoreFileKind {
    Block,
    Pmem { expected_mapped_len: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotRestoreFileSetKind {
    MultiBlock,
    Storage,
}

impl SnapshotRestoreFileSetKind {
    const fn preflight_stage(self) -> SnapshotRestoreResourceStage {
        match self {
            Self::MultiBlock => SnapshotRestoreResourceStage::DrivePreflight,
            Self::Storage => SnapshotRestoreResourceStage::StoragePreflight,
        }
    }

    const fn invalid_set(self) -> SnapshotRestoreResourceErrorKind {
        match self {
            Self::MultiBlock => SnapshotRestoreResourceErrorKind::InvalidDriveSet,
            Self::Storage => SnapshotRestoreResourceErrorKind::InvalidStorageSet,
        }
    }
}

impl SnapshotRestoreFileKind {
    const fn resource_class(self) -> SnapshotRestoreResourceClass {
        match self {
            Self::Block => SnapshotRestoreResourceClass::BlockBacking,
            Self::Pmem { .. } => SnapshotRestoreResourceClass::PmemBacking,
        }
    }

    const fn grant_role(self) -> bangbang_session::ResourceRole {
        match self {
            Self::Block => bangbang_session::ResourceRole::DriveBacking,
            Self::Pmem { .. } => bangbang_session::ResourceRole::PmemBacking,
        }
    }

    const fn expected_mapped_len(self) -> Option<u64> {
        match self {
            Self::Block => None,
            Self::Pmem {
                expected_mapped_len,
            } => Some(expected_mapped_len),
        }
    }
}

struct RequestedSnapshotDriveRestoreResource {
    key: SnapshotRestoreResourceKey,
    selector: PathBuf,
    is_read_only: bool,
    expected_len: u64,
    kind: SnapshotRestoreFileKind,
}

impl fmt::Debug for RequestedSnapshotDriveRestoreResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RequestedSnapshotDriveRestoreResource")
            .field("class", &self.kind.resource_class())
            .field("state", &"<redacted>")
            .finish()
    }
}

enum PreparedSnapshotFileBacking {
    Block(BlockFileBacking),
    Pmem(PmemFileBacking),
}

struct PreparedSnapshotDriveRestoreResource {
    key: SnapshotRestoreResourceKey,
    backing: PreparedSnapshotFileBacking,
    claim: Option<PreparedSnapshotBackingClaim>,
}

impl PreparedSnapshotDriveRestoreResource {
    fn abort(self) -> SnapshotRestoreAbortOutcome {
        let Self {
            key: _,
            backing,
            claim,
        } = self;
        drop(backing);
        match claim {
            Some(claim) => {
                if claim.abort().is_err() {
                    SnapshotRestoreAbortOutcome::terminal(true)
                } else {
                    SnapshotRestoreAbortOutcome::retryable()
                }
            }
            None => SnapshotRestoreAbortOutcome::retryable(),
        }
    }
}

enum ReservedDirectSnapshotFileBacking {
    Block(SnapshotBlockFileBackingReservation),
    Pmem(SnapshotPmemFileBackingReservation),
}

struct ReservedDirectSnapshotDriveRestoreResource {
    key: SnapshotRestoreResourceKey,
    reservation: ReservedDirectSnapshotFileBacking,
}

impl fmt::Debug for PreparedSnapshotDriveRestoreResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotDriveRestoreResource")
            .field(
                "class",
                &match &self.backing {
                    PreparedSnapshotFileBacking::Block(_) => {
                        SnapshotRestoreResourceClass::BlockBacking
                    }
                    PreparedSnapshotFileBacking::Pmem(_) => {
                        SnapshotRestoreResourceClass::PmemBacking
                    }
                },
            )
            .field("backing", &"<owned>")
            .field("claim", &self.claim.as_ref().map(|_| "<provisional>"))
            .finish()
    }
}

/// Complete profile-2 block resource request before host authority is touched.
pub(crate) struct RequestedSnapshotMultiDriveRestoreResources {
    drives: Vec<RequestedSnapshotDriveRestoreResource>,
    drive_keys: Vec<SnapshotRestoreResourceKey>,
    drive_configs: DriveConfigs,
    bindings: SnapshotRestoreBindings<PreparedSnapshotDriveRestoreResource>,
    file_set_kind: SnapshotRestoreFileSetKind,
}

impl RequestedSnapshotMultiDriveRestoreResources {
    pub(crate) fn try_from_native_v2_multi_block_device_graph(
        graph: &SnapshotV2MultiBlockDeviceGraph,
    ) -> Result<Self, SnapshotRestoreResourceError> {
        let drive_configs = graph.project_drive_configs().map_err(|_| {
            SnapshotRestoreResourceError::terminal(
                SnapshotRestoreResourceStage::DrivePreflight,
                SnapshotRestoreResourceErrorKind::DriveProjection,
            )
        })?;
        let mut drives = Vec::new();
        let mut drive_keys = Vec::new();
        let mut resources = Vec::new();
        drives
            .try_reserve_exact(graph.records().len())
            .map_err(manifest_allocation_error)?;
        drive_keys
            .try_reserve_exact(graph.records().len())
            .map_err(manifest_allocation_error)?;
        resources
            .try_reserve_exact(graph.records().len())
            .map_err(manifest_allocation_error)?;
        for record in graph.records() {
            let public_id = SnapshotRestorePublicId::try_from(record.config().drive_id()).map_err(
                |source| {
                    SnapshotRestoreResourceError::retryable(
                        SnapshotRestoreResourceStage::Manifest,
                        SnapshotRestoreResourceErrorKind::Manifest(
                            SnapshotRestoreManifestError::PublicId { source },
                        ),
                    )
                },
            )?;
            let key = SnapshotRestoreResourceKey::new(
                record.key(),
                public_id,
                SnapshotRestoreResourceClass::BlockBacking,
            );
            drives.push(RequestedSnapshotDriveRestoreResource {
                key: key.clone(),
                selector: PathBuf::from(record.config().selector()),
                is_read_only: record.config().is_read_only(),
                expected_len: record.block().backing_bytes(),
                kind: SnapshotRestoreFileKind::Block,
            });
            drive_keys.push(key.clone());
            resources.push(key);
        }
        let manifest =
            SnapshotRestoreManifest::try_new(resources, Vec::new()).map_err(|source| {
                SnapshotRestoreResourceError::retryable(
                    SnapshotRestoreResourceStage::Manifest,
                    SnapshotRestoreResourceErrorKind::Manifest(source),
                )
            })?;
        if manifest.len() != drives.len()
            || drives.is_empty()
            || drive_configs.as_slice().len() != drives.len()
            || drives
                .iter()
                .zip(graph.records())
                .zip(drive_configs.as_slice())
                .any(|((drive, record), config)| {
                    drive.key.device_key() != record.key()
                        || drive.key.public_id().as_str() != record.config().drive_id()
                        || drive.selector != Path::new(record.config().selector())
                        || drive.is_read_only != record.config().is_read_only()
                        || drive.expected_len != record.block().backing_bytes()
                        || drive.kind != SnapshotRestoreFileKind::Block
                        || config.drive_id() != record.config().drive_id()
                })
        {
            return Err(SnapshotRestoreResourceError::terminal(
                SnapshotRestoreResourceStage::DrivePreflight,
                SnapshotRestoreResourceErrorKind::InvalidDriveSet,
            ));
        }
        let bindings = manifest.try_into_bindings().map_err(|source| {
            SnapshotRestoreResourceError::retryable(
                SnapshotRestoreResourceStage::BindingAllocation,
                SnapshotRestoreResourceErrorKind::BindingAllocation(source),
            )
        })?;
        Ok(Self {
            drives,
            drive_keys,
            drive_configs,
            bindings,
            file_set_kind: SnapshotRestoreFileSetKind::MultiBlock,
        })
    }

    pub(crate) fn prepare(
        self,
        authority: Option<&ContainedSnapshotRestoreAuthority>,
        cancelled: impl Fn() -> bool,
    ) -> Result<PreparedSnapshotMultiDriveRestoreResources, SnapshotRestoreResourceError> {
        let Self {
            drives,
            drive_keys,
            drive_configs,
            mut bindings,
            file_set_kind,
        } = self;
        if cancelled() {
            return Err(cancelled_batch_error());
        }

        let (prepared, contained_transaction) = match authority {
            Some(authority) => {
                let mut requests = Vec::new();
                requests
                    .try_reserve_exact(drives.len())
                    .map_err(manifest_allocation_error)?;
                for drive in &drives {
                    requests.push(ContainedSnapshotRestoreFileRequest::new(
                        &drive.selector,
                        drive.kind.grant_role(),
                        if drive.is_read_only {
                            GrantAccess::ReadOnly
                        } else {
                            GrantAccess::ReadWrite
                        },
                        Some(drive.expected_len),
                    ));
                }
                let reserved = authority
                    .prepare_files(&requests, None, &cancelled)
                    .map_err(snapshot_contained_error)?;
                let (claims, vsock, transaction) = reserved
                    .into_file_parts()
                    .map_err(snapshot_contained_error)?;
                if vsock.is_some() || claims.len() != drives.len() {
                    let outcome = abort_contained_vsock_facets(vsock)
                        .merge(abort_prepared_drive_claims(claims))
                        .merge(abort_contained_transaction(Some(transaction)));
                    return Err(SnapshotRestoreResourceError::terminal(
                        file_set_kind.preflight_stage(),
                        file_set_kind.invalid_set(),
                    )
                    .with_abort_outcome(outcome));
                }
                let prepared =
                    match prepare_contained_drives(&drives, claims, file_set_kind, &cancelled) {
                        Ok(prepared) => prepared,
                        Err(failure) => {
                            let (source, resources, claims) = *failure;
                            return Err(source.with_abort_outcome(
                                abort_prepared_drive_claims(claims)
                                    .merge(abort_prepared_drive_resources(resources))
                                    .merge(abort_contained_transaction(Some(transaction))),
                            ));
                        }
                    };
                (prepared, Some(transaction))
            }
            None => (
                prepare_direct_drives(&drives, file_set_kind, &cancelled)?,
                None,
            ),
        };

        if cancelled() {
            let outcome = abort_prepared_drive_resources(prepared)
                .merge(abort_contained_transaction(contained_transaction));
            return Err(cancelled_batch_error().with_abort_outcome(outcome));
        }
        if prepared.len() != drive_keys.len()
            || prepared
                .iter()
                .zip(&drive_keys)
                .any(|(owner, key)| owner.key != *key)
        {
            let outcome = abort_prepared_drive_resources(prepared)
                .merge(abort_contained_transaction(contained_transaction));
            return Err(SnapshotRestoreResourceError::terminal(
                SnapshotRestoreResourceStage::Binding,
                file_set_kind.invalid_set(),
            )
            .with_abort_outcome(outcome));
        }
        let mut prepared = prepared.into_iter();
        for key in &drive_keys {
            let Some(owner) = prepared.next() else {
                let outcome = abort_prepared_drive_resource_iter(prepared)
                    .merge(abort_drive_bindings(bindings.into_values()))
                    .merge(abort_contained_transaction(contained_transaction));
                return Err(SnapshotRestoreResourceError::terminal(
                    SnapshotRestoreResourceStage::Binding,
                    file_set_kind.invalid_set(),
                )
                .with_abort_outcome(outcome));
            };
            if owner.key != *key {
                let outcome = abort_prepared_drive_resource_iter(prepared)
                    .merge(owner.abort())
                    .merge(abort_drive_bindings(bindings.into_values()))
                    .merge(abort_contained_transaction(contained_transaction));
                return Err(SnapshotRestoreResourceError::terminal(
                    SnapshotRestoreResourceStage::Binding,
                    file_set_kind.invalid_set(),
                )
                .with_abort_outcome(outcome));
            }
            if let Err(rejection) = bindings.bind(key, owner) {
                let reason = rejection.reason();
                let outcome = abort_prepared_drive_resource_iter(prepared)
                    .merge(rejection.into_value().abort())
                    .merge(abort_drive_bindings(bindings.into_values()))
                    .merge(abort_contained_transaction(contained_transaction));
                return Err(SnapshotRestoreResourceError::terminal(
                    SnapshotRestoreResourceStage::Binding,
                    SnapshotRestoreResourceErrorKind::Binding(reason),
                )
                .with_abort_outcome(outcome));
            }
        }
        if let Some(extra) = prepared.next() {
            let outcome =
                abort_prepared_drive_resource_iter(std::iter::once(extra).chain(prepared))
                    .merge(abort_drive_bindings(bindings.into_values()))
                    .merge(abort_contained_transaction(contained_transaction));
            return Err(SnapshotRestoreResourceError::terminal(
                SnapshotRestoreResourceStage::Binding,
                file_set_kind.invalid_set(),
            )
            .with_abort_outcome(outcome));
        }
        let bindings = match bindings.complete() {
            Ok(bindings) => bindings,
            Err(incomplete) => {
                let missing_count = incomplete.missing_count();
                let outcome = abort_drive_bindings(incomplete.into_bindings().into_values())
                    .merge(abort_contained_transaction(contained_transaction));
                return Err(SnapshotRestoreResourceError::terminal(
                    SnapshotRestoreResourceStage::Completion,
                    SnapshotRestoreResourceErrorKind::Incomplete { missing_count },
                )
                .with_abort_outcome(outcome));
            }
        };
        let prepared = PreparedSnapshotMultiDriveRestoreResources {
            drive_keys,
            drive_configs,
            bindings,
            contained_transaction,
            file_set_kind,
        };
        if cancelled() {
            let outcome = prepared.abort();
            return Err(cancelled_batch_error().with_abort_outcome(outcome));
        }
        Ok(prepared)
    }
}

/// Complete exact-2.6 block+pmem resource request before host authority is
/// touched.
pub(crate) struct RequestedSnapshotStorageRestoreResources {
    resources: RequestedSnapshotMultiDriveRestoreResources,
    block_count: usize,
    pmem_count: usize,
}

impl RequestedSnapshotStorageRestoreResources {
    pub(crate) fn try_from_native_v2_storage_device_graph(
        graph: &SnapshotV2StorageDeviceGraph,
    ) -> Result<Self, SnapshotRestoreResourceError> {
        let manifest =
            SnapshotRestoreManifest::try_from_native_v2_storage_device_graph(graph, Vec::new())
                .map_err(|source| {
                    SnapshotRestoreResourceError::retryable(
                        SnapshotRestoreResourceStage::Manifest,
                        SnapshotRestoreResourceErrorKind::Manifest(source),
                    )
                })?;
        let mut drives = Vec::new();
        let mut drive_keys = Vec::new();
        drives
            .try_reserve_exact(graph.record_count())
            .map_err(manifest_allocation_error)?;
        drive_keys
            .try_reserve_exact(graph.record_count())
            .map_err(manifest_allocation_error)?;
        for record in graph.block_records() {
            let public_id = SnapshotRestorePublicId::try_from(record.config().drive_id()).map_err(
                |source| {
                    SnapshotRestoreResourceError::retryable(
                        SnapshotRestoreResourceStage::Manifest,
                        SnapshotRestoreResourceErrorKind::Manifest(
                            SnapshotRestoreManifestError::PublicId { source },
                        ),
                    )
                },
            )?;
            let key = SnapshotRestoreResourceKey::new(
                record.key(),
                public_id,
                SnapshotRestoreResourceClass::BlockBacking,
            );
            drives.push(RequestedSnapshotDriveRestoreResource {
                key: key.clone(),
                selector: PathBuf::from(record.config().selector()),
                is_read_only: record.config().is_read_only(),
                expected_len: record.block().backing_bytes(),
                kind: SnapshotRestoreFileKind::Block,
            });
            drive_keys.push(key);
        }
        for record in graph.pmem_records() {
            let public_id =
                SnapshotRestorePublicId::try_from(record.config().pmem_id()).map_err(|source| {
                    SnapshotRestoreResourceError::retryable(
                        SnapshotRestoreResourceStage::Manifest,
                        SnapshotRestoreResourceErrorKind::Manifest(
                            SnapshotRestoreManifestError::PublicId { source },
                        ),
                    )
                })?;
            let key = SnapshotRestoreResourceKey::new(
                record.key(),
                public_id,
                SnapshotRestoreResourceClass::PmemBacking,
            );
            drives.push(RequestedSnapshotDriveRestoreResource {
                key: key.clone(),
                selector: PathBuf::from(record.config().selector()),
                is_read_only: record.config().is_read_only(),
                expected_len: record.pmem().file_bytes(),
                kind: SnapshotRestoreFileKind::Pmem {
                    expected_mapped_len: record.pmem().mapped_bytes(),
                },
            });
            drive_keys.push(key);
        }
        if drives.is_empty()
            || drives.len() != graph.record_count()
            || manifest.resources() != drive_keys.as_slice()
            || drives.iter().zip(&drive_keys).any(|(drive, key)| {
                drive.key != *key
                    || drive.key.resource_class() != drive.kind.resource_class()
                    || drive.expected_len == 0
                    || drive.kind.expected_mapped_len().is_some_and(|expected| {
                        aligned_pmem_mapping_len(drive.expected_len) != Some(expected)
                    })
            })
        {
            return Err(SnapshotRestoreResourceError::terminal(
                SnapshotRestoreResourceStage::StoragePreflight,
                SnapshotRestoreResourceErrorKind::InvalidStorageSet,
            ));
        }
        let bindings = manifest.try_into_bindings().map_err(|source| {
            SnapshotRestoreResourceError::retryable(
                SnapshotRestoreResourceStage::BindingAllocation,
                SnapshotRestoreResourceErrorKind::BindingAllocation(source),
            )
        })?;
        Ok(Self {
            resources: RequestedSnapshotMultiDriveRestoreResources {
                drives,
                drive_keys,
                drive_configs: DriveConfigs::new(),
                bindings,
                file_set_kind: SnapshotRestoreFileSetKind::Storage,
            },
            block_count: graph.block_records().len(),
            pmem_count: graph.pmem_records().len(),
        })
    }

    pub(crate) fn prepare(
        self,
        authority: Option<&ContainedSnapshotRestoreAuthority>,
        cancelled: impl Fn() -> bool,
    ) -> Result<PreparedSnapshotStorageRestoreResources, SnapshotRestoreResourceError> {
        let Self {
            resources,
            block_count,
            pmem_count,
        } = self;
        resources.prepare(authority, cancelled).map(|resources| {
            PreparedSnapshotStorageRestoreResources {
                resources,
                block_count,
                pmem_count,
            }
        })
    }
}

impl fmt::Debug for RequestedSnapshotStorageRestoreResources {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RequestedSnapshotStorageRestoreResources")
            .field("block_count", &self.block_count)
            .field("pmem_count", &self.pmem_count)
            .field("values", &"<redacted>")
            .finish()
    }
}

impl fmt::Debug for RequestedSnapshotMultiDriveRestoreResources {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RequestedSnapshotMultiDriveRestoreResources")
            .field("resource_count", &self.drives.len())
            .field("values", &"<redacted>")
            .finish()
    }
}

type ContainedDrivePreparationFailure = Box<(
    SnapshotRestoreResourceError,
    Vec<PreparedSnapshotDriveRestoreResource>,
    Vec<PreparedSnapshotBackingClaim>,
)>;

fn prepare_direct_drives(
    drives: &[RequestedSnapshotDriveRestoreResource],
    file_set_kind: SnapshotRestoreFileSetKind,
    cancelled: &impl Fn() -> bool,
) -> Result<Vec<PreparedSnapshotDriveRestoreResource>, SnapshotRestoreResourceError> {
    for drive in drives {
        if cancelled() {
            return Err(cancelled_batch_error());
        }
        if grant_reference_id(&drive.selector)
            .map_err(|source| {
                SnapshotRestoreResourceError::retryable(
                    file_set_kind.preflight_stage(),
                    SnapshotRestoreResourceErrorKind::DriveBacking(
                        SnapshotDriveBackingPreparationError::Authority(source),
                    ),
                )
            })?
            .is_some()
        {
            return Err(SnapshotRestoreResourceError::retryable(
                file_set_kind.preflight_stage(),
                file_set_kind.invalid_set(),
            ));
        }
        if drive.key.resource_class() != drive.kind.resource_class()
            || drive.expected_len == 0
            || drive.kind.expected_mapped_len().is_some_and(|expected| {
                aligned_pmem_mapping_len(drive.expected_len) != Some(expected)
            })
        {
            return Err(SnapshotRestoreResourceError::terminal(
                file_set_kind.preflight_stage(),
                file_set_kind.invalid_set(),
            ));
        }
    }

    let mut reservations = Vec::new();
    let mut identities = HashSet::new();
    reservations
        .try_reserve_exact(drives.len())
        .map_err(manifest_allocation_error)?;
    identities.try_reserve(drives.len()).map_err(|_| {
        SnapshotRestoreResourceError::retryable(
            file_set_kind.preflight_stage(),
            file_set_kind.invalid_set(),
        )
    })?;
    for drive in drives {
        if cancelled() {
            return Err(cancelled_batch_error());
        }
        let reservation = match drive.kind {
            SnapshotRestoreFileKind::Block => {
                match SnapshotBlockFileBackingReservation::open(&drive.selector, drive.is_read_only)
                {
                    Ok(reservation) => ReservedDirectSnapshotFileBacking::Block(reservation),
                    Err(source) => {
                        return Err(SnapshotRestoreResourceError::retryable(
                            SnapshotRestoreResourceStage::DrivePreparation,
                            SnapshotRestoreResourceErrorKind::DriveBacking(
                                SnapshotDriveBackingPreparationError::Backing(source),
                            ),
                        ));
                    }
                }
            }
            SnapshotRestoreFileKind::Pmem { .. } => {
                match SnapshotPmemFileBackingReservation::open(&drive.selector, drive.is_read_only)
                {
                    Ok(reservation) => ReservedDirectSnapshotFileBacking::Pmem(reservation),
                    Err(source) => {
                        return Err(SnapshotRestoreResourceError::retryable(
                            SnapshotRestoreResourceStage::PmemPreparation,
                            SnapshotRestoreResourceErrorKind::PmemBacking(source),
                        ));
                    }
                }
            }
        };
        let (device, inode) = match &reservation {
            ReservedDirectSnapshotFileBacking::Block(reservation) => {
                let identity = reservation.identity();
                if reservation.is_read_only() != drive.is_read_only
                    || !identity.kind().is_regular_file()
                    || identity.len() != drive.expected_len
                {
                    return Err(SnapshotRestoreResourceError::retryable(
                        file_set_kind.preflight_stage(),
                        file_set_kind.invalid_set(),
                    ));
                }
                (identity.device(), identity.inode())
            }
            ReservedDirectSnapshotFileBacking::Pmem(reservation) => {
                let identity = reservation.identity();
                if reservation.is_read_only() != drive.is_read_only
                    || !identity.is_regular_file()
                    || identity.len() != drive.expected_len
                    || drive.kind.expected_mapped_len().is_none_or(|expected| {
                        aligned_pmem_mapping_len(identity.len()) != Some(expected)
                    })
                {
                    return Err(SnapshotRestoreResourceError::retryable(
                        file_set_kind.preflight_stage(),
                        file_set_kind.invalid_set(),
                    ));
                }
                (identity.device(), identity.inode())
            }
        };
        if !identities.insert((device, inode)) {
            return Err(SnapshotRestoreResourceError::retryable(
                file_set_kind.preflight_stage(),
                file_set_kind.invalid_set(),
            ));
        }
        reservations.push(ReservedDirectSnapshotDriveRestoreResource {
            key: drive.key.clone(),
            reservation,
        });
    }

    if cancelled() {
        return Err(cancelled_batch_error());
    }
    let mut prepared = Vec::new();
    prepared
        .try_reserve_exact(drives.len())
        .map_err(manifest_allocation_error)?;
    let mut reservations = reservations.into_iter();
    for drive in drives {
        if cancelled() {
            return Err(cancelled_batch_error()
                .with_abort_outcome(abort_prepared_drive_resources(prepared)));
        }
        let Some(reserved) = reservations.next() else {
            return Err(SnapshotRestoreResourceError::terminal(
                file_set_kind.preflight_stage(),
                file_set_kind.invalid_set(),
            )
            .with_abort_outcome(abort_prepared_drive_resources(prepared)));
        };
        if reserved.key != drive.key {
            return Err(SnapshotRestoreResourceError::terminal(
                file_set_kind.preflight_stage(),
                file_set_kind.invalid_set(),
            )
            .with_abort_outcome(abort_prepared_drive_resources(prepared)));
        }
        let backing = match (drive.kind, reserved.reservation) {
            (
                SnapshotRestoreFileKind::Block,
                ReservedDirectSnapshotFileBacking::Block(reservation),
            ) => {
                let (backing, identity) = match reservation.into_backing() {
                    Ok(parts) => parts,
                    Err(source) => {
                        return Err(SnapshotRestoreResourceError::retryable(
                            SnapshotRestoreResourceStage::DrivePreparation,
                            SnapshotRestoreResourceErrorKind::DriveBacking(
                                SnapshotDriveBackingPreparationError::Backing(source),
                            ),
                        )
                        .with_abort_outcome(abort_prepared_drive_resources(prepared)));
                    }
                };
                if !snapshot_drive_observation_matches(drive, &backing, identity) {
                    drop(backing);
                    return Err(SnapshotRestoreResourceError::retryable(
                        file_set_kind.preflight_stage(),
                        file_set_kind.invalid_set(),
                    )
                    .with_abort_outcome(abort_prepared_drive_resources(prepared)));
                }
                PreparedSnapshotFileBacking::Block(backing)
            }
            (
                SnapshotRestoreFileKind::Pmem { .. },
                ReservedDirectSnapshotFileBacking::Pmem(reservation),
            ) => {
                let (backing, identity) = match reservation.into_backing() {
                    Ok(parts) => parts,
                    Err(source) => {
                        return Err(SnapshotRestoreResourceError::retryable(
                            SnapshotRestoreResourceStage::PmemPreparation,
                            SnapshotRestoreResourceErrorKind::PmemBacking(source),
                        )
                        .with_abort_outcome(abort_prepared_drive_resources(prepared)));
                    }
                };
                if !snapshot_pmem_observation_matches(drive, &backing, identity) {
                    drop(backing);
                    return Err(SnapshotRestoreResourceError::retryable(
                        file_set_kind.preflight_stage(),
                        file_set_kind.invalid_set(),
                    )
                    .with_abort_outcome(abort_prepared_drive_resources(prepared)));
                }
                PreparedSnapshotFileBacking::Pmem(backing)
            }
            (_, reservation) => {
                drop(reservation);
                return Err(SnapshotRestoreResourceError::terminal(
                    file_set_kind.preflight_stage(),
                    file_set_kind.invalid_set(),
                )
                .with_abort_outcome(abort_prepared_drive_resources(prepared)));
            }
        };
        prepared.push(PreparedSnapshotDriveRestoreResource {
            key: drive.key.clone(),
            backing,
            claim: None,
        });
    }
    if reservations.next().is_some() {
        return Err(SnapshotRestoreResourceError::terminal(
            file_set_kind.preflight_stage(),
            file_set_kind.invalid_set(),
        )
        .with_abort_outcome(abort_prepared_drive_resources(prepared)));
    }
    Ok(prepared)
}

fn prepare_contained_drives(
    drives: &[RequestedSnapshotDriveRestoreResource],
    mut claims: Vec<PreparedSnapshotBackingClaim>,
    file_set_kind: SnapshotRestoreFileSetKind,
    cancelled: &impl Fn() -> bool,
) -> Result<Vec<PreparedSnapshotDriveRestoreResource>, ContainedDrivePreparationFailure> {
    let mut prepared = Vec::new();
    let mut identities = HashSet::new();
    if prepared.try_reserve_exact(drives.len()).is_err()
        || identities.try_reserve(drives.len()).is_err()
    {
        return Err(Box::new((
            SnapshotRestoreResourceError::retryable(
                file_set_kind.preflight_stage(),
                file_set_kind.invalid_set(),
            ),
            prepared,
            claims,
        )));
    }
    if claims.len() != drives.len() {
        return Err(Box::new((
            SnapshotRestoreResourceError::terminal(
                file_set_kind.preflight_stage(),
                file_set_kind.invalid_set(),
            ),
            prepared,
            claims,
        )));
    }
    for drive in drives {
        if cancelled() {
            return Err(Box::new((cancelled_batch_error(), prepared, claims)));
        }
        let mut claim = claims.remove(0);
        let (file, observation) = match claim.take_snapshot_file_for_reference(
            &drive.selector,
            drive.kind.grant_role(),
            drive.is_read_only,
        ) {
            Ok(parts) => parts,
            Err(source) => {
                claims.insert(0, claim);
                return Err(Box::new((
                    SnapshotRestoreResourceError::retryable(
                        if matches!(drive.kind, SnapshotRestoreFileKind::Block) {
                            SnapshotRestoreResourceStage::DrivePreparation
                        } else {
                            SnapshotRestoreResourceStage::PmemPreparation
                        },
                        SnapshotRestoreResourceErrorKind::DriveBacking(
                            SnapshotDriveBackingPreparationError::Authority(source),
                        ),
                    ),
                    prepared,
                    claims,
                )));
            }
        };
        let (backing, device, inode, len, valid) = match drive.kind {
            SnapshotRestoreFileKind::Block => {
                let (backing, identity) =
                    match BlockFileBacking::from_snapshot_file(file, drive.is_read_only) {
                        Ok(parts) => parts,
                        Err(source) => {
                            claims.insert(0, claim);
                            return Err(Box::new((
                                SnapshotRestoreResourceError::retryable(
                                    SnapshotRestoreResourceStage::DrivePreparation,
                                    SnapshotRestoreResourceErrorKind::DriveBacking(
                                        SnapshotDriveBackingPreparationError::Backing(source),
                                    ),
                                ),
                                prepared,
                                claims,
                            )));
                        }
                    };
                let valid = snapshot_drive_observation_matches(drive, &backing, identity);
                (
                    PreparedSnapshotFileBacking::Block(backing),
                    identity.device(),
                    identity.inode(),
                    identity.len(),
                    valid,
                )
            }
            SnapshotRestoreFileKind::Pmem { .. } => {
                let (backing, identity) =
                    match PmemFileBacking::from_snapshot_file(file, drive.is_read_only) {
                        Ok(parts) => parts,
                        Err(source) => {
                            claims.insert(0, claim);
                            return Err(Box::new((
                                SnapshotRestoreResourceError::retryable(
                                    SnapshotRestoreResourceStage::PmemPreparation,
                                    SnapshotRestoreResourceErrorKind::PmemBacking(source),
                                ),
                                prepared,
                                claims,
                            )));
                        }
                    };
                let valid = snapshot_pmem_observation_matches(drive, &backing, identity);
                (
                    PreparedSnapshotFileBacking::Pmem(backing),
                    identity.device(),
                    identity.inode(),
                    identity.len(),
                    valid,
                )
            }
        };
        let observation_matches = observation.is_some_and(|observation| {
            observation.identity().device == device
                && observation.identity().inode == inode
                && observation.len() == len
        });
        if !observation_matches || !valid || !identities.insert((device, inode)) {
            drop(backing);
            claims.insert(0, claim);
            return Err(Box::new((
                SnapshotRestoreResourceError::retryable(
                    file_set_kind.preflight_stage(),
                    file_set_kind.invalid_set(),
                ),
                prepared,
                claims,
            )));
        }
        prepared.push(PreparedSnapshotDriveRestoreResource {
            key: drive.key.clone(),
            backing,
            claim: Some(claim),
        });
    }
    if !claims.is_empty() {
        return Err(Box::new((
            SnapshotRestoreResourceError::terminal(
                file_set_kind.preflight_stage(),
                file_set_kind.invalid_set(),
            ),
            prepared,
            claims,
        )));
    }
    Ok(prepared)
}

fn snapshot_drive_observation_matches(
    drive: &RequestedSnapshotDriveRestoreResource,
    backing: &BlockFileBacking,
    identity: BlockFileBackingIdentity,
) -> bool {
    backing.kind().is_regular_file()
        && backing.is_read_only() == drive.is_read_only
        && backing.len() == drive.expected_len
        && identity.kind().is_regular_file()
        && identity.len() == drive.expected_len
}

fn snapshot_pmem_observation_matches(
    drive: &RequestedSnapshotDriveRestoreResource,
    backing: &PmemFileBacking,
    identity: bangbang_runtime::pmem::PmemFileBackingIdentity,
) -> bool {
    identity.is_regular_file()
        && !identity.is_empty()
        && backing.is_read_only() == drive.is_read_only
        && backing.len() == drive.expected_len
        && identity.len() == drive.expected_len
        && drive
            .kind
            .expected_mapped_len()
            .is_some_and(|expected| aligned_pmem_mapping_len(identity.len()) == Some(expected))
}

fn abort_contained_vsock_facets(
    facets: Option<(
        crate::contained_session::PreparedSocketDirectoryClaim,
        crate::contained_session::PreparedSocketBrokerEndpoint,
        WorkerSocketNamespace,
    )>,
) -> SnapshotRestoreAbortOutcome {
    match facets {
        Some((directory, broker, namespace)) => {
            drop(namespace);
            if broker.abort().is_err() | directory.abort().is_err() {
                SnapshotRestoreAbortOutcome::terminal(true)
            } else {
                SnapshotRestoreAbortOutcome::retryable()
            }
        }
        None => SnapshotRestoreAbortOutcome::retryable(),
    }
}

fn abort_prepared_drive_claims(
    claims: Vec<PreparedDriveBackingClaim>,
) -> SnapshotRestoreAbortOutcome {
    let mut outcome = SnapshotRestoreAbortOutcome::retryable();
    for claim in claims.into_iter().rev() {
        if claim.abort().is_err() {
            outcome = outcome.merge(SnapshotRestoreAbortOutcome::terminal(true));
        }
    }
    outcome
}

fn abort_prepared_drive_resources(
    resources: Vec<PreparedSnapshotDriveRestoreResource>,
) -> SnapshotRestoreAbortOutcome {
    abort_prepared_drive_resource_iter(resources.into_iter())
}

fn abort_prepared_drive_resource_iter(
    resources: impl DoubleEndedIterator<Item = PreparedSnapshotDriveRestoreResource>,
) -> SnapshotRestoreAbortOutcome {
    let mut outcome = SnapshotRestoreAbortOutcome::retryable();
    for resource in resources.rev() {
        outcome = outcome.merge(resource.abort());
    }
    outcome
}

fn abort_drive_bindings(
    resources: impl DoubleEndedIterator<Item = PreparedSnapshotDriveRestoreResource>,
) -> SnapshotRestoreAbortOutcome {
    let mut outcome = SnapshotRestoreAbortOutcome::retryable();
    for resource in resources.rev() {
        outcome = outcome.merge(resource.abort());
    }
    outcome
}

pub(crate) struct PreparedSnapshotMultiDriveRestoreResources {
    drive_keys: Vec<SnapshotRestoreResourceKey>,
    drive_configs: DriveConfigs,
    bindings: PreparedSnapshotRestoreBindings<PreparedSnapshotDriveRestoreResource>,
    contained_transaction: Option<ContainedSnapshotRestoreTransaction>,
    file_set_kind: SnapshotRestoreFileSetKind,
}

impl PreparedSnapshotMultiDriveRestoreResources {
    pub(crate) fn into_drive_batch(
        mut self,
    ) -> Result<PreparedSnapshotRestoreDriveBatch, SnapshotRestoreResourceError> {
        if self.file_set_kind != SnapshotRestoreFileSetKind::MultiBlock {
            let outcome = self.abort();
            return Err(SnapshotRestoreResourceError::terminal(
                SnapshotRestoreResourceStage::Take,
                SnapshotRestoreResourceErrorKind::InvalidDriveSet,
            )
            .with_abort_outcome(outcome));
        }
        let mut owners = Vec::new();
        if owners.try_reserve_exact(self.drive_keys.len()).is_err() {
            let outcome = self.abort();
            return Err(SnapshotRestoreResourceError::retryable(
                SnapshotRestoreResourceStage::Take,
                SnapshotRestoreResourceErrorKind::InvalidDriveSet,
            )
            .with_abort_outcome(outcome));
        }
        for key in &self.drive_keys {
            match self.bindings.take(key) {
                Ok(owner) if owner.key == *key => owners.push(owner),
                Ok(owner) => {
                    owners.push(owner);
                    let outcome = self.abort_with_taken(owners);
                    return Err(SnapshotRestoreResourceError::terminal(
                        SnapshotRestoreResourceStage::Take,
                        SnapshotRestoreResourceErrorKind::InvalidDriveSet,
                    )
                    .with_abort_outcome(outcome));
                }
                Err(source) => {
                    let outcome = self.abort_with_taken(owners);
                    return Err(SnapshotRestoreResourceError::terminal(
                        SnapshotRestoreResourceStage::Take,
                        SnapshotRestoreResourceErrorKind::Take(source),
                    )
                    .with_abort_outcome(outcome));
                }
            }
        }
        let Self {
            drive_keys: _,
            drive_configs,
            bindings,
            contained_transaction,
            file_set_kind: _,
        } = self;
        if let Err(unconsumed) = bindings.finish() {
            let unconsumed_count = unconsumed.unconsumed_count();
            let outcome = abort_drive_bindings(unconsumed.into_bindings().into_values())
                .merge(abort_prepared_drive_resources(owners))
                .merge(abort_contained_transaction(contained_transaction));
            return Err(SnapshotRestoreResourceError::terminal(
                SnapshotRestoreResourceStage::Finish,
                SnapshotRestoreResourceErrorKind::Unconsumed { unconsumed_count },
            )
            .with_abort_outcome(outcome));
        }
        if owners.iter().any(|owner| {
            owner.key.resource_class() != SnapshotRestoreResourceClass::BlockBacking
                || !matches!(owner.backing, PreparedSnapshotFileBacking::Block(_))
        }) {
            let outcome = abort_prepared_drive_resources(owners)
                .merge(abort_contained_transaction(contained_transaction));
            return Err(SnapshotRestoreResourceError::terminal(
                SnapshotRestoreResourceStage::Finish,
                SnapshotRestoreResourceErrorKind::InvalidDriveSet,
            )
            .with_abort_outcome(outcome));
        }
        let mut backings = Vec::new();
        let mut claims = Vec::new();
        if backings.try_reserve_exact(owners.len()).is_err()
            || claims.try_reserve_exact(owners.len()).is_err()
        {
            let outcome = abort_prepared_drive_resources(owners)
                .merge(abort_contained_transaction(contained_transaction));
            return Err(SnapshotRestoreResourceError::retryable(
                SnapshotRestoreResourceStage::Finish,
                SnapshotRestoreResourceErrorKind::InvalidDriveSet,
            )
            .with_abort_outcome(outcome));
        }
        for owner in owners {
            let PreparedSnapshotDriveRestoreResource {
                key: _,
                backing,
                claim,
            } = owner;
            let backing = match backing {
                PreparedSnapshotFileBacking::Block(backing) => backing,
                PreparedSnapshotFileBacking::Pmem(backing) => {
                    drop(backing);
                    return Err(SnapshotRestoreResourceError::terminal(
                        SnapshotRestoreResourceStage::Finish,
                        SnapshotRestoreResourceErrorKind::InvalidDriveSet,
                    ));
                }
            };
            backings.push(backing);
            if let Some(claim) = claim {
                claims.push(claim);
            }
        }
        Ok(PreparedSnapshotRestoreDriveBatch {
            drive_configs,
            backings,
            completion: PreparedSnapshotDriveRestoreCompletion {
                claims,
                contained_transaction,
            },
        })
    }

    fn abort(self) -> SnapshotRestoreAbortOutcome {
        abort_drive_bindings(self.bindings.into_values())
            .merge(abort_contained_transaction(self.contained_transaction))
    }

    fn abort_with_taken(
        self,
        taken: Vec<PreparedSnapshotDriveRestoreResource>,
    ) -> SnapshotRestoreAbortOutcome {
        abort_drive_bindings(self.bindings.into_values())
            .merge(abort_prepared_drive_resources(taken))
            .merge(abort_contained_transaction(self.contained_transaction))
    }
}

impl fmt::Debug for PreparedSnapshotMultiDriveRestoreResources {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotMultiDriveRestoreResources")
            .field("resource_count", &self.drive_keys.len())
            .field("remaining_count", &self.bindings.remaining_count())
            .field("values", &"<redacted>")
            .finish()
    }
}

pub(crate) struct PreparedSnapshotStorageRestoreResources {
    resources: PreparedSnapshotMultiDriveRestoreResources,
    block_count: usize,
    pmem_count: usize,
}

impl PreparedSnapshotStorageRestoreResources {
    pub(crate) fn into_storage_batch(
        self,
    ) -> Result<PreparedSnapshotStorageRestoreBatch, SnapshotRestoreResourceError> {
        let Self {
            mut resources,
            block_count,
            pmem_count,
        } = self;
        if resources.file_set_kind != SnapshotRestoreFileSetKind::Storage {
            let outcome = resources.abort();
            return Err(SnapshotRestoreResourceError::terminal(
                SnapshotRestoreResourceStage::Take,
                SnapshotRestoreResourceErrorKind::InvalidStorageSet,
            )
            .with_abort_outcome(outcome));
        }
        let Some(expected_count) = block_count.checked_add(pmem_count) else {
            let outcome = resources.abort();
            return Err(SnapshotRestoreResourceError::terminal(
                SnapshotRestoreResourceStage::Take,
                SnapshotRestoreResourceErrorKind::InvalidStorageSet,
            )
            .with_abort_outcome(outcome));
        };
        if expected_count == 0 || resources.drive_keys.len() != expected_count {
            let outcome = resources.abort();
            return Err(SnapshotRestoreResourceError::terminal(
                SnapshotRestoreResourceStage::Take,
                SnapshotRestoreResourceErrorKind::InvalidStorageSet,
            )
            .with_abort_outcome(outcome));
        }
        let mut owners = Vec::new();
        if owners.try_reserve_exact(expected_count).is_err() {
            let outcome = resources.abort();
            return Err(SnapshotRestoreResourceError::retryable(
                SnapshotRestoreResourceStage::Take,
                SnapshotRestoreResourceErrorKind::InvalidStorageSet,
            )
            .with_abort_outcome(outcome));
        }
        for key in &resources.drive_keys {
            match resources.bindings.take(key) {
                Ok(owner) if owner.key == *key => owners.push(owner),
                Ok(owner) => {
                    owners.push(owner);
                    let outcome = resources.abort_with_taken(owners);
                    return Err(SnapshotRestoreResourceError::terminal(
                        SnapshotRestoreResourceStage::Take,
                        SnapshotRestoreResourceErrorKind::InvalidStorageSet,
                    )
                    .with_abort_outcome(outcome));
                }
                Err(source) => {
                    let outcome = resources.abort_with_taken(owners);
                    return Err(SnapshotRestoreResourceError::terminal(
                        SnapshotRestoreResourceStage::Take,
                        SnapshotRestoreResourceErrorKind::Take(source),
                    )
                    .with_abort_outcome(outcome));
                }
            }
        }
        let PreparedSnapshotMultiDriveRestoreResources {
            drive_keys: _,
            drive_configs: _,
            bindings,
            contained_transaction,
            file_set_kind: _,
        } = resources;
        if let Err(unconsumed) = bindings.finish() {
            let unconsumed_count = unconsumed.unconsumed_count();
            let outcome = abort_drive_bindings(unconsumed.into_bindings().into_values())
                .merge(abort_prepared_drive_resources(owners))
                .merge(abort_contained_transaction(contained_transaction));
            return Err(SnapshotRestoreResourceError::terminal(
                SnapshotRestoreResourceStage::Finish,
                SnapshotRestoreResourceErrorKind::Unconsumed { unconsumed_count },
            )
            .with_abort_outcome(outcome));
        }
        let valid_order = owners.len() == expected_count
            && owners.iter().enumerate().all(|(index, owner)| {
                if index < block_count {
                    owner.key.resource_class() == SnapshotRestoreResourceClass::BlockBacking
                        && matches!(owner.backing, PreparedSnapshotFileBacking::Block(_))
                } else {
                    owner.key.resource_class() == SnapshotRestoreResourceClass::PmemBacking
                        && matches!(owner.backing, PreparedSnapshotFileBacking::Pmem(_))
                }
            });
        if !valid_order {
            let outcome = abort_prepared_drive_resources(owners)
                .merge(abort_contained_transaction(contained_transaction));
            return Err(SnapshotRestoreResourceError::terminal(
                SnapshotRestoreResourceStage::Finish,
                SnapshotRestoreResourceErrorKind::InvalidStorageSet,
            )
            .with_abort_outcome(outcome));
        }
        let mut blocks = Vec::new();
        let mut pmems = Vec::new();
        let mut claims = Vec::new();
        if blocks.try_reserve_exact(block_count).is_err()
            || pmems.try_reserve_exact(pmem_count).is_err()
            || claims.try_reserve_exact(expected_count).is_err()
        {
            let outcome = abort_prepared_drive_resources(owners)
                .merge(abort_contained_transaction(contained_transaction));
            return Err(SnapshotRestoreResourceError::retryable(
                SnapshotRestoreResourceStage::Finish,
                SnapshotRestoreResourceErrorKind::InvalidStorageSet,
            )
            .with_abort_outcome(outcome));
        }
        for owner in owners {
            let PreparedSnapshotDriveRestoreResource {
                key,
                backing,
                claim,
            } = owner;
            match backing {
                PreparedSnapshotFileBacking::Block(backing) => {
                    blocks.push(PreparedSnapshotBlockRestoreBacking { key, backing });
                }
                PreparedSnapshotFileBacking::Pmem(backing) => {
                    pmems.push(PreparedSnapshotPmemRestoreBacking { key, backing });
                }
            }
            if let Some(claim) = claim {
                claims.push(claim);
            }
        }
        if blocks.len() != block_count || pmems.len() != pmem_count {
            drop(blocks);
            drop(pmems);
            let outcome = abort_prepared_drive_claims(claims)
                .merge(abort_contained_transaction(contained_transaction));
            return Err(SnapshotRestoreResourceError::terminal(
                SnapshotRestoreResourceStage::Finish,
                SnapshotRestoreResourceErrorKind::InvalidStorageSet,
            )
            .with_abort_outcome(outcome));
        }
        Ok(PreparedSnapshotStorageRestoreBatch {
            blocks,
            pmems,
            completion: PreparedSnapshotDriveRestoreCompletion {
                claims,
                contained_transaction,
            },
        })
    }
}

impl fmt::Debug for PreparedSnapshotStorageRestoreResources {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotStorageRestoreResources")
            .field("block_count", &self.block_count)
            .field("pmem_count", &self.pmem_count)
            .field("values", &"<redacted>")
            .finish()
    }
}

pub(crate) struct PreparedSnapshotBlockRestoreBacking {
    key: SnapshotRestoreResourceKey,
    backing: BlockFileBacking,
}

impl PreparedSnapshotBlockRestoreBacking {
    pub(crate) fn into_parts(self) -> (SnapshotRestoreResourceKey, BlockFileBacking) {
        (self.key, self.backing)
    }
}

impl fmt::Debug for PreparedSnapshotBlockRestoreBacking {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotBlockRestoreBacking")
            .field("key", &self.key)
            .field("backing", &"<owned>")
            .finish()
    }
}

pub(crate) struct PreparedSnapshotPmemRestoreBacking {
    key: SnapshotRestoreResourceKey,
    backing: PmemFileBacking,
}

impl PreparedSnapshotPmemRestoreBacking {
    pub(crate) fn into_parts(self) -> (SnapshotRestoreResourceKey, PmemFileBacking) {
        (self.key, self.backing)
    }
}

impl fmt::Debug for PreparedSnapshotPmemRestoreBacking {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotPmemRestoreBacking")
            .field("key", &self.key)
            .field("backing", &"<owned>")
            .finish()
    }
}

/// Exact graph-ordered, unmapped block and pmem restore owners.
pub(crate) struct PreparedSnapshotStorageRestoreBatch {
    blocks: Vec<PreparedSnapshotBlockRestoreBacking>,
    pmems: Vec<PreparedSnapshotPmemRestoreBacking>,
    completion: PreparedSnapshotDriveRestoreCompletion,
}

impl PreparedSnapshotStorageRestoreBatch {
    pub(crate) fn into_parts(
        self,
    ) -> (
        Vec<PreparedSnapshotBlockRestoreBacking>,
        Vec<PreparedSnapshotPmemRestoreBacking>,
        PreparedSnapshotDriveRestoreCompletion,
    ) {
        (self.blocks, self.pmems, self.completion)
    }
}

impl fmt::Debug for PreparedSnapshotStorageRestoreBatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotStorageRestoreBatch")
            .field("block_count", &self.blocks.len())
            .field("pmem_count", &self.pmems.len())
            .field("values", &"<redacted>")
            .finish()
    }
}

/// Exact graph-ordered block resources ready for pathless device construction.
pub(crate) struct PreparedSnapshotRestoreDriveBatch {
    drive_configs: DriveConfigs,
    backings: Vec<BlockFileBacking>,
    completion: PreparedSnapshotDriveRestoreCompletion,
}

impl PreparedSnapshotRestoreDriveBatch {
    pub(crate) fn into_parts(
        self,
    ) -> (
        DriveConfigs,
        Vec<BlockFileBacking>,
        PreparedSnapshotDriveRestoreCompletion,
    ) {
        (self.drive_configs, self.backings, self.completion)
    }
}

impl fmt::Debug for PreparedSnapshotRestoreDriveBatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotRestoreDriveBatch")
            .field("drive_count", &self.backings.len())
            .field("values", &"<redacted>")
            .finish()
    }
}

pub(crate) struct PreparedSnapshotDriveRestoreCompletion {
    claims: Vec<PreparedDriveBackingClaim>,
    contained_transaction: Option<ContainedSnapshotRestoreTransaction>,
}

impl PreparedSnapshotDriveRestoreCompletion {
    pub(crate) fn commit(mut self) -> Result<(), PreparedSnapshotDriveRestoreCompletionError> {
        let claims = std::mem::take(&mut self.claims);
        let contained_transaction = self.contained_transaction.take();
        for claim in claims {
            claim.commit();
        }
        match contained_transaction {
            Some(transaction) => {
                transaction
                    .commit()
                    .map_err(|source| PreparedSnapshotDriveRestoreCompletionError {
                        grant_failed: false,
                        contained: Some(source),
                    })
            }
            None => Ok(()),
        }
    }

    pub(crate) fn abort(mut self) -> Result<(), PreparedSnapshotDriveRestoreCompletionError> {
        let claims = std::mem::take(&mut self.claims);
        let contained_transaction = self.contained_transaction.take();
        let grant_failed = claims
            .into_iter()
            .rev()
            .fold(false, |failed, claim| claim.abort().is_err() | failed);
        let contained = contained_transaction.and_then(|transaction| transaction.abort().err());
        if grant_failed || contained.is_some() {
            Err(PreparedSnapshotDriveRestoreCompletionError {
                grant_failed,
                contained,
            })
        } else {
            Ok(())
        }
    }
}

impl Drop for PreparedSnapshotDriveRestoreCompletion {
    fn drop(&mut self) {
        for claim in std::mem::take(&mut self.claims).into_iter().rev() {
            let _ = claim.abort();
        }
        if let Some(transaction) = self.contained_transaction.take() {
            let _ = transaction.abort();
        }
    }
}

fn abort_drive_completion_outcome(
    completion: PreparedSnapshotDriveRestoreCompletion,
) -> SnapshotRestoreAbortOutcome {
    if completion.abort().is_err() {
        SnapshotRestoreAbortOutcome::terminal(true)
    } else {
        SnapshotRestoreAbortOutcome::retryable()
    }
}

impl fmt::Debug for PreparedSnapshotDriveRestoreCompletion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotDriveRestoreCompletion")
            .field("state", &"<provisional>")
            .finish()
    }
}

#[derive(Debug)]
pub(crate) struct PreparedSnapshotDriveRestoreCompletionError {
    grant_failed: bool,
    contained: Option<ContainedSnapshotRestoreError>,
}

impl PreparedSnapshotDriveRestoreCompletionError {
    pub(crate) const fn disposition(&self) -> SnapshotRestoreResourceDisposition {
        SnapshotRestoreResourceDisposition::Terminal
    }
}

impl fmt::Display for PreparedSnapshotDriveRestoreCompletionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("snapshot drive batch completion authority failed")?;
        if self.grant_failed {
            formatter.write_str("; grant cleanup also failed")?;
        }
        Ok(())
    }
}

impl std::error::Error for PreparedSnapshotDriveRestoreCompletionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.contained
            .as_ref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

/// Process-owned composition of one pathless block bundle and its aggregate
/// provisional backing authority.
pub(crate) struct PreparedSnapshotV2MultiBlockRestoreBundle {
    bundle: Option<PreparedSnapshotV2MultiBlockBundle>,
    completion: Option<PreparedSnapshotDriveRestoreCompletion>,
}

impl PreparedSnapshotV2MultiBlockRestoreBundle {
    pub(crate) const fn bundle(&self) -> Option<&PreparedSnapshotV2MultiBlockBundle> {
        self.bundle.as_ref()
    }

    pub(crate) fn construct_destination<D, E>(
        mut self,
        construct: impl FnOnce(PreparedSnapshotV2MultiBlockBundle) -> Result<D, E>,
    ) -> Result<
        PreparedSnapshotV2MultiBlockDestination<D>,
        PreparedSnapshotV2MultiBlockDestinationConstructionError<E>,
    > {
        let Some(bundle) = self.bundle.take() else {
            return Err(
                PreparedSnapshotV2MultiBlockDestinationConstructionError::InvalidState {
                    bundle_cleanup: None,
                },
            );
        };
        let Some(completion) = self.completion.take() else {
            return Err(
                PreparedSnapshotV2MultiBlockDestinationConstructionError::InvalidState {
                    bundle_cleanup: bundle.abort().err(),
                },
            );
        };
        match construct(bundle) {
            Ok(destination) => Ok(PreparedSnapshotV2MultiBlockDestination {
                destination: Some(destination),
                completion: Some(completion),
            }),
            Err(source) => Err(
                PreparedSnapshotV2MultiBlockDestinationConstructionError::Construction {
                    source,
                    completion_abort: completion.abort().err(),
                },
            ),
        }
    }

    pub(crate) fn abort(mut self) -> Result<(), PreparedSnapshotV2MultiBlockRestoreAbortError> {
        let bundle = self.bundle.take().and_then(|bundle| bundle.abort().err());
        let completion = self
            .completion
            .take()
            .and_then(|completion| completion.abort().err());
        if bundle.is_some() || completion.is_some() {
            Err(PreparedSnapshotV2MultiBlockRestoreAbortError { bundle, completion })
        } else {
            Ok(())
        }
    }
}

impl Drop for PreparedSnapshotV2MultiBlockRestoreBundle {
    fn drop(&mut self) {
        if let Some(bundle) = self.bundle.take() {
            let _ = bundle.abort();
        }
        if let Some(completion) = self.completion.take() {
            let _ = completion.abort();
        }
    }
}

impl fmt::Debug for PreparedSnapshotV2MultiBlockRestoreBundle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2MultiBlockRestoreBundle")
            .field(
                "drive_count",
                &self
                    .bundle
                    .as_ref()
                    .map_or(0, |bundle| bundle.records().len()),
            )
            .field("state", &"<redacted>")
            .finish()
    }
}

/// One complete private destination whose backing authority remains
/// provisional until the controller projection is also ready.
pub(crate) struct PreparedSnapshotV2MultiBlockDestination<D> {
    destination: Option<D>,
    completion: Option<PreparedSnapshotDriveRestoreCompletion>,
}

impl<D> PreparedSnapshotV2MultiBlockDestination<D> {
    pub(crate) fn commit<C, E, T>(
        mut self,
        prepare_controller: impl FnOnce(D) -> Result<(D, C), (D, E)>,
        destroy_destination: impl FnOnce(D) -> Result<(), T>,
    ) -> Result<(D, C), PreparedSnapshotV2MultiBlockDestinationCommitError<E, T>> {
        let destination = self
            .destination
            .take()
            .ok_or(PreparedSnapshotV2MultiBlockDestinationCommitError::InvalidState)?;
        let completion = self
            .completion
            .take()
            .ok_or(PreparedSnapshotV2MultiBlockDestinationCommitError::InvalidState)?;
        let (destination, controller) = match prepare_controller(destination) {
            Ok(prepared) => prepared,
            Err((destination, source)) => {
                let destination_cleanup = destroy_destination(destination).err();
                let completion_abort = completion.abort().err();
                return Err(
                    PreparedSnapshotV2MultiBlockDestinationCommitError::Controller {
                        source,
                        destination_cleanup,
                        completion_abort,
                    },
                );
            }
        };
        match completion.commit() {
            Ok(()) => Ok((destination, controller)),
            Err(source) => {
                let destination_cleanup = destroy_destination(destination).err();
                Err(
                    PreparedSnapshotV2MultiBlockDestinationCommitError::Completion {
                        source,
                        destination_cleanup,
                    },
                )
            }
        }
    }
}

impl<D> fmt::Debug for PreparedSnapshotV2MultiBlockDestination<D> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2MultiBlockDestination")
            .field("state", &"<private-provisional>")
            .finish()
    }
}

pub(crate) enum PreparedSnapshotV2MultiBlockDestinationConstructionError<E> {
    InvalidState {
        bundle_cleanup: Option<SnapshotV2MultiBlockCleanupError>,
    },
    Construction {
        source: E,
        completion_abort: Option<PreparedSnapshotDriveRestoreCompletionError>,
    },
}

impl<E> PreparedSnapshotV2MultiBlockDestinationConstructionError<E> {
    pub(crate) const fn is_terminal(&self) -> bool {
        match self {
            Self::InvalidState { .. }
            | Self::Construction {
                completion_abort: Some(_),
                ..
            } => true,
            Self::Construction {
                completion_abort: None,
                ..
            } => false,
        }
    }
}

impl<E> fmt::Debug for PreparedSnapshotV2MultiBlockDestinationConstructionError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::InvalidState { .. } => "invalid-state",
            Self::Construction { .. } => "construction",
        };
        formatter
            .debug_struct("PreparedSnapshotV2MultiBlockDestinationConstructionError")
            .field("kind", &kind)
            .field("terminal", &self.is_terminal())
            .field("state", &"<redacted>")
            .finish()
    }
}

impl<E> fmt::Display for PreparedSnapshotV2MultiBlockDestinationConstructionError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidState { .. } => {
                "snapshot multi-block destination construction state is invalid"
            }
            Self::Construction { .. } => "snapshot multi-block destination construction failed",
        })
    }
}

impl<E> std::error::Error for PreparedSnapshotV2MultiBlockDestinationConstructionError<E>
where
    E: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidState { bundle_cleanup } => bundle_cleanup
                .as_ref()
                .map(|source| source as &(dyn std::error::Error + 'static)),
            Self::Construction { source, .. } => Some(source),
        }
    }
}

pub(crate) enum PreparedSnapshotV2MultiBlockDestinationCommitError<E, T> {
    InvalidState,
    Controller {
        source: E,
        destination_cleanup: Option<T>,
        completion_abort: Option<PreparedSnapshotDriveRestoreCompletionError>,
    },
    Completion {
        source: PreparedSnapshotDriveRestoreCompletionError,
        destination_cleanup: Option<T>,
    },
}

impl<E, T> PreparedSnapshotV2MultiBlockDestinationCommitError<E, T> {
    pub(crate) const fn is_terminal(&self) -> bool {
        match self {
            Self::InvalidState => true,
            Self::Controller {
                destination_cleanup,
                completion_abort,
                ..
            } => destination_cleanup.is_some() || completion_abort.is_some(),
            Self::Completion {
                destination_cleanup,
                ..
            } => {
                let _cleanup_failed = destination_cleanup.is_some();
                true
            }
        }
    }
}

impl<E, T> fmt::Debug for PreparedSnapshotV2MultiBlockDestinationCommitError<E, T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::InvalidState => "invalid-state",
            Self::Controller { .. } => "controller",
            Self::Completion { .. } => "completion",
        };
        formatter
            .debug_struct("PreparedSnapshotV2MultiBlockDestinationCommitError")
            .field("kind", &kind)
            .field("terminal", &self.is_terminal())
            .field("state", &"<redacted>")
            .finish()
    }
}

impl<E, T> fmt::Display for PreparedSnapshotV2MultiBlockDestinationCommitError<E, T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidState => "snapshot multi-block destination commit state is invalid",
            Self::Controller { .. } => {
                "snapshot multi-block controller preparation failed before completion"
            }
            Self::Completion { .. } => {
                "snapshot multi-block backing completion failed after destination construction"
            }
        })
    }
}

impl<E, T> std::error::Error for PreparedSnapshotV2MultiBlockDestinationCommitError<E, T>
where
    E: std::error::Error + 'static,
    T: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Controller { source, .. } => Some(source),
            Self::Completion { source, .. } => Some(source),
            Self::InvalidState => None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct PreparedSnapshotV2MultiBlockRestoreAbortError {
    bundle: Option<SnapshotV2MultiBlockCleanupError>,
    completion: Option<PreparedSnapshotDriveRestoreCompletionError>,
}

impl fmt::Display for PreparedSnapshotV2MultiBlockRestoreAbortError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("snapshot multi-block restore bundle cleanup failed")
    }
}

impl std::error::Error for PreparedSnapshotV2MultiBlockRestoreAbortError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.bundle
            .as_ref()
            .map(|source| source as &(dyn std::error::Error + 'static))
            .or_else(|| {
                self.completion
                    .as_ref()
                    .map(|source| source as &(dyn std::error::Error + 'static))
            })
    }
}

/// Process-owned composition of one pathless profile-3 storage bundle and
/// its aggregate provisional backing authority.
pub(crate) struct PreparedSnapshotV2StorageRestoreBundle {
    bundle: Option<PreparedSnapshotV2StorageBundle>,
    completion: Option<PreparedSnapshotDriveRestoreCompletion>,
}

impl PreparedSnapshotV2StorageRestoreBundle {
    pub(crate) const fn bundle(&self) -> Option<&PreparedSnapshotV2StorageBundle> {
        self.bundle.as_ref()
    }

    pub(crate) fn construct_destination<D, E>(
        mut self,
        construct: impl FnOnce(PreparedSnapshotV2StorageBundle) -> Result<D, E>,
    ) -> Result<
        PreparedSnapshotV2StorageDestination<D>,
        PreparedSnapshotV2StorageDestinationConstructionError<E>,
    > {
        let Some(bundle) = self.bundle.take() else {
            return Err(
                PreparedSnapshotV2StorageDestinationConstructionError::InvalidState {
                    bundle_cleanup: None,
                },
            );
        };
        let Some(completion) = self.completion.take() else {
            return Err(
                PreparedSnapshotV2StorageDestinationConstructionError::InvalidState {
                    bundle_cleanup: bundle.abort().err(),
                },
            );
        };
        match construct(bundle) {
            Ok(destination) => Ok(PreparedSnapshotV2StorageDestination {
                destination: Some(destination),
                completion: Some(completion),
            }),
            Err(source) => Err(
                PreparedSnapshotV2StorageDestinationConstructionError::Construction {
                    source,
                    completion_abort: completion.abort().err(),
                },
            ),
        }
    }

    pub(crate) fn abort(mut self) -> Result<(), PreparedSnapshotV2StorageRestoreAbortError> {
        let bundle = self.bundle.take().and_then(|bundle| bundle.abort().err());
        let completion = self
            .completion
            .take()
            .and_then(|completion| completion.abort().err());
        if bundle.is_some() || completion.is_some() {
            Err(PreparedSnapshotV2StorageRestoreAbortError { bundle, completion })
        } else {
            Ok(())
        }
    }
}

impl Drop for PreparedSnapshotV2StorageRestoreBundle {
    fn drop(&mut self) {
        if let Some(bundle) = self.bundle.take() {
            let _ = bundle.abort();
        }
        if let Some(completion) = self.completion.take() {
            let _ = completion.abort();
        }
    }
}

impl fmt::Debug for PreparedSnapshotV2StorageRestoreBundle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2StorageRestoreBundle")
            .field(
                "block_count",
                &self
                    .bundle
                    .as_ref()
                    .and_then(PreparedSnapshotV2StorageBundle::block_bundle)
                    .map_or(0, |bundle| bundle.records().len()),
            )
            .field(
                "pmem_count",
                &self
                    .bundle
                    .as_ref()
                    .map_or(0, |bundle| bundle.pmem_records().len()),
            )
            .field("state", &"<redacted>")
            .finish()
    }
}

/// One complete private profile-3 destination whose backing authority remains
/// provisional until its controller projection is also ready.
pub(crate) struct PreparedSnapshotV2StorageDestination<D> {
    destination: Option<D>,
    completion: Option<PreparedSnapshotDriveRestoreCompletion>,
}

impl<D> PreparedSnapshotV2StorageDestination<D> {
    pub(crate) fn commit<C, E, T>(
        mut self,
        prepare_controller: impl FnOnce(D) -> Result<(D, C), (D, E)>,
        destroy_destination: impl FnOnce(D) -> Result<(), T>,
    ) -> Result<(D, C), PreparedSnapshotV2StorageDestinationCommitError<E, T>> {
        let destination = self
            .destination
            .take()
            .ok_or(PreparedSnapshotV2StorageDestinationCommitError::InvalidState)?;
        let completion = self
            .completion
            .take()
            .ok_or(PreparedSnapshotV2StorageDestinationCommitError::InvalidState)?;
        let (destination, controller) = match prepare_controller(destination) {
            Ok(prepared) => prepared,
            Err((destination, source)) => {
                let destination_cleanup = destroy_destination(destination).err();
                let completion_abort = completion.abort().err();
                return Err(
                    PreparedSnapshotV2StorageDestinationCommitError::Controller {
                        source,
                        destination_cleanup,
                        completion_abort,
                    },
                );
            }
        };
        match completion.commit() {
            Ok(()) => Ok((destination, controller)),
            Err(source) => {
                let destination_cleanup = destroy_destination(destination).err();
                Err(
                    PreparedSnapshotV2StorageDestinationCommitError::Completion {
                        source,
                        destination_cleanup,
                    },
                )
            }
        }
    }
}

impl<D> fmt::Debug for PreparedSnapshotV2StorageDestination<D> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2StorageDestination")
            .field("state", &"<private-provisional>")
            .finish()
    }
}

pub(crate) enum PreparedSnapshotV2StorageDestinationConstructionError<E> {
    InvalidState {
        bundle_cleanup: Option<SnapshotV2StorageCleanupError>,
    },
    Construction {
        source: E,
        completion_abort: Option<PreparedSnapshotDriveRestoreCompletionError>,
    },
}

impl<E> PreparedSnapshotV2StorageDestinationConstructionError<E> {
    pub(crate) const fn is_terminal(&self) -> bool {
        match self {
            Self::InvalidState { .. }
            | Self::Construction {
                completion_abort: Some(_),
                ..
            } => true,
            Self::Construction {
                completion_abort: None,
                ..
            } => false,
        }
    }
}

impl<E> fmt::Debug for PreparedSnapshotV2StorageDestinationConstructionError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::InvalidState { .. } => "invalid-state",
            Self::Construction { .. } => "construction",
        };
        formatter
            .debug_struct("PreparedSnapshotV2StorageDestinationConstructionError")
            .field("kind", &kind)
            .field("terminal", &self.is_terminal())
            .field("state", &"<redacted>")
            .finish()
    }
}

impl<E> fmt::Display for PreparedSnapshotV2StorageDestinationConstructionError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidState { .. } => {
                "snapshot storage destination construction state is invalid"
            }
            Self::Construction { .. } => "snapshot storage destination construction failed",
        })
    }
}

impl<E> std::error::Error for PreparedSnapshotV2StorageDestinationConstructionError<E>
where
    E: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidState { bundle_cleanup } => bundle_cleanup
                .as_ref()
                .map(|source| source as &(dyn std::error::Error + 'static)),
            Self::Construction { source, .. } => Some(source),
        }
    }
}

pub(crate) enum PreparedSnapshotV2StorageDestinationCommitError<E, T> {
    InvalidState,
    Controller {
        source: E,
        destination_cleanup: Option<T>,
        completion_abort: Option<PreparedSnapshotDriveRestoreCompletionError>,
    },
    Completion {
        source: PreparedSnapshotDriveRestoreCompletionError,
        destination_cleanup: Option<T>,
    },
}

impl<E, T> PreparedSnapshotV2StorageDestinationCommitError<E, T> {
    pub(crate) const fn is_terminal(&self) -> bool {
        match self {
            Self::InvalidState => true,
            Self::Controller {
                destination_cleanup,
                completion_abort,
                ..
            } => destination_cleanup.is_some() || completion_abort.is_some(),
            Self::Completion {
                destination_cleanup,
                ..
            } => {
                let _cleanup_failed = destination_cleanup.is_some();
                true
            }
        }
    }
}

impl<E, T> fmt::Debug for PreparedSnapshotV2StorageDestinationCommitError<E, T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::InvalidState => "invalid-state",
            Self::Controller { .. } => "controller",
            Self::Completion { .. } => "completion",
        };
        formatter
            .debug_struct("PreparedSnapshotV2StorageDestinationCommitError")
            .field("kind", &kind)
            .field("terminal", &self.is_terminal())
            .field("state", &"<redacted>")
            .finish()
    }
}

impl<E, T> fmt::Display for PreparedSnapshotV2StorageDestinationCommitError<E, T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidState => "snapshot storage destination commit state is invalid",
            Self::Controller { .. } => {
                "snapshot storage controller preparation failed before completion"
            }
            Self::Completion { .. } => {
                "snapshot storage backing completion failed after destination construction"
            }
        })
    }
}

impl<E, T> std::error::Error for PreparedSnapshotV2StorageDestinationCommitError<E, T>
where
    E: std::error::Error + 'static,
    T: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Controller { source, .. } => Some(source),
            Self::Completion { source, .. } => Some(source),
            Self::InvalidState => None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct PreparedSnapshotV2StorageRestoreAbortError {
    bundle: Option<SnapshotV2StorageCleanupError>,
    completion: Option<PreparedSnapshotDriveRestoreCompletionError>,
}

impl fmt::Display for PreparedSnapshotV2StorageRestoreAbortError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("snapshot storage restore bundle cleanup failed")
    }
}

impl std::error::Error for PreparedSnapshotV2StorageRestoreAbortError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.bundle
            .as_ref()
            .map(|source| source as &(dyn std::error::Error + 'static))
            .or_else(|| {
                self.completion
                    .as_ref()
                    .map(|source| source as &(dyn std::error::Error + 'static))
            })
    }
}

pub(crate) enum SnapshotV2StorageRestoreBundleError {
    Resources(SnapshotRestoreResourceError),
    Plan(SnapshotV2StorageRestorePlanError),
    Bundle {
        source: SnapshotV2StorageBundleError,
        completion_abort: Option<PreparedSnapshotDriveRestoreCompletionError>,
    },
}

impl SnapshotV2StorageRestoreBundleError {
    pub(crate) const fn disposition(&self) -> SnapshotRestoreResourceDisposition {
        match self {
            Self::Resources(source) => source.disposition(),
            Self::Plan(
                SnapshotV2StorageRestorePlanError::Allocation
                | SnapshotV2StorageRestorePlanError::Block(
                    SnapshotV2MultiBlockRestorePlanError::Allocation,
                ),
            ) => SnapshotRestoreResourceDisposition::Retryable,
            Self::Plan(_) => SnapshotRestoreResourceDisposition::Terminal,
            Self::Bundle {
                completion_abort: Some(_),
                ..
            } => SnapshotRestoreResourceDisposition::Terminal,
            Self::Bundle {
                source,
                completion_abort: None,
            } if source.is_retryable() => SnapshotRestoreResourceDisposition::Retryable,
            Self::Bundle { .. } => SnapshotRestoreResourceDisposition::Terminal,
        }
    }
}

impl fmt::Debug for SnapshotV2StorageRestoreBundleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::Resources(_) => "Resources",
            Self::Plan(_) => "Plan",
            Self::Bundle { .. } => "Bundle",
        };
        formatter
            .debug_struct("SnapshotV2StorageRestoreBundleError")
            .field("kind", &kind)
            .field("disposition", &self.disposition())
            .field("state", &"<redacted>")
            .finish()
    }
}

impl fmt::Display for SnapshotV2StorageRestoreBundleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "snapshot storage restore bundle preparation failed ({:?})",
            self.disposition()
        )
    }
}

impl std::error::Error for SnapshotV2StorageRestoreBundleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Resources(source) => Some(source),
            Self::Plan(source) => Some(source),
            Self::Bundle { source, .. } => Some(source),
        }
    }
}

pub(crate) enum SnapshotV2MultiBlockRestoreBundleError {
    Resources(SnapshotRestoreResourceError),
    Plan(SnapshotV2MultiBlockRestorePlanError),
    Bundle {
        source: SnapshotV2MultiBlockBundleError,
        completion_abort: Option<PreparedSnapshotDriveRestoreCompletionError>,
    },
}

impl SnapshotV2MultiBlockRestoreBundleError {
    pub(crate) const fn disposition(&self) -> SnapshotRestoreResourceDisposition {
        match self {
            Self::Resources(source) => source.disposition(),
            Self::Plan(SnapshotV2MultiBlockRestorePlanError::Allocation) => {
                SnapshotRestoreResourceDisposition::Retryable
            }
            Self::Plan(_) => SnapshotRestoreResourceDisposition::Terminal,
            Self::Bundle {
                completion_abort: Some(_),
                ..
            } => SnapshotRestoreResourceDisposition::Terminal,
            Self::Bundle {
                source:
                    SnapshotV2MultiBlockBundleError::Allocation
                    | SnapshotV2MultiBlockBundleError::AsyncBinding {
                        source:
                            BlockAsyncRuntimeError::MetadataAllocation
                            | BlockAsyncRuntimeError::BuildExecutor(_)
                            | BlockAsyncRuntimeError::DriveBuild(_),
                        cleanup: None,
                    },
                completion_abort: None,
            } => SnapshotRestoreResourceDisposition::Retryable,
            Self::Bundle { .. } => SnapshotRestoreResourceDisposition::Terminal,
        }
    }
}

impl fmt::Debug for SnapshotV2MultiBlockRestoreBundleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::Resources(_) => "Resources",
            Self::Plan(_) => "Plan",
            Self::Bundle { .. } => "Bundle",
        };
        formatter
            .debug_struct("SnapshotV2MultiBlockRestoreBundleError")
            .field("kind", &kind)
            .field("disposition", &self.disposition())
            .field("state", &"<redacted>")
            .finish()
    }
}

impl fmt::Display for SnapshotV2MultiBlockRestoreBundleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "snapshot multi-block restore bundle preparation failed ({:?})",
            self.disposition()
        )
    }
}

impl std::error::Error for SnapshotV2MultiBlockRestoreBundleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Resources(source) => Some(source),
            Self::Plan(source) => Some(source),
            Self::Bundle { source, .. } => Some(source),
        }
    }
}

pub(crate) enum RequestedSnapshotRestoreResource {
    Root {
        key: SnapshotRestoreResourceKey,
        selector: PathBuf,
    },
    Vsock {
        key: SnapshotRestoreResourceKey,
        request: RequestedVsockRestoreResource,
    },
}

impl RequestedSnapshotRestoreResource {
    fn key(&self) -> &SnapshotRestoreResourceKey {
        match self {
            Self::Root { key, .. } | Self::Vsock { key, .. } => key,
        }
    }

    const fn expected_class(&self) -> SnapshotRestoreResourceClass {
        match self {
            Self::Root { .. } => SnapshotRestoreResourceClass::BlockBacking,
            Self::Vsock { .. } => SnapshotRestoreResourceClass::VsockEndpoint,
        }
    }

    fn overridden_key(&self) -> Option<SnapshotRestoreResourceKey> {
        match self {
            Self::Vsock { key, request } if request.is_overridden() => Some(key.clone()),
            Self::Root { .. } | Self::Vsock { .. } => None,
        }
    }
}

impl fmt::Debug for RequestedSnapshotRestoreResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RequestedSnapshotRestoreResource")
            .field("class", &self.expected_class())
            .field("value", &"<redacted>")
            .finish()
    }
}

pub(crate) struct RequestedSnapshotRestoreResources {
    root_key: SnapshotRestoreResourceKey,
    root_selector: PathBuf,
    vsock_key: Option<SnapshotRestoreResourceKey>,
    vsock_request: Option<RequestedVsockRestoreResource>,
    bindings: SnapshotRestoreBindings<PreparedSnapshotRestoreResource>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotRestorePreparationStep {
    RootReservation,
    VsockReservation,
    RootLocalPreparation,
    VsockLocalPreparation,
    VsockPublication,
    RootBinding,
    VsockBinding,
    Completion,
    VsockAbort,
    RootAbort,
}

enum SnapshotRestoreReservationSource<'a> {
    Direct,
    Contained(&'a ContainedSnapshotRestoreAuthority),
    #[cfg(test)]
    Independent {
        root: Option<&'a GrantAuthority>,
        directory: Option<&'a DirectoryGrantAuthority>,
        broker: Option<&'a SocketBrokerAuthority>,
        namespace: Option<&'a WorkerSocketNamespace>,
    },
}

impl RequestedSnapshotRestoreResources {
    /// Active profile-2 pathless bundle producer. It performs pure loaded-
    /// memory planning before opening or taking one backing and retains the
    /// aggregate completion without publishing controller or VM state.
    pub(crate) fn prepare_native_v2_multi_block_restore_bundle<F>(
        graph: SnapshotV2MultiBlockDeviceGraph,
        memory: &GuestMemory,
        now: Instant,
        authority: Option<&ContainedSnapshotRestoreAuthority>,
        cancelled: F,
    ) -> Result<PreparedSnapshotV2MultiBlockRestoreBundle, SnapshotV2MultiBlockRestoreBundleError>
    where
        F: Fn() -> bool,
    {
        // Keep the complete ownership transaction contract explicit at the
        // public profile-2 activation seam.
        let _bundle = PreparedSnapshotV2MultiBlockRestoreBundle::bundle;
        let _construct = |prepared: PreparedSnapshotV2MultiBlockRestoreBundle| {
            prepared.construct_destination(Ok::<_, std::convert::Infallible>)
        };
        let _commit = |destination: PreparedSnapshotV2MultiBlockDestination<()>| {
            destination.commit(
                |destination| Ok::<_, ((), std::convert::Infallible)>((destination, ())),
                |_| Ok::<_, std::convert::Infallible>(()),
            )
        };
        let _abort = PreparedSnapshotV2MultiBlockRestoreBundle::abort;
        Self::prepare_native_v2_multi_block_restore_bundle_with(
            graph,
            memory,
            now,
            authority,
            cancelled,
            SnapshotV2MultiBlockRestorePlan::prepare_backings,
        )
    }

    fn prepare_native_v2_multi_block_restore_bundle_with<F, B>(
        graph: SnapshotV2MultiBlockDeviceGraph,
        memory: &GuestMemory,
        now: Instant,
        authority: Option<&ContainedSnapshotRestoreAuthority>,
        cancelled: F,
        build_bundle: B,
    ) -> Result<PreparedSnapshotV2MultiBlockRestoreBundle, SnapshotV2MultiBlockRestoreBundleError>
    where
        F: Fn() -> bool,
        B: FnOnce(
            SnapshotV2MultiBlockRestorePlan,
            DriveConfigs,
            Vec<BlockFileBacking>,
        )
            -> Result<PreparedSnapshotV2MultiBlockBundle, SnapshotV2MultiBlockBundleError>,
    {
        let requested =
            RequestedSnapshotMultiDriveRestoreResources::try_from_native_v2_multi_block_device_graph(
                &graph,
            )
            .map_err(SnapshotV2MultiBlockRestoreBundleError::Resources)?;
        let plan = SnapshotV2MultiBlockRestorePlan::prepare(graph, memory, now)
            .map_err(SnapshotV2MultiBlockRestoreBundleError::Plan)?;
        let batch = requested
            .prepare(authority, cancelled)
            .and_then(PreparedSnapshotMultiDriveRestoreResources::into_drive_batch)
            .map_err(SnapshotV2MultiBlockRestoreBundleError::Resources)?;
        let (drive_configs, backings, completion) = batch.into_parts();
        match build_bundle(plan, drive_configs, backings) {
            Ok(bundle) => Ok(PreparedSnapshotV2MultiBlockRestoreBundle {
                bundle: Some(bundle),
                completion: Some(completion),
            }),
            Err(source) => {
                let completion_abort = completion.abort().err();
                Err(SnapshotV2MultiBlockRestoreBundleError::Bundle {
                    source,
                    completion_abort,
                })
            }
        }
    }

    /// Dormant profile-3 pathless bundle producer. It performs pure loaded-
    /// memory planning before opening or taking one backing and retains the
    /// aggregate completion without publishing transport, controller, or VM
    /// state.
    pub(crate) fn prepare_native_v2_storage_restore_bundle<F>(
        graph: SnapshotV2StorageDeviceGraph,
        memory: &GuestMemory,
        now: Instant,
        authority: Option<&ContainedSnapshotRestoreAuthority>,
        cancelled: F,
    ) -> Result<PreparedSnapshotV2StorageRestoreBundle, SnapshotV2StorageRestoreBundleError>
    where
        F: Fn() -> bool,
    {
        let _bundle = PreparedSnapshotV2StorageRestoreBundle::bundle;
        let _construct = |prepared: PreparedSnapshotV2StorageRestoreBundle| {
            prepared.construct_destination(Ok::<_, std::convert::Infallible>)
        };
        let _commit = |destination: PreparedSnapshotV2StorageDestination<()>| {
            destination.commit(
                |destination| Ok::<_, ((), std::convert::Infallible)>((destination, ())),
                |_| Ok::<_, std::convert::Infallible>(()),
            )
        };
        let _abort = PreparedSnapshotV2StorageRestoreBundle::abort;
        Self::prepare_native_v2_storage_restore_bundle_with(
            graph,
            memory,
            now,
            authority,
            cancelled,
            |plan, blocks, pmems, cancelled| plan.prepare_backings(blocks, pmems, cancelled),
        )
    }

    fn prepare_native_v2_storage_restore_bundle_with<F, B>(
        graph: SnapshotV2StorageDeviceGraph,
        memory: &GuestMemory,
        now: Instant,
        authority: Option<&ContainedSnapshotRestoreAuthority>,
        cancelled: F,
        build_bundle: B,
    ) -> Result<PreparedSnapshotV2StorageRestoreBundle, SnapshotV2StorageRestoreBundleError>
    where
        F: Fn() -> bool,
        B: FnOnce(
            SnapshotV2StorageRestorePlan,
            Vec<BlockFileBacking>,
            Vec<PmemFileBacking>,
            &F,
        ) -> Result<PreparedSnapshotV2StorageBundle, SnapshotV2StorageBundleError>,
    {
        let mut expected_keys = Vec::new();
        expected_keys
            .try_reserve_exact(graph.record_count())
            .map_err(|_| {
                SnapshotV2StorageRestoreBundleError::Plan(
                    SnapshotV2StorageRestorePlanError::Allocation,
                )
            })?;
        expected_keys.extend(
            graph
                .block_records()
                .iter()
                .map(|record| (record.key(), SnapshotRestoreResourceClass::BlockBacking))
                .chain(
                    graph
                        .pmem_records()
                        .iter()
                        .map(|record| (record.key(), SnapshotRestoreResourceClass::PmemBacking)),
                ),
        );
        let requested =
            RequestedSnapshotStorageRestoreResources::try_from_native_v2_storage_device_graph(
                &graph,
            )
            .map_err(SnapshotV2StorageRestoreBundleError::Resources)?;
        let plan = SnapshotV2StorageRestorePlan::prepare(graph, memory, now)
            .map_err(SnapshotV2StorageRestoreBundleError::Plan)?;
        let batch = requested
            .prepare(authority, &cancelled)
            .and_then(PreparedSnapshotStorageRestoreResources::into_storage_batch)
            .map_err(SnapshotV2StorageRestoreBundleError::Resources)?;
        let (blocks, pmems, completion) = batch.into_parts();
        let mut block_backings = Vec::new();
        let mut pmem_backings = Vec::new();
        if block_backings.try_reserve_exact(blocks.len()).is_err()
            || pmem_backings.try_reserve_exact(pmems.len()).is_err()
        {
            drop(blocks);
            drop(pmems);
            let outcome = abort_drive_completion_outcome(completion);
            return Err(SnapshotV2StorageRestoreBundleError::Resources(
                SnapshotRestoreResourceError::retryable(
                    SnapshotRestoreResourceStage::Finish,
                    SnapshotRestoreResourceErrorKind::InvalidStorageSet,
                )
                .with_abort_outcome(outcome),
            ));
        }

        let mut expected = expected_keys.iter();
        let mut valid = true;
        for owner in blocks {
            let (key, backing) = owner.into_parts();
            valid &= expected.next().is_some_and(|(device_key, resource_class)| {
                key.device_key() == *device_key
                    && key.resource_class() == *resource_class
                    && *resource_class == SnapshotRestoreResourceClass::BlockBacking
            });
            block_backings.push(backing);
        }
        for owner in pmems {
            let (key, backing) = owner.into_parts();
            valid &= expected.next().is_some_and(|(device_key, resource_class)| {
                key.device_key() == *device_key
                    && key.resource_class() == *resource_class
                    && *resource_class == SnapshotRestoreResourceClass::PmemBacking
            });
            pmem_backings.push(backing);
        }
        valid &= expected.next().is_none();
        if !valid {
            drop(block_backings);
            drop(pmem_backings);
            let outcome = abort_drive_completion_outcome(completion);
            return Err(SnapshotV2StorageRestoreBundleError::Resources(
                SnapshotRestoreResourceError::terminal(
                    SnapshotRestoreResourceStage::Finish,
                    SnapshotRestoreResourceErrorKind::InvalidStorageSet,
                )
                .with_abort_outcome(outcome),
            ));
        }

        match build_bundle(plan, block_backings, pmem_backings, &cancelled) {
            Ok(bundle) => Ok(PreparedSnapshotV2StorageRestoreBundle {
                bundle: Some(bundle),
                completion: Some(completion),
            }),
            Err(source) => {
                let completion_abort = completion.abort().err();
                Err(SnapshotV2StorageRestoreBundleError::Bundle {
                    source,
                    completion_abort,
                })
            }
        }
    }

    /// Typed profile-2 resource producer retained for focused handoff tests.
    ///
    /// Public activation consumes the composed restore-bundle variant above.
    pub(crate) fn prepare_native_v2_multi_block_device_graph<F>(
        graph: &SnapshotV2MultiBlockDeviceGraph,
        authority: Option<&ContainedSnapshotRestoreAuthority>,
        cancelled: F,
    ) -> Result<PreparedSnapshotRestoreDriveBatch, SnapshotRestoreResourceError>
    where
        F: Fn() -> bool,
    {
        // Keep the lower-level handoff contract type-checked alongside the
        // composed public restore-bundle path.
        let _batch_parts = PreparedSnapshotRestoreDriveBatch::into_parts;
        let _commit = PreparedSnapshotDriveRestoreCompletion::commit;
        let _abort = PreparedSnapshotDriveRestoreCompletion::abort;
        let _completion_disposition = PreparedSnapshotDriveRestoreCompletionError::disposition;
        RequestedSnapshotMultiDriveRestoreResources::try_from_native_v2_multi_block_device_graph(
            graph,
        )?
        .prepare(authority, cancelled)?
        .into_drive_batch()
    }

    /// Typed exact-2.6 resource producer retained for the pathless storage
    /// reconstruction child.
    pub(crate) fn prepare_native_v2_storage_device_graph<F>(
        graph: &SnapshotV2StorageDeviceGraph,
        authority: Option<&ContainedSnapshotRestoreAuthority>,
        cancelled: F,
    ) -> Result<PreparedSnapshotStorageRestoreBatch, SnapshotRestoreResourceError>
    where
        F: Fn() -> bool,
    {
        let _batch_parts = PreparedSnapshotStorageRestoreBatch::into_parts;
        let _block_parts = PreparedSnapshotBlockRestoreBacking::into_parts;
        let _pmem_parts = PreparedSnapshotPmemRestoreBacking::into_parts;
        let _commit = PreparedSnapshotDriveRestoreCompletion::commit;
        let _abort = PreparedSnapshotDriveRestoreCompletion::abort;
        RequestedSnapshotStorageRestoreResources::try_from_native_v2_storage_device_graph(graph)?
            .prepare(authority, cancelled)?
            .into_storage_batch()
    }

    pub(crate) fn try_from_native_v2_device_graph(
        graph: &SnapshotV2DeviceGraph,
    ) -> Result<Self, SnapshotRestoreResourceError> {
        // Keep the dormant typed producer linked without admitting profile 2
        // to public dispatch.
        let _profile_2_producer = Self::prepare_native_v2_multi_block_device_graph::<fn() -> bool>;
        let _profile_2_bundle_producer =
            Self::prepare_native_v2_multi_block_restore_bundle::<fn() -> bool>;
        let _profile_3_producer = Self::prepare_native_v2_storage_device_graph::<fn() -> bool>;
        let _profile_3_bundle_producer =
            Self::prepare_native_v2_storage_restore_bundle::<fn() -> bool>;
        Self::try_from_native_v2_device_graph_and_vsock(graph, None)
    }

    fn try_from_native_v2_device_graph_and_vsock(
        graph: &SnapshotV2DeviceGraph,
        vsock: Option<(SnapshotRestoreResourceKey, RequestedVsockRestoreResource)>,
    ) -> Result<Self, SnapshotRestoreResourceError> {
        let public_id = SnapshotRestorePublicId::try_from(graph.record().config().drive_id())
            .map_err(|source| {
                SnapshotRestoreResourceError::retryable(
                    SnapshotRestoreResourceStage::Manifest,
                    SnapshotRestoreResourceErrorKind::Manifest(
                        SnapshotRestoreManifestError::PublicId { source },
                    ),
                )
            })?;
        let key = SnapshotRestoreResourceKey::new(
            graph.root_key(),
            public_id,
            SnapshotRestoreResourceClass::BlockBacking,
        );
        let mut requests = vec![RequestedSnapshotRestoreResource::Root {
            key,
            selector: PathBuf::from(graph.record().config().selector()),
        }];
        if let Some((key, request)) = vsock {
            requests.push(RequestedSnapshotRestoreResource::Vsock { key, request });
        }
        Self::try_from_exact_requests(requests)
    }

    pub(crate) fn try_from_exact_requests(
        mut requests: Vec<RequestedSnapshotRestoreResource>,
    ) -> Result<Self, SnapshotRestoreResourceError> {
        let mut resources = Vec::new();
        resources
            .try_reserve_exact(requests.len())
            .map_err(manifest_allocation_error)?;
        let mut overrides = Vec::new();
        overrides
            .try_reserve_exact(requests.len())
            .map_err(manifest_allocation_error)?;
        for request in &requests {
            if request.key().resource_class() != request.expected_class() {
                return Err(SnapshotRestoreResourceError::terminal(
                    SnapshotRestoreResourceStage::Manifest,
                    SnapshotRestoreResourceErrorKind::OwnerClassMismatch,
                ));
            }
            resources.push(request.key().clone());
            if let Some(overridden) = request.overridden_key() {
                overrides.push(overridden);
            }
        }
        let manifest =
            SnapshotRestoreManifest::try_new(resources, overrides).map_err(|source| {
                SnapshotRestoreResourceError::retryable(
                    SnapshotRestoreResourceStage::Manifest,
                    SnapshotRestoreResourceErrorKind::Manifest(source),
                )
            })?;
        requests.sort_unstable_by(|left, right| left.key().cmp(right.key()));

        let mut root = None;
        let mut vsock = None;
        for request in requests {
            match request {
                RequestedSnapshotRestoreResource::Root { key, selector } => {
                    if root.replace((key, selector)).is_some() {
                        return Err(SnapshotRestoreResourceError::terminal(
                            SnapshotRestoreResourceStage::Manifest,
                            SnapshotRestoreResourceErrorKind::OwnerClassMismatch,
                        ));
                    }
                }
                RequestedSnapshotRestoreResource::Vsock { key, request } => {
                    if vsock.replace((key, request)).is_some() {
                        return Err(SnapshotRestoreResourceError::terminal(
                            SnapshotRestoreResourceStage::Manifest,
                            SnapshotRestoreResourceErrorKind::OwnerClassMismatch,
                        ));
                    }
                }
            }
        }
        let (root_key, root_selector) = root.ok_or_else(|| {
            SnapshotRestoreResourceError::terminal(
                SnapshotRestoreResourceStage::Manifest,
                SnapshotRestoreResourceErrorKind::OwnerClassMismatch,
            )
        })?;
        let (vsock_key, vsock_request) = match vsock {
            Some((key, request)) => (Some(key), Some(request)),
            None => (None, None),
        };
        if manifest.len() != usize::from(vsock_key.is_some()).saturating_add(1) {
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
            root_selector,
            vsock_key,
            vsock_request,
            bindings,
        })
    }

    pub(crate) fn prepare_root(
        self,
        authority: Option<&ContainedSnapshotRestoreAuthority>,
        cancelled: impl Fn() -> bool,
    ) -> Result<PreparedSnapshotRestoreResources, SnapshotRestoreResourceError> {
        self.prepare(authority, cancelled)
    }

    pub(crate) fn prepare(
        self,
        authority: Option<&ContainedSnapshotRestoreAuthority>,
        cancelled: impl Fn() -> bool,
    ) -> Result<PreparedSnapshotRestoreResources, SnapshotRestoreResourceError> {
        self.prepare_from_source_with_observer(
            match authority {
                Some(authority) => SnapshotRestoreReservationSource::Contained(authority),
                None => SnapshotRestoreReservationSource::Direct,
            },
            cancelled,
            |_| {},
        )
    }

    #[cfg(test)]
    fn prepare_with_independent(
        self,
        root_authority: Option<&GrantAuthority>,
        directory_authority: Option<&DirectoryGrantAuthority>,
        broker_authority: Option<&SocketBrokerAuthority>,
        namespace: Option<&WorkerSocketNamespace>,
        cancelled: impl Fn() -> bool,
    ) -> Result<PreparedSnapshotRestoreResources, SnapshotRestoreResourceError> {
        self.prepare_with_independent_observer(
            root_authority,
            directory_authority,
            broker_authority,
            namespace,
            cancelled,
            |_| {},
        )
    }

    #[cfg(test)]
    fn prepare_with_independent_observer(
        self,
        root_authority: Option<&GrantAuthority>,
        directory_authority: Option<&DirectoryGrantAuthority>,
        broker_authority: Option<&SocketBrokerAuthority>,
        namespace: Option<&WorkerSocketNamespace>,
        cancelled: impl Fn() -> bool,
        observe: impl FnMut(SnapshotRestorePreparationStep),
    ) -> Result<PreparedSnapshotRestoreResources, SnapshotRestoreResourceError> {
        self.prepare_from_source_with_observer(
            SnapshotRestoreReservationSource::Independent {
                root: root_authority,
                directory: directory_authority,
                broker: broker_authority,
                namespace,
            },
            cancelled,
            observe,
        )
    }

    fn prepare_from_source_with_observer(
        self,
        source: SnapshotRestoreReservationSource<'_>,
        cancelled: impl Fn() -> bool,
        mut observe: impl FnMut(SnapshotRestorePreparationStep),
    ) -> Result<PreparedSnapshotRestoreResources, SnapshotRestoreResourceError> {
        let Self {
            root_key,
            root_selector,
            vsock_key,
            vsock_request,
            mut bindings,
        } = self;
        if cancelled() {
            return Err(
                cancelled_batch_error().with_abort_outcome(abort_resources(bindings.into_values()))
            );
        }

        observe(SnapshotRestorePreparationStep::RootReservation);
        let (root_reserved, vsock_reserved, contained_transaction) = match source {
            SnapshotRestoreReservationSource::Direct => {
                let root_reserved =
                    match ReservedSnapshotRootRestoreResource::reserve(&root_selector, None) {
                        Ok(reserved) => reserved,
                        Err(source) => {
                            let outcome = abort_resources(bindings.into_values());
                            return Err(source.with_abort_outcome(outcome));
                        }
                    };
                let vsock_reserved = match vsock_request {
                    Some(request) => {
                        observe(SnapshotRestorePreparationStep::VsockReservation);
                        match request.reserve(None, None, None, &cancelled) {
                            Ok(reserved) => Some(reserved),
                            Err(source) => {
                                let outcome = root_reserved
                                    .abort()
                                    .merge(abort_resources(bindings.into_values()));
                                return Err(
                                    snapshot_vsock_error(source).with_abort_outcome(outcome)
                                );
                            }
                        }
                    }
                    None => None,
                };
                (root_reserved, vsock_reserved, None)
            }
            SnapshotRestoreReservationSource::Contained(authority) => {
                if vsock_request.is_some() {
                    observe(SnapshotRestorePreparationStep::VsockReservation);
                }
                let reserved = authority
                    .prepare(
                        &root_selector,
                        vsock_request
                            .as_ref()
                            .map(RequestedVsockRestoreResource::destination_reference),
                        &cancelled,
                    )
                    .map_err(snapshot_contained_error)?;
                let (root_claim, vsock_facets, transaction) =
                    reserved.into_parts().map_err(snapshot_contained_error)?;
                let root_reserved = ReservedSnapshotRootRestoreResource::from_prepared_claim(
                    root_selector,
                    root_claim,
                );
                let vsock_reserved = match (vsock_request, vsock_facets) {
                    (Some(request), Some((claim, broker, namespace))) => {
                        Some(request.reserve_contained(claim, broker, namespace))
                    }
                    (None, None) => None,
                    (Some(_), None) => {
                        let outcome = root_reserved
                            .abort()
                            .merge(abort_contained_transaction(Some(transaction)));
                        let error = ContainedSnapshotRestoreError::invalid_request();
                        return Err(snapshot_contained_error(error).with_abort_outcome(outcome));
                    }
                    (None, Some((directory, broker, namespace))) => {
                        drop(namespace);
                        let owner_cleanup_failed =
                            broker.abort().is_err() | directory.abort().is_err();
                        let owner_outcome = if owner_cleanup_failed {
                            SnapshotRestoreAbortOutcome::terminal(true)
                        } else {
                            SnapshotRestoreAbortOutcome::retryable()
                        };
                        let outcome = owner_outcome
                            .merge(root_reserved.abort())
                            .merge(abort_contained_transaction(Some(transaction)));
                        let error = ContainedSnapshotRestoreError::invalid_request();
                        return Err(snapshot_contained_error(error).with_abort_outcome(outcome));
                    }
                };
                (root_reserved, vsock_reserved, Some(transaction))
            }
            #[cfg(test)]
            SnapshotRestoreReservationSource::Independent {
                root,
                directory,
                broker,
                namespace,
            } => {
                let root_reserved =
                    match ReservedSnapshotRootRestoreResource::reserve(&root_selector, root) {
                        Ok(reserved) => reserved,
                        Err(source) => {
                            let outcome = abort_resources(bindings.into_values());
                            return Err(source.with_abort_outcome(outcome));
                        }
                    };
                let vsock_reserved = match vsock_request {
                    Some(request) => {
                        observe(SnapshotRestorePreparationStep::VsockReservation);
                        match request.reserve(directory, broker, namespace, &cancelled) {
                            Ok(reserved) => Some(reserved),
                            Err(source) => {
                                let outcome = root_reserved
                                    .abort()
                                    .merge(abort_resources(bindings.into_values()));
                                return Err(
                                    snapshot_vsock_error(source).with_abort_outcome(outcome)
                                );
                            }
                        }
                    }
                    None => None,
                };
                (root_reserved, vsock_reserved, None)
            }
        };
        if cancelled() {
            let outcome = abort_reserved_vsock(vsock_reserved)
                .merge(root_reserved.abort())
                .merge(abort_resources(bindings.into_values()))
                .merge(abort_contained_transaction(contained_transaction));
            return Err(cancelled_batch_error().with_abort_outcome(outcome));
        }

        observe(SnapshotRestorePreparationStep::RootLocalPreparation);
        let root = match root_reserved.prepare_local() {
            Ok(root) => root,
            Err(failure) => {
                let (source, root_reserved) = *failure;
                let outcome = abort_reserved_vsock(vsock_reserved)
                    .merge(root_reserved.abort())
                    .merge(abort_resources(bindings.into_values()))
                    .merge(abort_contained_transaction(contained_transaction));
                return Err(SnapshotRestoreResourceError::retryable(
                    SnapshotRestoreResourceStage::RootPreparation,
                    SnapshotRestoreResourceErrorKind::RootBacking(source),
                )
                .with_abort_outcome(outcome));
            }
        };
        let vsock_local = match vsock_reserved {
            Some(reserved) => {
                observe(SnapshotRestorePreparationStep::VsockLocalPreparation);
                match reserved.prepare_local(&cancelled) {
                    Ok(local) => Some(local),
                    Err(source) => {
                        let outcome = prepared_root_abort_outcome(root)
                            .merge(abort_resources(bindings.into_values()))
                            .merge(abort_contained_transaction(contained_transaction));
                        return Err(snapshot_vsock_error(source).with_abort_outcome(outcome));
                    }
                }
            }
            None => None,
        };
        if cancelled() {
            let outcome = abort_local_vsock(vsock_local)
                .merge(prepared_root_abort_outcome(root))
                .merge(abort_resources(bindings.into_values()))
                .merge(abort_contained_transaction(contained_transaction));
            return Err(cancelled_batch_error().with_abort_outcome(outcome));
        }
        let vsock = match vsock_local {
            Some(local) => {
                observe(SnapshotRestorePreparationStep::VsockPublication);
                match local.publish(&cancelled) {
                    Ok(vsock) => Some(vsock),
                    Err(source) => {
                        let outcome = prepared_root_abort_outcome(root)
                            .merge(abort_resources(bindings.into_values()))
                            .merge(abort_contained_transaction(contained_transaction));
                        return Err(snapshot_vsock_error(source).with_abort_outcome(outcome));
                    }
                }
            }
            None => None,
        };

        observe(SnapshotRestorePreparationStep::RootBinding);
        let root_owner = PreparedSnapshotRestoreResource::Root(root);
        if let Err(rejection) = bindings.bind(&root_key, root_owner) {
            let reason = rejection.reason();
            if vsock.is_some() {
                observe(SnapshotRestorePreparationStep::VsockAbort);
            }
            let outcome = prepared_vsock_abort_outcome(vsock)
                .merge({
                    observe(SnapshotRestorePreparationStep::RootAbort);
                    rejection.into_value().abort()
                })
                .merge(abort_resources(bindings.into_values()))
                .merge(abort_contained_transaction(contained_transaction));
            return Err(SnapshotRestoreResourceError::terminal(
                SnapshotRestoreResourceStage::Binding,
                SnapshotRestoreResourceErrorKind::Binding(reason),
            )
            .with_abort_outcome(outcome));
        }
        if let (Some(key), Some(vsock)) = (vsock_key.as_ref(), vsock) {
            observe(SnapshotRestorePreparationStep::VsockBinding);
            let owner = PreparedSnapshotRestoreResource::Vsock(vsock);
            if let Err(rejection) = bindings.bind(key, owner) {
                let reason = rejection.reason();
                let outcome = rejection
                    .into_value()
                    .abort()
                    .merge(abort_resources(bindings.into_values()))
                    .merge(abort_contained_transaction(contained_transaction));
                return Err(SnapshotRestoreResourceError::terminal(
                    SnapshotRestoreResourceStage::Binding,
                    SnapshotRestoreResourceErrorKind::Binding(reason),
                )
                .with_abort_outcome(outcome));
            }
        }

        observe(SnapshotRestorePreparationStep::Completion);
        let bindings = match bindings.complete() {
            Ok(bindings) => bindings,
            Err(incomplete) => {
                let missing_count = incomplete.missing_count();
                let outcome = abort_resources(incomplete.into_bindings().into_values())
                    .merge(abort_contained_transaction(contained_transaction));
                return Err(SnapshotRestoreResourceError::terminal(
                    SnapshotRestoreResourceStage::Completion,
                    SnapshotRestoreResourceErrorKind::Incomplete { missing_count },
                )
                .with_abort_outcome(outcome));
            }
        };
        let prepared = PreparedSnapshotRestoreResources {
            root_key,
            vsock_key,
            bindings,
            contained_transaction,
        };
        if cancelled() {
            let outcome = prepared.abort();
            return Err(cancelled_batch_error().with_abort_outcome(outcome));
        }
        Ok(prepared)
    }

    #[cfg(test)]
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

    #[cfg(test)]
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
            let outcome = abort_resources(self.bindings.into_values());
            return Err(cancelled_batch_error().with_abort_outcome(outcome));
        }
        let root = match provider(&self.root_selector) {
            Ok(root) => root,
            Err(source) => {
                let outcome = abort_resources(self.bindings.into_values());
                return Err(source.with_abort_outcome(outcome));
            }
        };
        let owner = PreparedSnapshotRestoreResource::Root(root);
        if owner.resource_class() != root_key.resource_class() {
            let outcome = owner
                .abort()
                .merge(abort_resources(self.bindings.into_values()));
            return Err(SnapshotRestoreResourceError::terminal(
                SnapshotRestoreResourceStage::Binding,
                SnapshotRestoreResourceErrorKind::OwnerClassMismatch,
            )
            .with_abort_outcome(outcome));
        }
        if let Err(rejection) = self.bindings.bind(&root_key, owner) {
            let reason = rejection.reason();
            let outcome = rejection
                .into_value()
                .abort()
                .merge(abort_resources(self.bindings.into_values()));
            return Err(SnapshotRestoreResourceError::terminal(
                SnapshotRestoreResourceStage::Binding,
                SnapshotRestoreResourceErrorKind::Binding(reason),
            )
            .with_abort_outcome(outcome));
        }
        let bindings = match self.bindings.complete() {
            Ok(bindings) => bindings,
            Err(incomplete) => {
                let missing_count = incomplete.missing_count();
                let outcome = abort_resources(incomplete.into_bindings().into_values());
                return Err(SnapshotRestoreResourceError::terminal(
                    SnapshotRestoreResourceStage::Completion,
                    SnapshotRestoreResourceErrorKind::Incomplete { missing_count },
                )
                .with_abort_outcome(outcome));
            }
        };
        let prepared = PreparedSnapshotRestoreResources {
            root_key: self.root_key,
            vsock_key: self.vsock_key,
            bindings,
            contained_transaction: None,
        };
        if cancelled() {
            let outcome = prepared.abort();
            return Err(cancelled_batch_error().with_abort_outcome(outcome));
        }
        Ok(prepared)
    }

    #[cfg(test)]
    fn complete(self) -> Result<PreparedSnapshotRestoreResources, SnapshotRestoreResourceError> {
        let bindings = match self.bindings.complete() {
            Ok(bindings) => bindings,
            Err(incomplete) => {
                let missing_count = incomplete.missing_count();
                let outcome = abort_resources(incomplete.into_bindings().into_values());
                return Err(SnapshotRestoreResourceError::terminal(
                    SnapshotRestoreResourceStage::Completion,
                    SnapshotRestoreResourceErrorKind::Incomplete { missing_count },
                )
                .with_abort_outcome(outcome));
            }
        };
        Ok(PreparedSnapshotRestoreResources {
            root_key: self.root_key,
            vsock_key: self.vsock_key,
            bindings,
            contained_transaction: None,
        })
    }
}

fn manifest_allocation_error(source: TryReserveError) -> SnapshotRestoreResourceError {
    SnapshotRestoreResourceError::retryable(
        SnapshotRestoreResourceStage::Manifest,
        SnapshotRestoreResourceErrorKind::Manifest(
            SnapshotRestoreManifestError::AllocationFailed { source },
        ),
    )
}

fn cancelled_batch_error() -> SnapshotRestoreResourceError {
    SnapshotRestoreResourceError::retryable(
        SnapshotRestoreResourceStage::Cancellation,
        SnapshotRestoreResourceErrorKind::Cancelled,
    )
}

fn abort_reserved_vsock(
    reserved: Option<ReservedVsockRestoreResource>,
) -> SnapshotRestoreAbortOutcome {
    match reserved {
        Some(reserved) => vsock_abort_outcome(reserved.abort()),
        None => SnapshotRestoreAbortOutcome::retryable(),
    }
}

fn abort_local_vsock(
    local: Option<LocallyPreparedVsockRestoreResource>,
) -> SnapshotRestoreAbortOutcome {
    match local {
        Some(local) => vsock_abort_outcome(local.abort()),
        None => SnapshotRestoreAbortOutcome::retryable(),
    }
}

fn abort_contained_transaction(
    transaction: Option<ContainedSnapshotRestoreTransaction>,
) -> SnapshotRestoreAbortOutcome {
    match transaction {
        Some(transaction) => match transaction.abort() {
            Ok(()) => SnapshotRestoreAbortOutcome::retryable(),
            Err(_) => SnapshotRestoreAbortOutcome::terminal(true),
        },
        None => SnapshotRestoreAbortOutcome::retryable(),
    }
}

fn prepared_root_abort_outcome(
    root: PreparedSnapshotRootRestoreResource,
) -> SnapshotRestoreAbortOutcome {
    match root.abort() {
        Ok(()) => SnapshotRestoreAbortOutcome::retryable(),
        Err(_) => SnapshotRestoreAbortOutcome::terminal(true),
    }
}

fn prepared_vsock_abort_outcome(
    vsock: Option<PreparedVsockRestoreResource>,
) -> SnapshotRestoreAbortOutcome {
    match vsock {
        Some(vsock) => vsock_abort_outcome(vsock.abort()),
        None => SnapshotRestoreAbortOutcome::retryable(),
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
    vsock_key: Option<SnapshotRestoreResourceKey>,
    bindings: PreparedSnapshotRestoreBindings<PreparedSnapshotRestoreResource>,
    contained_transaction: Option<ContainedSnapshotRestoreTransaction>,
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
            PreparedSnapshotRestoreResource::Vsock(vsock) => {
                Err(SnapshotRestoreResourceError::terminal(
                    SnapshotRestoreResourceStage::Take,
                    SnapshotRestoreResourceErrorKind::OwnerClassMismatch,
                )
                .with_abort_outcome(vsock_abort_outcome(vsock.abort())))
            }
        }
    }

    fn take_vsock(
        &mut self,
        key: &SnapshotRestoreResourceKey,
    ) -> Result<PreparedVsockRestoreResource, SnapshotRestoreResourceError> {
        let owner = self.bindings.take(key).map_err(|source| {
            SnapshotRestoreResourceError::terminal(
                SnapshotRestoreResourceStage::Take,
                SnapshotRestoreResourceErrorKind::Take(source),
            )
        })?;
        match owner {
            PreparedSnapshotRestoreResource::Vsock(vsock) => Ok(vsock),
            PreparedSnapshotRestoreResource::Root(root) => {
                Err(SnapshotRestoreResourceError::terminal(
                    SnapshotRestoreResourceStage::Take,
                    SnapshotRestoreResourceErrorKind::OwnerClassMismatch,
                )
                .with_abort_outcome(prepared_root_abort_outcome(root)))
            }
        }
    }

    fn abort_bindings_and_take_transaction(
        self,
    ) -> (
        SnapshotRestoreAbortOutcome,
        Option<ContainedSnapshotRestoreTransaction>,
    ) {
        (
            abort_resources(self.bindings.into_values()),
            self.contained_transaction,
        )
    }

    #[cfg(test)]
    fn finish(self) -> Result<(), SnapshotRestoreResourceError> {
        let Self {
            root_key: _,
            vsock_key: _,
            bindings,
            contained_transaction,
        } = self;
        match bindings.finish() {
            Ok(()) => match contained_transaction {
                Some(transaction) => transaction.abort().map_err(snapshot_contained_error),
                None => Ok(()),
            },
            Err(unconsumed) => {
                let unconsumed_count = unconsumed.unconsumed_count();
                let outcome = abort_resources(unconsumed.into_bindings().into_values())
                    .merge(abort_contained_transaction(contained_transaction));
                Err(SnapshotRestoreResourceError::terminal(
                    SnapshotRestoreResourceStage::Finish,
                    SnapshotRestoreResourceErrorKind::Unconsumed { unconsumed_count },
                )
                .with_abort_outcome(outcome))
            }
        }
    }

    pub(crate) fn into_root_and_optional_vsock(
        mut self,
    ) -> Result<
        (
            PreparedSnapshotRootRestoreResource,
            Option<PreparedVsockRestoreResource>,
        ),
        SnapshotRestoreResourceError,
    > {
        let root_key = self.root_key.clone();
        let root = match self.take_root(&root_key) {
            Ok(root) => root,
            Err(source) => {
                let outcome = self.abort();
                return Err(source.with_abort_outcome(outcome));
            }
        };
        let vsock = match self.vsock_key.clone() {
            Some(key) => match self.take_vsock(&key) {
                Ok(vsock) => Some(vsock),
                Err(source) => {
                    let (outcome, transaction) = self.abort_bindings_and_take_transaction();
                    let outcome = outcome
                        .merge(prepared_root_abort_outcome(root))
                        .merge(abort_contained_transaction(transaction));
                    return Err(source.with_abort_outcome(outcome));
                }
            },
            None => None,
        };
        let Self {
            root_key: _,
            vsock_key: _,
            bindings,
            contained_transaction,
        } = self;
        let root = match bindings.finish() {
            Ok(()) => match contained_transaction {
                Some(transaction) => root.with_contained_transaction(transaction),
                None => root,
            },
            Err(unconsumed) => {
                let unconsumed_count = unconsumed.unconsumed_count();
                let outcome = abort_resources(unconsumed.into_bindings().into_values())
                    .merge(prepared_vsock_abort_outcome(vsock))
                    .merge(prepared_root_abort_outcome(root))
                    .merge(abort_contained_transaction(contained_transaction));
                return Err(SnapshotRestoreResourceError::terminal(
                    SnapshotRestoreResourceStage::Finish,
                    SnapshotRestoreResourceErrorKind::Unconsumed { unconsumed_count },
                )
                .with_abort_outcome(outcome));
            }
        };
        Ok((root, vsock))
    }

    fn consume_root_with<T>(
        self,
        consumer: impl FnOnce(PreparedSnapshotRootRestoreResource) -> T,
    ) -> Result<T, SnapshotRestoreResourceError> {
        let (root, vsock) = self.into_root_and_optional_vsock()?;
        if let Some(vsock) = vsock {
            let outcome =
                vsock_abort_outcome(vsock.abort()).merge(prepared_root_abort_outcome(root));
            return Err(SnapshotRestoreResourceError::terminal(
                SnapshotRestoreResourceStage::Finish,
                SnapshotRestoreResourceErrorKind::OwnerClassMismatch,
            )
            .with_abort_outcome(outcome));
        }
        Ok(consumer(root))
    }

    pub(crate) fn into_root(
        self,
    ) -> Result<PreparedSnapshotRootRestoreResource, SnapshotRestoreResourceError> {
        self.consume_root_with(|root| root)
    }

    fn abort(self) -> SnapshotRestoreAbortOutcome {
        abort_resources(self.bindings.into_values())
            .merge(abort_contained_transaction(self.contained_transaction))
    }
}

impl fmt::Debug for PreparedSnapshotRestoreResources {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotRestoreResources")
            .field("resource_count", &self.bindings.manifest().len())
            .field("remaining_count", &self.bindings.remaining_count())
            .field(
                "contained_transaction",
                &self.contained_transaction.as_ref().map(|_| "<provisional>"),
            )
            .field("values", &"<redacted>")
            .finish()
    }
}

fn abort_resources(
    resources: impl DoubleEndedIterator<Item = PreparedSnapshotRestoreResource>,
) -> SnapshotRestoreAbortOutcome {
    let mut outcome = SnapshotRestoreAbortOutcome::retryable();
    for resource in resources.rev() {
        outcome = outcome.merge(resource.abort());
    }
    outcome
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::env;
    use std::fs::{self, OpenOptions};
    use std::io::{self, Write};
    use std::rc::Rc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use bangbang_runtime::block::DriveConfigInput;
    use bangbang_runtime::memory::{
        GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange,
    };
    use bangbang_runtime::snapshot::SnapshotVsockOverride;
    use bangbang_runtime::snapshot_device_v2::{
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION, SnapshotV2DeviceTransportKind,
    };
    use bangbang_runtime::snapshot_device_v2_5::{
        NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION, SnapshotV2MultiBlockDeviceGraph,
    };
    use bangbang_runtime::snapshot_device_v2_6::{
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION, SnapshotV2StorageDeviceGraph,
    };
    use bangbang_runtime::snapshot_restore::{
        SnapshotRestoreBindingRejectionReason, SnapshotRestorePublicId,
    };
    use bangbang_runtime::virtio_mmio::VirtioMmioRegisterHandler;
    use bangbang_runtime::vsock::{
        PreparedVsockDevice, VIRTIO_VSOCK_DEVICE_ID, VIRTIO_VSOCK_QUEUE_SIZES,
        VirtioVsockTransportResetAttempt, VsockBackendSelector, VsockConfigInput,
    };
    use bangbang_session::ResourceRole;

    use crate::contained_session::{
        ContainedSnapshotRestoreErrorKind, GrantAuthority, TestContainedRestoreAuthority,
        contained_restore_authority_for_test, contained_restore_authority_with_grants_for_test,
        file_grant_authority_for_test, root_file_grant_authority_for_test,
        snapshot_storage_grant_authority_for_test, vsock_directory_authority_for_test,
    };

    use super::*;

    const ROOT_REFERENCE: &str = "bangbang-grant:drive-ro";

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    struct TempRoot {
        path: PathBuf,
    }

    impl TempRoot {
        fn new(name: &str, bytes: &[u8]) -> Self {
            let path = unique_path(name);
            Self::new_at(path, bytes)
        }

        fn new_at(path: PathBuf, bytes: &[u8]) -> Self {
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

        fn new_sized_at(path: PathBuf, len: u64) -> Self {
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .expect("temporary backing should be created once");
            file.set_len(len)
                .expect("temporary backing length should set");
            file.sync_all()
                .expect("temporary backing should synchronize");
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

    fn unique_short_path() -> PathBuf {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        PathBuf::from(format!("/tmp/bb{id:011x}"))
    }

    fn unique_pmem_short_path() -> PathBuf {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        PathBuf::from(format!("/tmp/p{id:09x}"))
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

    fn multi_fixture_graph(
        first_selector: Option<&Path>,
        second_selector: Option<&Path>,
    ) -> SnapshotV2MultiBlockDeviceGraph {
        let fixture = include_str!("../../runtime/src/snapshot_device_v2_5/fixtures/root-mmio.hex");
        let mut bytes = fixture
            .trim()
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let pair = std::str::from_utf8(pair).expect("fixture hex should be UTF-8");
                u8::from_str_radix(pair, 16).expect("fixture hex should decode")
            })
            .collect::<Vec<_>>();
        for (captured, replacement) in [
            (b"logical-selector-0".as_slice(), first_selector),
            (b"logical-selector-1".as_slice(), second_selector),
        ] {
            let Some(replacement) = replacement else {
                continue;
            };
            let replacement = replacement
                .to_str()
                .expect("test selector should be UTF-8")
                .as_bytes();
            assert_eq!(replacement.len(), captured.len());
            let offset = bytes
                .windows(captured.len())
                .position(|window| window == captured)
                .expect("fixture selector should exist");
            bytes[offset..offset + captured.len()].copy_from_slice(replacement);
        }
        SnapshotV2MultiBlockDeviceGraph::decode(
            NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &bytes,
        )
        .expect("profile-2 fixture graph should decode")
    }

    fn storage_fixture_graph_from_hex(
        fixture: &str,
        selectors: &[(&[u8], &Path)],
    ) -> SnapshotV2StorageDeviceGraph {
        let mut bytes = fixture
            .trim()
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let pair = std::str::from_utf8(pair).expect("fixture hex should be UTF-8");
                u8::from_str_radix(pair, 16).expect("fixture hex should decode")
            })
            .collect::<Vec<_>>();
        for &(captured, replacement) in selectors {
            let replacement = replacement
                .to_str()
                .expect("test selector should be UTF-8")
                .as_bytes();
            assert_eq!(replacement.len(), captured.len());
            let offset = bytes
                .windows(captured.len())
                .position(|window| window == captured)
                .expect("fixture selector should exist");
            bytes[offset..offset + captured.len()].copy_from_slice(replacement);
        }
        SnapshotV2StorageDeviceGraph::decode(
            NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &bytes,
        )
        .expect("profile-3 fixture graph should decode")
    }

    fn storage_fixture_graph(
        block_selector: &Path,
        pmem_selector: &Path,
    ) -> SnapshotV2StorageDeviceGraph {
        storage_fixture_graph_from_hex(
            include_str!(
                "../../runtime/src/snapshot_device_v2_6/fixtures/mixed-block-root-mmio.hex"
            ),
            &[
                (b"logical-selector-0".as_slice(), block_selector),
                (b"pmem-selector-0".as_slice(), pmem_selector),
            ],
        )
    }

    fn contained_storage_request(
        graph: &SnapshotV2StorageDeviceGraph,
        block_reference: &Path,
        pmem_reference: &Path,
    ) -> RequestedSnapshotStorageRestoreResources {
        let mut request =
            RequestedSnapshotStorageRestoreResources::try_from_native_v2_storage_device_graph(
                graph,
            )
            .expect("mixed contained storage request should derive");
        assert_eq!(request.resources.drives.len(), 2);
        request.resources.drives[0].selector = block_reference.to_path_buf();
        request.resources.drives[1].selector = pmem_reference.to_path_buf();
        request
    }

    const fn grant_access(is_read_only: bool) -> GrantAccess {
        if is_read_only {
            GrantAccess::ReadOnly
        } else {
            GrantAccess::ReadWrite
        }
    }

    fn assert_storage_grants_reusable(
        fixture: &TestContainedRestoreAuthority,
        block_reference: &Path,
        block_access: GrantAccess,
        pmem_reference: &Path,
        pmem_access: GrantAccess,
    ) {
        fixture
            .grants()
            .prepare_drive_backing_claim(block_reference, block_access)
            .expect("block grant inspection should succeed")
            .expect("block reference should remain contained")
            .abort()
            .expect("block grant inspection should restore");
        drop(
            fixture
                .grants()
                .prepare_file_claim(pmem_reference, ResourceRole::PmemBacking, pmem_access)
                .expect("pmem grant inspection should succeed")
                .expect("pmem reference should remain contained"),
        );
    }

    fn multi_restore_memory(graph: &SnapshotV2MultiBlockDeviceGraph) -> GuestMemory {
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), 0x80_0000)
                .expect("profile-2 restore memory range should validate"),
        ])
        .expect("profile-2 restore memory layout should validate");
        let mut memory =
            GuestMemory::allocate(&layout).expect("profile-2 restore memory should allocate");
        for record in graph.records() {
            let Some(cursor) = record.block().continuation().active_queue() else {
                continue;
            };
            let queue = record
                .virtio()
                .queues()
                .first()
                .expect("profile-2 fixture queue should exist");
            let available_index = if record.block().continuation().retry()
                == bangbang_runtime::storage_capture::StorageRetryState::None
            {
                cursor.next_available()
            } else {
                cursor.next_available().wrapping_add(1)
            };
            memory
                .write_slice(
                    &available_index.to_le_bytes(),
                    GuestAddress::new(queue.driver_ring().raw_value() + 2),
                )
                .expect("profile-2 available cursor should write");
            memory
                .write_slice(
                    &cursor.next_used().to_le_bytes(),
                    GuestAddress::new(queue.device_ring().raw_value() + 2),
                )
                .expect("profile-2 used cursor should write");
        }
        memory
    }

    fn storage_restore_memory(graph: &SnapshotV2StorageDeviceGraph) -> GuestMemory {
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), 0x80_0000)
                .expect("profile-3 restore memory range should validate"),
        ])
        .expect("profile-3 restore memory layout should validate");
        let mut memory =
            GuestMemory::allocate(&layout).expect("profile-3 restore memory should allocate");
        for record in graph.block_records() {
            let Some(cursor) = record.block().continuation().active_queue() else {
                continue;
            };
            let queue = record
                .virtio()
                .queues()
                .first()
                .expect("profile-3 block queue should exist");
            let available_index = if record.block().continuation().retry()
                == bangbang_runtime::storage_capture::StorageRetryState::None
            {
                cursor.next_available()
            } else {
                cursor.next_available().wrapping_add(1)
            };
            memory
                .write_slice(
                    &available_index.to_le_bytes(),
                    GuestAddress::new(queue.driver_ring().raw_value() + 2),
                )
                .expect("profile-3 block available cursor should write");
            memory
                .write_slice(
                    &cursor.next_used().to_le_bytes(),
                    GuestAddress::new(queue.device_ring().raw_value() + 2),
                )
                .expect("profile-3 block used cursor should write");
        }
        for record in graph.pmem_records() {
            let Some(cursor) = record.pmem().active_queue() else {
                continue;
            };
            let queue = record
                .virtio()
                .queues()
                .first()
                .expect("profile-3 pmem queue should exist");
            let available_index = if record.pmem().retry()
                == bangbang_runtime::storage_capture::StorageRetryState::None
            {
                cursor.next_available()
            } else {
                cursor.next_available().wrapping_add(1)
            };
            memory
                .write_slice(
                    &available_index.to_le_bytes(),
                    GuestAddress::new(queue.driver_ring().raw_value() + 2),
                )
                .expect("profile-3 pmem available cursor should write");
            memory
                .write_slice(
                    &cursor.next_used().to_le_bytes(),
                    GuestAddress::new(queue.device_ring().raw_value() + 2),
                )
                .expect("profile-3 pmem used cursor should write");
        }
        memory
    }

    fn direct_profile_2_process_bundle() -> (
        PreparedSnapshotV2MultiBlockRestoreBundle,
        TempRoot,
        TempRoot,
    ) {
        let template = multi_fixture_graph(None, None);
        let first_path = unique_short_path();
        let second_path = unique_short_path();
        let first = TempRoot::new_at(
            first_path,
            &vec![
                0x61;
                usize::try_from(template.records()[0].block().backing_bytes())
                    .expect("fixture length should fit")
            ],
        );
        let second = TempRoot::new_at(
            second_path,
            &vec![
                0x62;
                usize::try_from(template.records()[1].block().backing_bytes())
                    .expect("fixture length should fit")
            ],
        );
        let graph = multi_fixture_graph(Some(first.path()), Some(second.path()));
        let memory = multi_restore_memory(&graph);
        let prepared =
            RequestedSnapshotRestoreResources::prepare_native_v2_multi_block_restore_bundle(
                graph,
                &memory,
                Instant::now(),
                None,
                || false,
            )
            .expect("direct profile-2 process bundle should prepare");
        (prepared, first, second)
    }

    fn contained_profile_2_lengths() -> [u64; 2] {
        [
            fs::metadata(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/contained_session.rs"))
                .expect("read-only grant metadata should read")
                .len(),
            fs::metadata(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/vmm.rs"))
                .expect("read-write grant metadata should read")
                .len(),
        ]
    }

    fn contained_profile_2_request(
        selectors: [&Path; 2],
        read_only: [bool; 2],
        expected_lengths: [u64; 2],
    ) -> RequestedSnapshotMultiDriveRestoreResources {
        let graph = multi_fixture_graph(None, None);
        let mut drives = Vec::new();
        let mut drive_keys = Vec::new();
        let mut resources = Vec::new();
        let mut configs = DriveConfigs::new();
        for (index, record) in graph.records().iter().enumerate() {
            let public_id = SnapshotRestorePublicId::try_from(record.config().drive_id())
                .expect("fixture public ID should validate");
            let key = SnapshotRestoreResourceKey::new(
                record.key(),
                public_id,
                SnapshotRestoreResourceClass::BlockBacking,
            );
            let mut input = DriveConfigInput::new(
                record.config().drive_id(),
                record.config().drive_id(),
                selectors[index],
                record.is_root(),
            )
            .with_is_read_only(read_only[index])
            .with_cache_type(record.config().cache_type())
            .with_io_engine(record.config().io_engine());
            if let Some(partuuid) = record.config().partuuid() {
                input = input.with_partuuid(partuuid);
            }
            if let Some(rate_limiter) = record.config().rate_limiter() {
                input = input.with_rate_limiter(rate_limiter);
            }
            configs
                .insert(input)
                .expect("contained profile-2 config should validate");
            drives.push(RequestedSnapshotDriveRestoreResource {
                key: key.clone(),
                selector: selectors[index].to_path_buf(),
                is_read_only: read_only[index],
                expected_len: expected_lengths[index],
                kind: SnapshotRestoreFileKind::Block,
            });
            drive_keys.push(key.clone());
            resources.push(key);
        }
        let manifest = SnapshotRestoreManifest::try_new(resources, Vec::new())
            .expect("contained profile-2 manifest should validate");
        let bindings = manifest
            .try_into_bindings()
            .expect("contained profile-2 bindings should allocate");
        RequestedSnapshotMultiDriveRestoreResources {
            drives,
            drive_keys,
            drive_configs: configs,
            bindings,
            file_set_kind: SnapshotRestoreFileSetKind::MultiBlock,
        }
    }

    fn assert_profile_2_grants_reusable(authority: &GrantAuthority) {
        for (selector, access) in [
            (Path::new("bangbang-grant:drive-ro"), GrantAccess::ReadOnly),
            (Path::new("bangbang-grant:drive-rw"), GrantAccess::ReadWrite),
        ] {
            authority
                .prepare_drive_backing_claim(selector, access)
                .expect("retryable profile-2 failure should retain authority")
                .expect("profile-2 selector should remain contained")
                .abort()
                .expect("observed profile-2 claim should restore");
        }
    }

    #[derive(Debug)]
    struct TestPrivateMultiBlockDestination {
        bundle: Option<PreparedSnapshotV2MultiBlockBundle>,
        destroyed: Rc<Cell<bool>>,
    }

    impl TestPrivateMultiBlockDestination {
        fn destroy(mut self) -> Result<(), SnapshotV2MultiBlockCleanupError> {
            self.destroyed.set(true);
            match self.bundle.take() {
                Some(bundle) => bundle.abort(),
                None => Ok(()),
            }
        }
    }

    fn requested() -> RequestedSnapshotRestoreResources {
        RequestedSnapshotRestoreResources::try_from_native_v2_device_graph(&fixture_graph(
            SnapshotV2DeviceTransportKind::Mmio,
        ))
        .expect("fixture request should build")
    }

    fn contained_requested_root() -> RequestedSnapshotRestoreResources {
        let graph = fixture_graph(SnapshotV2DeviceTransportKind::Mmio);
        let key = SnapshotRestoreResourceKey::new(
            graph.root_key(),
            SnapshotRestorePublicId::try_from(graph.record().config().drive_id())
                .expect("fixture root ID should validate"),
            SnapshotRestoreResourceClass::BlockBacking,
        );
        RequestedSnapshotRestoreResources::try_from_exact_requests(vec![
            RequestedSnapshotRestoreResource::Root {
                key,
                selector: PathBuf::from(ROOT_REFERENCE),
            },
        ])
        .expect("contained root request should validate")
    }

    fn socket_path(name: &str) -> PathBuf {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        PathBuf::from(format!(
            "/tmp/bb-srr-{name}-{}-{id}.sock",
            std::process::id()
        ))
    }

    fn selector(path: &Path) -> VsockBackendSelector {
        VsockBackendSelector::try_from_path(path).expect("test selector should validate")
    }

    fn composed_request(
        root_selector: &Path,
        captured: &VsockBackendSelector,
        requested_override: Option<&SnapshotVsockOverride>,
        reverse_caller_order: bool,
    ) -> (
        RequestedSnapshotRestoreResources,
        SnapshotRestoreResourceKey,
        SnapshotRestoreResourceKey,
    ) {
        let graph = fixture_graph(SnapshotV2DeviceTransportKind::Mmio);
        let root_key = SnapshotRestoreResourceKey::new(
            graph.root_key(),
            SnapshotRestorePublicId::try_from(graph.record().config().drive_id())
                .expect("fixture root ID should validate"),
            SnapshotRestoreResourceClass::BlockBacking,
        );
        let vsock_key = SnapshotRestoreResourceKey::new(
            graph.root_key(),
            SnapshotRestorePublicId::try_from("vsock0").expect("vsock ID should validate"),
            SnapshotRestoreResourceClass::VsockEndpoint,
        );
        let requested_vsock =
            RequestedVsockRestoreResource::resolve(Some(captured), requested_override)
                .expect("vsock selectors should resolve")
                .expect("captured vsock should produce a request");
        let root = RequestedSnapshotRestoreResource::Root {
            key: root_key.clone(),
            selector: root_selector.to_path_buf(),
        };
        let vsock = RequestedSnapshotRestoreResource::Vsock {
            key: vsock_key.clone(),
            request: requested_vsock,
        };
        let requests = if reverse_caller_order {
            vec![vsock, root]
        } else {
            vec![root, vsock]
        };
        (
            RequestedSnapshotRestoreResources::try_from_exact_requests(requests)
                .expect("composed request should validate"),
            root_key,
            vsock_key,
        )
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

    fn assert_coherent_root_claim_restored(fixture: &TestContainedRestoreAuthority) {
        assert_root_claim_restored(fixture.grants());
    }

    #[test]
    fn coherent_contained_generation_moves_from_bindings_to_root_completion() {
        let root = TempRoot::new("coherent-contained", b"root");
        let fixture = contained_restore_authority_for_test(root.path(), false);
        let prepared = contained_requested_root()
            .prepare(Some(fixture.authority()), || false)
            .expect("coherent contained batch should prepare");
        let overlap = fixture
            .authority()
            .prepare(Path::new(ROOT_REFERENCE), None, &|| false)
            .expect_err("completed bindings must retain their generation");
        assert_eq!(overlap.kind(), ContainedSnapshotRestoreErrorKind::Busy);

        let root_owner = prepared
            .into_root()
            .expect("exact take and finish should produce the root owner");
        let overlap = fixture
            .authority()
            .prepare(Path::new(ROOT_REFERENCE), None, &|| false)
            .expect_err("root owner completion must retain its generation");
        assert_eq!(overlap.kind(), ContainedSnapshotRestoreErrorKind::Busy);
        let (backing, completion) = root_owner.into_parts();
        drop(backing);
        completion
            .abort()
            .expect("root and generation should abort in order");
        assert_coherent_root_claim_restored(&fixture);

        let committed = contained_requested_root()
            .prepare(Some(fixture.authority()), || false)
            .expect("released transaction should prepare again")
            .into_root()
            .expect("committed transaction should take exactly");
        let (backing, completion) = committed.into_parts();
        drop(backing);
        completion
            .commit()
            .expect("final completion boundary should clear the generation");
        assert!(
            fixture
                .grants()
                .prepare_drive_backing_claim(Path::new(ROOT_REFERENCE), GrantAccess::ReadOnly)
                .is_err(),
            "committed root authority must remain consumed"
        );
        let consumed = fixture
            .authority()
            .prepare(Path::new(ROOT_REFERENCE), None, &|| false)
            .expect_err("consumed root must fail complete-set preflight");
        assert_eq!(
            consumed.kind(),
            ContainedSnapshotRestoreErrorKind::Authority
        );
    }

    #[test]
    fn coherent_contained_abort_restores_root_before_stale_generation_failure() {
        let root = TempRoot::new("coherent-stale", b"root");
        let fixture = contained_restore_authority_for_test(root.path(), false);
        let root_owner = contained_requested_root()
            .prepare(Some(fixture.authority()), || false)
            .expect("coherent contained batch should prepare")
            .into_root()
            .expect("coherent root should take exactly");
        let (backing, completion) = root_owner.into_parts();
        drop(backing);
        fixture.invalidate_generation();
        let error = completion
            .abort()
            .expect_err("stale generation must not be converted into success");
        let diagnostic = format!("{error:?} {error}");
        assert!(!diagnostic.contains(ROOT_REFERENCE));
        assert!(!diagnostic.contains(root.path().to_string_lossy().as_ref()));
        assert_coherent_root_claim_restored(&fixture);
    }

    #[test]
    fn coherent_contained_vsock_facets_abort_before_publication_on_cancellation() {
        let root = TempRoot::new("coherent-vsock-cancel", b"root");
        let fixture = contained_restore_authority_for_test(root.path(), true);
        let contained_vsock = selector(Path::new("bangbang-grant:vsock-directory/restored.sock"));
        let (request, _, _) =
            composed_request(Path::new(ROOT_REFERENCE), &contained_vsock, None, false);
        let checks = Cell::new(0);
        let error = request
            .prepare(Some(fixture.authority()), || {
                let next = checks.get() + 1;
                checks.set(next);
                next > 10
            })
            .expect_err("cancellation after coherent reservation should reverse every facet");
        assert_eq!(error.stage, SnapshotRestoreResourceStage::Cancellation);
        assert_eq!(
            error.disposition,
            SnapshotRestoreResourceDisposition::Retryable
        );
        assert!(!error.cleanup_failed);
        fixture.assert_authorities_available(true);
    }

    #[test]
    fn coherent_contained_vsock_abort_reports_broker_restoration_failure() {
        let root = TempRoot::new("coherent-vsock-broker-loss", b"root");
        let fixture = contained_restore_authority_for_test(root.path(), true);
        let contained_vsock = selector(Path::new("bangbang-grant:vsock-directory/restored.sock"));
        let (request, _, _) =
            composed_request(Path::new(ROOT_REFERENCE), &contained_vsock, None, false);
        let checks = Cell::new(0);
        let error = request
            .prepare(Some(fixture.authority()), || {
                let next = checks.get() + 1;
                checks.set(next);
                if next == 11 {
                    fixture.invalidate_broker();
                    true
                } else {
                    false
                }
            })
            .expect_err("broker invalidation during outer cancellation must be terminal");
        assert_eq!(error.stage, SnapshotRestoreResourceStage::Cancellation);
        assert_eq!(
            error.disposition,
            SnapshotRestoreResourceDisposition::Terminal
        );
        assert!(error.cleanup_failed);
        assert_coherent_root_claim_restored(&fixture);
        let directory = fixture
            .directories()
            .prepare_socket_directory(
                Path::new("bangbang-grant:vsock-directory/restored.sock"),
                bangbang_session::ResourceRole::VsockSocketDirectory,
            )
            .expect("directory restoration should still be attempted")
            .expect("directory reference should remain contained");
        directory
            .abort()
            .expect("restored directory should remain reusable");
    }

    #[test]
    fn coherent_contained_local_cancellation_preserves_cleanup_failure_evidence() {
        let root = TempRoot::new("coherent-vsock-local-broker-loss", b"root");
        let fixture = contained_restore_authority_for_test(root.path(), true);
        let contained_vsock = selector(Path::new("bangbang-grant:vsock-directory/restored.sock"));
        let (request, _, _) =
            composed_request(Path::new(ROOT_REFERENCE), &contained_vsock, None, false);
        let checks = Cell::new(0);
        let error = request
            .prepare(Some(fixture.authority()), || {
                let next = checks.get() + 1;
                checks.set(next);
                if next == 12 {
                    fixture.invalidate_broker();
                    true
                } else {
                    false
                }
            })
            .expect_err("local cancellation must preserve broker cleanup failure evidence");
        assert_eq!(error.stage, SnapshotRestoreResourceStage::VsockPreparation);
        assert_eq!(
            error.disposition,
            SnapshotRestoreResourceDisposition::Terminal
        );
        assert!(error.cleanup_failed);
        assert_coherent_root_claim_restored(&fixture);
        let directory = fixture
            .directories()
            .prepare_socket_directory(
                Path::new("bangbang-grant:vsock-directory/restored.sock"),
                bangbang_session::ResourceRole::VsockSocketDirectory,
            )
            .expect("directory restoration should still be attempted")
            .expect("directory reference should remain contained");
        directory
            .abort()
            .expect("restored directory should remain reusable");
    }

    #[test]
    fn direct_batch_rejects_reserved_root_reference_without_path_fallback() {
        let error = contained_requested_root()
            .prepare(None, || false)
            .expect_err("direct mode must reject reserved grant grammar");
        assert_eq!(error.stage, SnapshotRestoreResourceStage::RootPreparation);
        assert!(matches!(
            error.kind,
            SnapshotRestoreResourceErrorKind::RootBacking(SnapshotRootBackingLeaseError::Grant(_))
        ));
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
    fn profile_2_derives_one_exact_request_and_config_per_record() {
        let graph = multi_fixture_graph(None, None);
        let request =
            RequestedSnapshotMultiDriveRestoreResources::try_from_native_v2_multi_block_device_graph(
                &graph,
            )
            .expect("profile-2 graph should produce an exact request");
        assert_eq!(request.drives.len(), graph.records().len());
        assert_eq!(request.drive_keys.len(), graph.records().len());
        assert_eq!(
            request.drive_configs.as_slice().len(),
            graph.records().len()
        );
        assert_eq!(request.bindings.manifest().len(), graph.records().len());
        for (((drive, key), config), record) in request
            .drives
            .iter()
            .zip(&request.drive_keys)
            .zip(request.drive_configs.as_slice())
            .zip(graph.records())
        {
            assert_eq!(drive.key, *key);
            assert_eq!(key.device_key(), record.key());
            assert_eq!(key.public_id().as_str(), record.config().drive_id());
            assert_eq!(
                key.resource_class(),
                SnapshotRestoreResourceClass::BlockBacking
            );
            assert_eq!(drive.selector, Path::new(record.config().selector()));
            assert_eq!(drive.is_read_only, record.config().is_read_only());
            assert_eq!(drive.expected_len, record.block().backing_bytes());
            assert_eq!(config.drive_id(), record.config().drive_id());
            assert_eq!(
                config.path_on_host(),
                Some(Path::new(record.config().selector()))
            );
        }
        let diagnostics = format!("{request:?}");
        for record in graph.records() {
            assert!(!diagnostics.contains(record.config().drive_id()));
            assert!(!diagnostics.contains(record.config().selector()));
        }
    }

    #[test]
    fn profile_3_direct_block_only_and_pmem_only_batches_preserve_typed_cardinality() {
        let block_path = unique_short_path();
        let block_graph = storage_fixture_graph_from_hex(
            include_str!("../../runtime/src/snapshot_device_v2_6/fixtures/block-root-mmio.hex"),
            &[(b"logical-selector-0".as_slice(), block_path.as_path())],
        );
        assert_eq!(
            block_graph.root_key(),
            Some(block_graph.block_records()[0].key())
        );
        let _block = TempRoot::new_sized_at(
            block_path,
            block_graph.block_records()[0].block().backing_bytes(),
        );
        let block_batch =
            RequestedSnapshotRestoreResources::prepare_native_v2_storage_device_graph(
                &block_graph,
                None,
                || false,
            )
            .expect("block-only exact-2.6 batch should prepare");
        let (blocks, pmems, completion) = block_batch.into_parts();
        assert_eq!(blocks.len(), 1);
        assert!(pmems.is_empty());
        drop(blocks);
        completion
            .abort()
            .expect("block-only direct completion should abort");

        let mut invalid_block_only =
            RequestedSnapshotStorageRestoreResources::try_from_native_v2_storage_device_graph(
                &block_graph,
            )
            .expect("block-only exact-2.6 request should validate");
        invalid_block_only.resources.drives[0].expected_len = 0;
        let error = invalid_block_only
            .prepare(None, || false)
            .expect_err("block-only exact-2.6 preflight should reject invalid geometry");
        assert_eq!(error.stage, SnapshotRestoreResourceStage::StoragePreflight);
        assert!(matches!(
            error.kind,
            SnapshotRestoreResourceErrorKind::InvalidStorageSet
        ));

        let pmem_path = unique_pmem_short_path();
        let pmem_graph = storage_fixture_graph_from_hex(
            include_str!("../../runtime/src/snapshot_device_v2_6/fixtures/pmem-root-mmio.hex"),
            &[(b"pmem-selector-0".as_slice(), pmem_path.as_path())],
        );
        assert_eq!(
            pmem_graph.root_key(),
            Some(pmem_graph.pmem_records()[0].key())
        );
        let _pmem =
            TempRoot::new_sized_at(pmem_path, pmem_graph.pmem_records()[0].pmem().file_bytes());
        let pmem_batch = RequestedSnapshotRestoreResources::prepare_native_v2_storage_device_graph(
            &pmem_graph,
            None,
            || false,
        )
        .expect("pmem-only exact-2.6 batch should prepare");
        let (blocks, pmems, completion) = pmem_batch.into_parts();
        assert!(blocks.is_empty());
        assert_eq!(pmems.len(), 1);
        drop(pmems);
        completion
            .abort()
            .expect("pmem-only direct completion should abort");
    }

    #[test]
    fn profile_3_direct_mixed_pmem_root_pci_batch_preserves_cross_class_root() {
        let block_path = unique_short_path();
        let pmem_path = unique_pmem_short_path();
        let graph = storage_fixture_graph_from_hex(
            include_str!("../../runtime/src/snapshot_device_v2_6/fixtures/mixed-pmem-root-pci.hex"),
            &[
                (b"logical-selector-0".as_slice(), block_path.as_path()),
                (b"pmem-selector-0".as_slice(), pmem_path.as_path()),
            ],
        );
        assert_eq!(graph.root_key(), Some(graph.pmem_records()[0].key()));
        let _block =
            TempRoot::new_sized_at(block_path, graph.block_records()[0].block().backing_bytes());
        let _pmem = TempRoot::new_sized_at(pmem_path, graph.pmem_records()[0].pmem().file_bytes());

        let batch = RequestedSnapshotRestoreResources::prepare_native_v2_storage_device_graph(
            &graph,
            None,
            || false,
        )
        .expect("mixed pmem-root PCI batch should prepare");
        let (blocks, pmems, completion) = batch.into_parts();
        assert_eq!(blocks.len(), 1);
        assert_eq!(pmems.len(), 1);
        drop(blocks);
        drop(pmems);
        completion
            .commit()
            .expect("mixed pmem-root direct completion should commit");
    }

    #[test]
    fn profile_3_direct_batch_returns_exact_keyed_unmapped_block_and_pmem_owners() {
        let block_path = unique_short_path();
        let pmem_path = unique_pmem_short_path();
        let graph = storage_fixture_graph(&block_path, &pmem_path);
        assert_eq!(graph.block_records().len(), 1);
        assert_eq!(graph.pmem_records().len(), 1);
        let _block =
            TempRoot::new_sized_at(block_path, graph.block_records()[0].block().backing_bytes());
        let _pmem = TempRoot::new_sized_at(pmem_path, graph.pmem_records()[0].pmem().file_bytes());

        let batch = RequestedSnapshotRestoreResources::prepare_native_v2_storage_device_graph(
            &graph,
            None,
            || false,
        )
        .expect("mixed direct storage batch should prepare");
        let diagnostic = format!("{batch:?}");
        assert!(!diagnostic.contains(graph.block_records()[0].config().selector()));
        assert!(!diagnostic.contains(graph.pmem_records()[0].config().selector()));
        assert!(!diagnostic.contains(graph.block_records()[0].config().drive_id()));
        assert!(!diagnostic.contains(graph.pmem_records()[0].config().pmem_id()));

        let (mut blocks, mut pmems, completion) = batch.into_parts();
        let (block_key, block) = blocks
            .pop()
            .expect("one block owner should exist")
            .into_parts();
        let (pmem_key, pmem) = pmems
            .pop()
            .expect("one pmem owner should exist")
            .into_parts();
        assert!(blocks.is_empty());
        assert!(pmems.is_empty());
        assert_eq!(
            block_key.resource_class(),
            SnapshotRestoreResourceClass::BlockBacking
        );
        assert_eq!(block_key.device_key(), graph.block_records()[0].key());
        assert_eq!(
            block_key.public_id().as_str(),
            graph.block_records()[0].config().drive_id()
        );
        assert_eq!(
            block.len(),
            graph.block_records()[0].block().backing_bytes()
        );
        assert_eq!(
            block.is_read_only(),
            graph.block_records()[0].config().is_read_only()
        );
        assert_eq!(
            pmem_key.resource_class(),
            SnapshotRestoreResourceClass::PmemBacking
        );
        assert_eq!(pmem_key.device_key(), graph.pmem_records()[0].key());
        assert_eq!(
            pmem_key.public_id().as_str(),
            graph.pmem_records()[0].config().pmem_id()
        );
        assert_eq!(pmem.len(), graph.pmem_records()[0].pmem().file_bytes());
        assert_eq!(
            aligned_pmem_mapping_len(pmem.len()),
            Some(graph.pmem_records()[0].pmem().mapped_bytes())
        );
        assert_eq!(
            pmem.is_read_only(),
            graph.pmem_records()[0].config().is_read_only()
        );
        completion
            .commit()
            .expect("direct storage completion should commit once");
    }

    #[test]
    fn profile_3_direct_preflight_rejects_cross_class_alias_before_conversion() {
        let block_path = unique_short_path();
        let pmem_path = unique_pmem_short_path();
        let graph = storage_fixture_graph(&block_path, &pmem_path);
        let mut requested =
            RequestedSnapshotStorageRestoreResources::try_from_native_v2_storage_device_graph(
                &graph,
            )
            .expect("mixed storage request should derive");
        let common_len = 4096;
        let _block = TempRoot::new_sized_at(block_path.clone(), common_len);
        fs::hard_link(&block_path, &pmem_path).expect("cross-class hard link should create");
        let _pmem = TempRoot {
            path: pmem_path.clone(),
        };
        requested.resources.drives[0].selector = block_path;
        requested.resources.drives[0].expected_len = common_len;
        requested.resources.drives[1].selector = pmem_path;
        requested.resources.drives[1].expected_len = common_len;
        requested.resources.drives[1].kind = SnapshotRestoreFileKind::Pmem {
            expected_mapped_len: aligned_pmem_mapping_len(common_len)
                .expect("test pmem length should align"),
        };

        let error = requested
            .prepare(None, || false)
            .expect_err("cross-class descriptor alias must reject");
        assert_eq!(error.stage, SnapshotRestoreResourceStage::StoragePreflight);
        assert!(matches!(
            error.kind,
            SnapshotRestoreResourceErrorKind::InvalidStorageSet
        ));
        assert_eq!(
            error.disposition,
            SnapshotRestoreResourceDisposition::Retryable
        );
    }

    #[test]
    fn profile_3_direct_mode_rejects_pmem_grant_reference_without_path_fallback() {
        let block_path = unique_short_path();
        let pmem_path = unique_pmem_short_path();
        let graph = storage_fixture_graph(&block_path, &pmem_path);
        let mut requested =
            RequestedSnapshotStorageRestoreResources::try_from_native_v2_storage_device_graph(
                &graph,
            )
            .expect("mixed storage request should derive");
        let _block =
            TempRoot::new_sized_at(block_path, graph.block_records()[0].block().backing_bytes());
        requested.resources.drives[1].selector = PathBuf::from("bangbang-grant:pmem-ro");
        let error = requested
            .prepare(None, || false)
            .expect_err("direct mode must reject contained pmem reference");
        assert_eq!(error.stage, SnapshotRestoreResourceStage::StoragePreflight);
        assert!(matches!(
            error.kind,
            SnapshotRestoreResourceErrorKind::InvalidStorageSet
        ));
    }

    #[test]
    fn profile_3_direct_rejects_pmem_geometry_changes_and_cancellation_before_batch() {
        let block_path = unique_short_path();
        let pmem_path = unique_pmem_short_path();
        let graph = storage_fixture_graph(&block_path, &pmem_path);
        let _block =
            TempRoot::new_sized_at(block_path, graph.block_records()[0].block().backing_bytes());
        let pmem = TempRoot::new_sized_at(pmem_path, graph.pmem_records()[0].pmem().file_bytes());
        let expected_pmem_len = graph.pmem_records()[0].pmem().file_bytes();

        let mut wrong_alignment =
            RequestedSnapshotStorageRestoreResources::try_from_native_v2_storage_device_graph(
                &graph,
            )
            .expect("mixed storage request should derive");
        wrong_alignment.resources.drives[1].kind = SnapshotRestoreFileKind::Pmem {
            expected_mapped_len: graph.pmem_records()[0]
                .pmem()
                .mapped_bytes()
                .saturating_add(1),
        };
        let alignment_error = wrong_alignment
            .prepare(None, || false)
            .expect_err("wrong aligned pmem geometry must reject before opening a batch");
        assert_eq!(
            alignment_error.stage,
            SnapshotRestoreResourceStage::StoragePreflight
        );
        assert_eq!(
            alignment_error.disposition,
            SnapshotRestoreResourceDisposition::Terminal
        );

        OpenOptions::new()
            .write(true)
            .open(pmem.path())
            .expect("pmem geometry fixture should reopen")
            .set_len(expected_pmem_len.saturating_add(1))
            .expect("pmem geometry fixture should grow");
        let file_error =
            RequestedSnapshotStorageRestoreResources::try_from_native_v2_storage_device_graph(
                &graph,
            )
            .expect("mixed storage request should derive")
            .prepare(None, || false)
            .expect_err("wrong pmem file length must reject the complete reservation");
        assert_eq!(
            file_error.stage,
            SnapshotRestoreResourceStage::StoragePreflight
        );
        assert_eq!(
            file_error.disposition,
            SnapshotRestoreResourceDisposition::Retryable
        );
        OpenOptions::new()
            .write(true)
            .open(pmem.path())
            .expect("pmem geometry fixture should reopen")
            .set_len(expected_pmem_len)
            .expect("pmem geometry fixture should reset");

        let conversion_calls = Cell::new(0_u8);
        let conversion_error =
            RequestedSnapshotStorageRestoreResources::try_from_native_v2_storage_device_graph(
                &graph,
            )
            .expect("mixed storage request should derive")
            .prepare(None, || {
                let next = conversion_calls.get().saturating_add(1);
                conversion_calls.set(next);
                if next == 8 {
                    OpenOptions::new()
                        .write(true)
                        .open(pmem.path())
                        .expect("reserved pmem fixture should reopen")
                        .set_len(expected_pmem_len.saturating_add(1))
                        .expect("reserved pmem fixture should change before adoption");
                }
                false
            })
            .expect_err("changed pmem reservation must reject before binding");
        assert_eq!(
            conversion_error.stage,
            SnapshotRestoreResourceStage::PmemPreparation
        );
        assert_eq!(
            conversion_error.disposition,
            SnapshotRestoreResourceDisposition::Retryable
        );
        OpenOptions::new()
            .write(true)
            .open(pmem.path())
            .expect("changed pmem fixture should reopen")
            .set_len(expected_pmem_len)
            .expect("changed pmem fixture should reset");

        let cancellation_calls = Cell::new(0_u8);
        let cancellation_error =
            RequestedSnapshotStorageRestoreResources::try_from_native_v2_storage_device_graph(
                &graph,
            )
            .expect("mixed storage request should derive")
            .prepare(None, || {
                let next = cancellation_calls.get().saturating_add(1);
                cancellation_calls.set(next);
                next >= 5
            })
            .expect_err("mid-reservation cancellation must unwind the earlier block");
        assert_eq!(
            cancellation_error.stage,
            SnapshotRestoreResourceStage::Cancellation
        );
        assert_eq!(
            cancellation_error.disposition,
            SnapshotRestoreResourceDisposition::Retryable
        );

        let diagnostics = format!(
            "{alignment_error:?} {file_error:?} {conversion_error:?} {cancellation_error:?}"
        );
        assert!(!diagnostics.contains(pmem.path().to_string_lossy().as_ref()));
        let retry = RequestedSnapshotRestoreResources::prepare_native_v2_storage_device_graph(
            &graph,
            None,
            || false,
        )
        .expect("all direct failure paths should leave the mixed files reusable");
        let (blocks, pmems, completion) = retry.into_parts();
        drop(blocks);
        drop(pmems);
        completion
            .abort()
            .expect("retried direct completion should abort");
    }

    #[test]
    fn profile_3_contained_batch_returns_mixed_owners_under_one_rollback_transaction() {
        let block_path = unique_short_path();
        let pmem_path = unique_pmem_short_path();
        let graph = storage_fixture_graph(&block_path, &pmem_path);
        let block =
            TempRoot::new_sized_at(block_path, graph.block_records()[0].block().backing_bytes());
        let pmem = TempRoot::new_sized_at(pmem_path, graph.pmem_records()[0].pmem().file_bytes());
        let block_reference = PathBuf::from("bangbang-grant:block0");
        let pmem_reference = PathBuf::from("bangbang-grant:pmem0");
        let block_access = grant_access(graph.block_records()[0].config().is_read_only());
        let pmem_access = grant_access(graph.pmem_records()[0].config().is_read_only());
        let fixture = contained_restore_authority_with_grants_for_test(
            snapshot_storage_grant_authority_for_test(&[
                (
                    "block0",
                    ResourceRole::DriveBacking,
                    block_access,
                    block.path(),
                ),
                ("pmem0", ResourceRole::PmemBacking, pmem_access, pmem.path()),
            ]),
            false,
        );

        let batch = contained_storage_request(&graph, &block_reference, &pmem_reference)
            .prepare(Some(fixture.authority()), || false)
            .expect("mixed contained storage resources should prepare")
            .into_storage_batch()
            .expect("mixed contained storage batch should take exact owners");
        let diagnostics = format!("{batch:?}");
        assert!(!diagnostics.contains("block0"));
        assert!(!diagnostics.contains("pmem0"));
        assert!(!diagnostics.contains(block.path().to_string_lossy().as_ref()));
        assert!(!diagnostics.contains(pmem.path().to_string_lossy().as_ref()));

        let (mut blocks, mut pmems, completion) = batch.into_parts();
        let (block_key, block_backing) = blocks
            .pop()
            .expect("one contained block owner should exist")
            .into_parts();
        let (pmem_key, pmem_backing) = pmems
            .pop()
            .expect("one contained pmem owner should exist")
            .into_parts();
        assert!(blocks.is_empty());
        assert!(pmems.is_empty());
        assert_eq!(
            block_key.resource_class(),
            SnapshotRestoreResourceClass::BlockBacking
        );
        assert_eq!(
            pmem_key.resource_class(),
            SnapshotRestoreResourceClass::PmemBacking
        );
        assert_eq!(
            block_backing.len(),
            graph.block_records()[0].block().backing_bytes()
        );
        assert_eq!(
            pmem_backing.len(),
            graph.pmem_records()[0].pmem().file_bytes()
        );
        drop(block_backing);
        drop(pmem_backing);
        completion
            .abort()
            .expect("mixed contained completion should restore both grants and generation");

        let rebound = contained_storage_request(&graph, &block_reference, &pmem_reference)
            .prepare(Some(fixture.authority()), || false)
            .expect("one abort should restore the complete mixed transaction")
            .into_storage_batch()
            .expect("restored mixed contained batch should take exact owners");
        let (blocks, pmems, completion) = rebound.into_parts();
        assert_eq!(blocks.len(), 1);
        assert_eq!(pmems.len(), 1);
        drop(blocks);
        drop(pmems);
        completion
            .abort()
            .expect("rebound mixed contained completion should restore");
    }

    #[test]
    fn profile_3_contained_mode_rejects_wrong_pmem_role_atomically_without_path_fallback() {
        let block_path = unique_short_path();
        let pmem_path = unique_pmem_short_path();
        let graph = storage_fixture_graph(&block_path, &pmem_path);
        let block =
            TempRoot::new_sized_at(block_path, graph.block_records()[0].block().backing_bytes());
        let pmem = TempRoot::new_sized_at(pmem_path, graph.pmem_records()[0].pmem().file_bytes());
        let block_reference = PathBuf::from("bangbang-grant:block0");
        let pmem_reference = PathBuf::from("bangbang-grant:pmem0");
        let block_access = grant_access(graph.block_records()[0].config().is_read_only());
        let pmem_access = grant_access(graph.pmem_records()[0].config().is_read_only());
        let fixture = contained_restore_authority_with_grants_for_test(
            snapshot_storage_grant_authority_for_test(&[
                (
                    "block0",
                    ResourceRole::DriveBacking,
                    block_access,
                    block.path(),
                ),
                (
                    "pmem0",
                    ResourceRole::DriveBacking,
                    pmem_access,
                    pmem.path(),
                ),
            ]),
            false,
        );

        let error = contained_storage_request(&graph, &block_reference, &pmem_reference)
            .prepare(Some(fixture.authority()), || false)
            .expect_err("wrong-class pmem grant must reject the complete contained batch");
        assert_eq!(
            error.stage,
            SnapshotRestoreResourceStage::ContainedReservation
        );
        assert!(matches!(
            error.kind,
            SnapshotRestoreResourceErrorKind::Contained(_)
        ));
        assert_eq!(
            error.disposition,
            SnapshotRestoreResourceDisposition::Retryable
        );
        let diagnostics = format!("{error:?}");
        assert!(!diagnostics.contains("block0"));
        assert!(!diagnostics.contains("pmem0"));
        assert!(!diagnostics.contains(block.path().to_string_lossy().as_ref()));
        assert!(!diagnostics.contains(pmem.path().to_string_lossy().as_ref()));

        let block_claim = fixture
            .grants()
            .prepare_drive_backing_claim(&block_reference, block_access)
            .expect("wrong-role failure should preserve the block grant")
            .expect("block reference should remain contained");
        block_claim
            .abort()
            .expect("preserved block grant should restore after inspection");
        let pmem_claim = fixture
            .grants()
            .prepare_drive_backing_claim(&pmem_reference, pmem_access)
            .expect("wrong-role failure should preserve the mislabeled pmem grant")
            .expect("mislabeled pmem reference should remain contained");
        pmem_claim
            .abort()
            .expect("preserved mislabeled pmem grant should restore after inspection");
    }

    #[test]
    fn profile_3_contained_preflight_rejects_access_length_selector_and_cancellation() {
        let block_path = unique_short_path();
        let pmem_path = unique_pmem_short_path();
        let graph = storage_fixture_graph(&block_path, &pmem_path);
        let block =
            TempRoot::new_sized_at(block_path, graph.block_records()[0].block().backing_bytes());
        let pmem = TempRoot::new_sized_at(pmem_path, graph.pmem_records()[0].pmem().file_bytes());
        let block_reference = PathBuf::from("bangbang-grant:block0");
        let pmem_reference = PathBuf::from("bangbang-grant:pmem0");
        let block_access = grant_access(graph.block_records()[0].config().is_read_only());
        let pmem_access = grant_access(graph.pmem_records()[0].config().is_read_only());
        let wrong_pmem_access = match pmem_access {
            GrantAccess::ReadOnly => GrantAccess::ReadWrite,
            GrantAccess::ReadWrite => GrantAccess::ReadOnly,
            _ => panic!("pmem fixture access should be read-only or read-write"),
        };

        let wrong_access = contained_restore_authority_with_grants_for_test(
            snapshot_storage_grant_authority_for_test(&[
                (
                    "block0",
                    ResourceRole::DriveBacking,
                    block_access,
                    block.path(),
                ),
                (
                    "pmem0",
                    ResourceRole::PmemBacking,
                    wrong_pmem_access,
                    pmem.path(),
                ),
            ]),
            false,
        );
        let access_error = contained_storage_request(&graph, &block_reference, &pmem_reference)
            .prepare(Some(wrong_access.authority()), || false)
            .expect_err("wrong pmem access must reject the complete contained vector");
        assert_eq!(
            access_error.stage,
            SnapshotRestoreResourceStage::ContainedReservation
        );
        assert_eq!(
            access_error.disposition,
            SnapshotRestoreResourceDisposition::Retryable
        );
        assert_storage_grants_reusable(
            &wrong_access,
            &block_reference,
            block_access,
            &pmem_reference,
            wrong_pmem_access,
        );

        let exact = contained_restore_authority_with_grants_for_test(
            snapshot_storage_grant_authority_for_test(&[
                (
                    "block0",
                    ResourceRole::DriveBacking,
                    block_access,
                    block.path(),
                ),
                ("pmem0", ResourceRole::PmemBacking, pmem_access, pmem.path()),
            ]),
            false,
        );
        let mut wrong_length = contained_storage_request(&graph, &block_reference, &pmem_reference);
        wrong_length.resources.drives[1].expected_len = wrong_length.resources.drives[1]
            .expected_len
            .saturating_add(1);
        let length_error = wrong_length
            .prepare(Some(exact.authority()), || false)
            .expect_err("wrong pmem length must reject before exact take");
        assert_eq!(
            length_error.stage,
            SnapshotRestoreResourceStage::ContainedReservation
        );
        assert_storage_grants_reusable(
            &exact,
            &block_reference,
            block_access,
            &pmem_reference,
            pmem_access,
        );

        let mut invalid_selector =
            contained_storage_request(&graph, &block_reference, &pmem_reference);
        invalid_selector.resources.drives[1].selector = pmem.path().to_path_buf();
        let selector_error = invalid_selector
            .prepare(Some(exact.authority()), || false)
            .expect_err("contained pmem selector must never fall back to a direct path");
        assert_eq!(
            selector_error.stage,
            SnapshotRestoreResourceStage::ContainedReservation
        );
        assert_storage_grants_reusable(
            &exact,
            &block_reference,
            block_access,
            &pmem_reference,
            pmem_access,
        );

        let cancellation_calls = Cell::new(0_u8);
        let cancellation_error =
            contained_storage_request(&graph, &block_reference, &pmem_reference)
                .prepare(Some(exact.authority()), || {
                    let next = cancellation_calls.get().saturating_add(1);
                    cancellation_calls.set(next);
                    next >= 4
                })
                .expect_err("post-inspection cancellation must release the mixed generation");
        assert_eq!(
            cancellation_error.stage,
            SnapshotRestoreResourceStage::ContainedReservation
        );
        assert_eq!(
            cancellation_error.disposition,
            SnapshotRestoreResourceDisposition::Retryable
        );
        assert_storage_grants_reusable(
            &exact,
            &block_reference,
            block_access,
            &pmem_reference,
            pmem_access,
        );

        let diagnostics = format!(
            "{access_error:?} {length_error:?} {selector_error:?} \
             {cancellation_error:?}"
        );
        for private in [
            block.path(),
            pmem.path(),
            block_reference.as_path(),
            pmem_reference.as_path(),
        ] {
            assert!(!diagnostics.contains(private.to_string_lossy().as_ref()));
        }
    }

    #[test]
    fn profile_3_storage_batch_rejects_omitted_and_swapped_typed_outputs_with_full_rollback() {
        let block_path = unique_short_path();
        let pmem_path = unique_pmem_short_path();
        let graph = storage_fixture_graph(&block_path, &pmem_path);
        let block =
            TempRoot::new_sized_at(block_path, graph.block_records()[0].block().backing_bytes());
        let pmem = TempRoot::new_sized_at(pmem_path, graph.pmem_records()[0].pmem().file_bytes());
        let block_reference = PathBuf::from("bangbang-grant:block0");
        let pmem_reference = PathBuf::from("bangbang-grant:pmem0");
        let block_access = grant_access(graph.block_records()[0].config().is_read_only());
        let pmem_access = grant_access(graph.pmem_records()[0].config().is_read_only());
        let fixture = contained_restore_authority_with_grants_for_test(
            snapshot_storage_grant_authority_for_test(&[
                (
                    "block0",
                    ResourceRole::DriveBacking,
                    block_access,
                    block.path(),
                ),
                ("pmem0", ResourceRole::PmemBacking, pmem_access, pmem.path()),
            ]),
            false,
        );

        let mut omitted = contained_storage_request(&graph, &block_reference, &pmem_reference)
            .prepare(Some(fixture.authority()), || false)
            .expect("omitted-output fixture should prepare");
        omitted.pmem_count = 0;
        let omitted_error = omitted
            .into_storage_batch()
            .expect_err("omitted typed output count must reject before consumption");
        assert_eq!(omitted_error.stage, SnapshotRestoreResourceStage::Take);
        assert_eq!(
            omitted_error.disposition,
            SnapshotRestoreResourceDisposition::Terminal
        );
        assert_storage_grants_reusable(
            &fixture,
            &block_reference,
            block_access,
            &pmem_reference,
            pmem_access,
        );

        let mut swapped = contained_storage_request(&graph, &block_reference, &pmem_reference)
            .prepare(Some(fixture.authority()), || false)
            .expect("swapped-output fixture should prepare");
        swapped.resources.drive_keys.swap(0, 1);
        let swapped_error = swapped
            .into_storage_batch()
            .expect_err("swapped typed outputs must reject before returning a batch");
        assert_eq!(swapped_error.stage, SnapshotRestoreResourceStage::Finish);
        assert_eq!(
            swapped_error.disposition,
            SnapshotRestoreResourceDisposition::Terminal
        );
        assert_storage_grants_reusable(
            &fixture,
            &block_reference,
            block_access,
            &pmem_reference,
            pmem_access,
        );

        let diagnostics = format!("{omitted_error:?} {swapped_error:?}");
        for private in [
            block.path(),
            pmem.path(),
            block_reference.as_path(),
            pmem_reference.as_path(),
        ] {
            assert!(!diagnostics.contains(private.to_string_lossy().as_ref()));
        }
    }

    #[test]
    fn profile_3_contained_abort_faults_are_terminal_and_release_all_owned_descriptors() {
        let block_path = unique_short_path();
        let pmem_path = unique_pmem_short_path();
        let graph = storage_fixture_graph(&block_path, &pmem_path);
        let block =
            TempRoot::new_sized_at(block_path, graph.block_records()[0].block().backing_bytes());
        let pmem = TempRoot::new_sized_at(pmem_path, graph.pmem_records()[0].pmem().file_bytes());
        let block_reference = PathBuf::from("bangbang-grant:block0");
        let pmem_reference = PathBuf::from("bangbang-grant:pmem0");
        let block_access = grant_access(graph.block_records()[0].config().is_read_only());
        let pmem_access = grant_access(graph.pmem_records()[0].config().is_read_only());
        let grants = || {
            snapshot_storage_grant_authority_for_test(&[
                (
                    "block0",
                    ResourceRole::DriveBacking,
                    block_access,
                    block.path(),
                ),
                ("pmem0", ResourceRole::PmemBacking, pmem_access, pmem.path()),
            ])
        };

        let stale_generation = contained_restore_authority_with_grants_for_test(grants(), false);
        let batch = contained_storage_request(&graph, &block_reference, &pmem_reference)
            .prepare(Some(stale_generation.authority()), || false)
            .expect("stale-generation fixture should prepare")
            .into_storage_batch()
            .expect("stale-generation fixture should return one batch");
        let (blocks, pmems, completion) = batch.into_parts();
        drop(blocks);
        drop(pmems);
        stale_generation.invalidate_generation();
        let generation_error = completion
            .abort()
            .expect_err("stale generation must report terminal completion uncertainty");
        assert_eq!(
            generation_error.disposition(),
            SnapshotRestoreResourceDisposition::Terminal
        );
        assert!(!generation_error.grant_failed);
        assert_storage_grants_reusable(
            &stale_generation,
            &block_reference,
            block_access,
            &pmem_reference,
            pmem_access,
        );

        let inactive_grants = contained_restore_authority_with_grants_for_test(grants(), false);
        let batch = contained_storage_request(&graph, &block_reference, &pmem_reference)
            .prepare(Some(inactive_grants.authority()), || false)
            .expect("inactive-grant fixture should prepare")
            .into_storage_batch()
            .expect("inactive-grant fixture should return one batch");
        let (blocks, pmems, completion) = batch.into_parts();
        drop(blocks);
        drop(pmems);
        inactive_grants.grants().invalidate_for_test();
        let grant_error = completion
            .abort()
            .expect_err("inactive grant registry must report terminal cleanup uncertainty");
        assert_eq!(
            grant_error.disposition(),
            SnapshotRestoreResourceDisposition::Terminal
        );
        assert!(grant_error.grant_failed);
        let diagnostics = format!("{generation_error:?} {grant_error:?}");
        for private in [
            block.path(),
            pmem.path(),
            block_reference.as_path(),
            pmem_reference.as_path(),
        ] {
            assert!(!diagnostics.contains(private.to_string_lossy().as_ref()));
        }
    }

    #[test]
    fn profile_3_process_bundle_retains_runtime_owners_until_explicit_commit() {
        let block_path = unique_short_path();
        let pmem_path = unique_pmem_short_path();
        let template = storage_fixture_graph(&block_path, &pmem_path);
        let block = TempRoot::new_sized_at(
            block_path,
            template.block_records()[0].block().backing_bytes(),
        );
        let pmem =
            TempRoot::new_sized_at(pmem_path, template.pmem_records()[0].pmem().file_bytes());
        let graph = storage_fixture_graph(block.path(), pmem.path());
        let expected_root = graph.root_key();
        let memory = storage_restore_memory(&graph);
        let prepared = RequestedSnapshotRestoreResources::prepare_native_v2_storage_restore_bundle(
            graph,
            &memory,
            Instant::now(),
            None,
            || false,
        )
        .expect("profile-3 process bundle should prepare");
        let retained = prepared
            .bundle()
            .expect("profile-3 process owner should retain its bundle");
        assert_eq!(retained.root_key(), expected_root);
        assert_eq!(
            retained
                .block_bundle()
                .expect("mixed storage should retain block owners")
                .records()
                .len(),
            1
        );
        assert_eq!(retained.pmem_records().len(), 1);
        assert_eq!(retained.pmem_configs().as_slice().len(), 1);
        assert_eq!(
            retained.pmem_records()[0].prepared_device().guest_range(),
            template.pmem_records()[0].pmem().guest_range()
        );
        let diagnostics = format!("{prepared:?}");
        assert!(!diagnostics.contains(block.path().to_string_lossy().as_ref()));
        assert!(!diagnostics.contains(pmem.path().to_string_lossy().as_ref()));

        let destination = prepared
            .construct_destination(|bundle| Ok::<_, std::convert::Infallible>(Box::new(bundle)))
            .expect("private profile-3 destination should construct");
        let (bundle, ()) = destination
            .commit(
                |bundle| {
                    Ok::<
                        _,
                        (
                            Box<PreparedSnapshotV2StorageBundle>,
                            std::convert::Infallible,
                        ),
                    >((bundle, ()))
                },
                |_| Ok::<_, std::convert::Infallible>(()),
            )
            .expect("profile-3 completion should commit after controller preparation");
        (*bundle)
            .abort()
            .expect("committed detached profile-3 owners should abort cleanly");
    }

    #[test]
    fn profile_3_destination_failures_and_drop_preserve_clean_retry() {
        let block_path = unique_short_path();
        let pmem_path = unique_pmem_short_path();
        let template = storage_fixture_graph(&block_path, &pmem_path);
        let block = TempRoot::new_sized_at(
            block_path,
            template.block_records()[0].block().backing_bytes(),
        );
        let pmem =
            TempRoot::new_sized_at(pmem_path, template.pmem_records()[0].pmem().file_bytes());
        let prepare = || {
            let graph = storage_fixture_graph(block.path(), pmem.path());
            let memory = storage_restore_memory(&graph);
            RequestedSnapshotRestoreResources::prepare_native_v2_storage_restore_bundle(
                graph,
                &memory,
                Instant::now(),
                None,
                || false,
            )
            .expect("profile-3 failure fixture should prepare")
        };

        let construction = prepare()
            .construct_destination(|bundle| {
                drop(bundle);
                Err::<(), _>(io::Error::other(
                    "scripted destination construction failure",
                ))
            })
            .expect_err("destination construction failure should abort completion");
        assert!(!construction.is_terminal());

        let destination = prepare()
            .construct_destination(|bundle| Ok::<_, std::convert::Infallible>(Box::new(bundle)))
            .expect("controller failure destination should construct");
        let controller = destination
            .commit(
                |destination| {
                    Err::<(Box<PreparedSnapshotV2StorageBundle>, ()), _>((
                        destination,
                        io::Error::other("scripted controller preparation failure"),
                    ))
                },
                |destination| (*destination).abort(),
            )
            .expect_err("controller failure should destroy the destination and abort completion");
        assert!(!controller.is_terminal());

        drop(prepare());
        prepare()
            .abort()
            .expect("construction, controller, and drop failures should leave a clean retry");
        let diagnostics = format!("{construction:?} {controller:?}");
        assert!(!diagnostics.contains(block.path().to_string_lossy().as_ref()));
        assert!(!diagnostics.contains(pmem.path().to_string_lossy().as_ref()));
    }

    #[test]
    fn profile_3_plan_failure_precedes_backing_access_and_bundle_failure_aborts_completion() {
        let missing_block = unique_short_path();
        let missing_pmem = unique_pmem_short_path();
        let graph = storage_fixture_graph(&missing_block, &missing_pmem);
        let tiny_layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), 0x4000)
                .expect("tiny restore range should validate"),
        ])
        .expect("tiny restore layout should validate");
        let tiny_memory =
            GuestMemory::allocate(&tiny_layout).expect("tiny restore memory should allocate");
        let error = RequestedSnapshotRestoreResources::prepare_native_v2_storage_restore_bundle(
            graph,
            &tiny_memory,
            Instant::now(),
            None,
            || false,
        )
        .expect_err("loaded-memory failure must precede missing backing access");
        assert!(matches!(
            error,
            SnapshotV2StorageRestoreBundleError::Plan(_)
        ));
        assert_eq!(
            error.disposition(),
            SnapshotRestoreResourceDisposition::Terminal
        );

        let block_path = unique_short_path();
        let pmem_path = unique_pmem_short_path();
        let template = storage_fixture_graph(&block_path, &pmem_path);
        let block = TempRoot::new_sized_at(
            block_path,
            template.block_records()[0].block().backing_bytes(),
        );
        let pmem =
            TempRoot::new_sized_at(pmem_path, template.pmem_records()[0].pmem().file_bytes());
        let graph = storage_fixture_graph(block.path(), pmem.path());
        let memory = storage_restore_memory(&graph);
        let error =
            RequestedSnapshotRestoreResources::prepare_native_v2_storage_restore_bundle_with(
                graph,
                &memory,
                Instant::now(),
                None,
                || false,
                |plan, blocks, pmems, _| {
                    drop(blocks);
                    plan.prepare_backings(Vec::new(), pmems, || false)
                },
            )
            .expect_err("wrong runtime cardinality should abort aggregate completion");
        assert!(matches!(
            error,
            SnapshotV2StorageRestoreBundleError::Bundle { .. }
        ));
        assert_eq!(
            error.disposition(),
            SnapshotRestoreResourceDisposition::Terminal
        );

        let retry_graph = storage_fixture_graph(block.path(), pmem.path());
        let retry_memory = storage_restore_memory(&retry_graph);
        RequestedSnapshotRestoreResources::prepare_native_v2_storage_restore_bundle(
            retry_graph,
            &retry_memory,
            Instant::now(),
            None,
            || false,
        )
        .expect("bundle failure should leave direct backings reusable")
        .abort()
        .expect("retried profile-3 bundle should abort cleanly");
    }

    #[test]
    fn profile_3_runtime_cancellation_and_contained_abort_release_all_authority() {
        let block_path = unique_short_path();
        let pmem_path = unique_pmem_short_path();
        let template = storage_fixture_graph(&block_path, &pmem_path);
        let block = TempRoot::new_sized_at(
            block_path,
            template.block_records()[0].block().backing_bytes(),
        );
        let pmem =
            TempRoot::new_sized_at(pmem_path, template.pmem_records()[0].pmem().file_bytes());
        let graph = storage_fixture_graph(block.path(), pmem.path());
        let memory = storage_restore_memory(&graph);
        let cancellation =
            RequestedSnapshotRestoreResources::prepare_native_v2_storage_restore_bundle_with(
                graph,
                &memory,
                Instant::now(),
                None,
                || false,
                |plan, blocks, pmems, _| plan.prepare_backings(blocks, pmems, || true),
            )
            .expect_err("runtime cancellation should unwind the complete direct batch");
        assert!(matches!(
            cancellation,
            SnapshotV2StorageRestoreBundleError::Bundle { .. }
        ));
        assert_eq!(
            cancellation.disposition(),
            SnapshotRestoreResourceDisposition::Retryable
        );

        let block_reference = PathBuf::from("bangbang-grant:blk");
        let pmem_reference = PathBuf::from("bangbang-grant:pm");
        let base = storage_fixture_graph(&block_reference, pmem.path());
        let captured_pmem = &base.pmem_records()[0];
        let pmem_config = bangbang_runtime::snapshot_device_v2_6::SnapshotV2PmemConfig::try_new(
            captured_pmem.config().pmem_id(),
            captured_pmem.config().is_root(),
            captured_pmem.config().is_read_only(),
            captured_pmem.config().rate_limiter(),
            pmem_reference.to_string_lossy(),
        )
        .expect("contained pmem configuration should validate");
        let pmem_record =
            bangbang_runtime::snapshot_device_v2_6::SnapshotV2PmemDeviceRecord::try_new(
                captured_pmem.key().instance(),
                pmem_config,
                captured_pmem.pmem().clone(),
                captured_pmem.virtio().clone(),
                captured_pmem.transport().clone(),
            )
            .expect("contained pmem record should validate");
        let graph = SnapshotV2StorageDeviceGraph::try_from_parts(
            base.root_key(),
            base.transport_kind(),
            base.block_records().to_vec(),
            vec![pmem_record],
        )
        .expect("contained storage graph should validate");
        let block_access = grant_access(graph.block_records()[0].config().is_read_only());
        let pmem_access = grant_access(graph.pmem_records()[0].config().is_read_only());
        let fixture = contained_restore_authority_with_grants_for_test(
            snapshot_storage_grant_authority_for_test(&[
                (
                    "blk",
                    ResourceRole::DriveBacking,
                    block_access,
                    block.path(),
                ),
                ("pm", ResourceRole::PmemBacking, pmem_access, pmem.path()),
            ]),
            false,
        );
        let memory = storage_restore_memory(&graph);
        let prepared = RequestedSnapshotRestoreResources::prepare_native_v2_storage_restore_bundle(
            graph,
            &memory,
            Instant::now(),
            Some(fixture.authority()),
            || false,
        )
        .expect("contained profile-3 process bundle should prepare");
        prepared
            .abort()
            .expect("contained profile-3 abort should release runtime then authority");
        assert_storage_grants_reusable(
            &fixture,
            &block_reference,
            block_access,
            &pmem_reference,
            pmem_access,
        );
        let diagnostics = format!("{cancellation:?}");
        for private in [block.path(), pmem.path()] {
            assert!(!diagnostics.contains(private.to_string_lossy().as_ref()));
        }
    }

    #[test]
    fn profile_2_direct_batch_preserves_mixed_access_order_and_completion() {
        let template = multi_fixture_graph(None, None);
        let first_path = unique_short_path();
        let second_path = unique_short_path();
        let first = TempRoot::new_at(
            first_path.clone(),
            &vec![
                0x11;
                usize::try_from(template.records()[0].block().backing_bytes())
                    .expect("fixture length should fit")
            ],
        );
        let second = TempRoot::new_at(
            second_path.clone(),
            &vec![
                0x22;
                usize::try_from(template.records()[1].block().backing_bytes())
                    .expect("fixture length should fit")
            ],
        );
        let graph = multi_fixture_graph(Some(first.path()), Some(second.path()));
        let batch = RequestedSnapshotRestoreResources::prepare_native_v2_multi_block_device_graph(
            &graph,
            None,
            || false,
        )
        .expect("mixed direct batch should prepare");
        let (configs, backings, completion) = batch.into_parts();
        assert_eq!(configs.as_slice().len(), 2);
        assert_eq!(backings.len(), 2);
        for (((config, backing), record), expected_path) in configs
            .as_slice()
            .iter()
            .zip(&backings)
            .zip(graph.records())
            .zip([first.path(), second.path()])
        {
            assert_eq!(config.drive_id(), record.config().drive_id());
            assert_eq!(config.path_on_host(), Some(expected_path));
            assert_eq!(config.is_root_device(), record.is_root());
            assert_eq!(config.is_read_only(), Some(record.config().is_read_only()));
            assert_eq!(backing.is_read_only(), record.config().is_read_only());
            assert_eq!(backing.len(), record.block().backing_bytes());
        }
        assert!(configs.as_slice()[0].is_root_device());
        assert!(!configs.as_slice()[1].is_root_device());
        completion
            .commit()
            .expect("direct batch completion should commit once");
    }

    #[test]
    fn profile_2_process_bundle_retains_completion_until_explicit_commit() {
        let template = multi_fixture_graph(None, None);
        let first_path = unique_short_path();
        let second_path = unique_short_path();
        let first = TempRoot::new_at(
            first_path,
            &vec![
                0x31;
                usize::try_from(template.records()[0].block().backing_bytes())
                    .expect("fixture length should fit")
            ],
        );
        let second = TempRoot::new_at(
            second_path,
            &vec![
                0x32;
                usize::try_from(template.records()[1].block().backing_bytes())
                    .expect("fixture length should fit")
            ],
        );
        let graph = multi_fixture_graph(Some(first.path()), Some(second.path()));
        let expected_configs = graph
            .project_drive_configs()
            .expect("profile-2 configs should project");
        let memory = multi_restore_memory(&graph);
        let prepared =
            RequestedSnapshotRestoreResources::prepare_native_v2_multi_block_restore_bundle(
                graph,
                &memory,
                Instant::now(),
                None,
                || false,
            )
            .expect("profile-2 process bundle should prepare");
        let retained = prepared
            .bundle()
            .expect("prepared process owner should retain its bundle");
        assert_eq!(retained.drive_configs(), &expected_configs);
        assert_eq!(retained.records().len(), 2);
        assert_eq!(retained.retry_projection().len(), 2);
        assert_eq!(
            retained
                .async_runtime()
                .expect("mixed profile should own one runtime")
                .generation_count()
                .expect("runtime should lock"),
            1
        );
        let diagnostics = format!("{prepared:?}");
        for private in [
            first.path().to_string_lossy(),
            second.path().to_string_lossy(),
        ] {
            assert!(!diagnostics.contains(private.as_ref()));
        }

        let destination = prepared
            .construct_destination(Ok::<_, std::convert::Infallible>)
            .expect("private destination should construct before completion");
        let (bundle, ()) = destination
            .commit(
                |bundle| {
                    Ok::<_, (PreparedSnapshotV2MultiBlockBundle, std::convert::Infallible)>((
                        bundle,
                        (),
                    ))
                },
                PreparedSnapshotV2MultiBlockBundle::abort,
            )
            .expect("direct aggregate completion should commit");
        assert_eq!(bundle.drive_configs(), &expected_configs);
        bundle
            .abort()
            .expect("fresh Async generation should release cleanly");
    }

    #[test]
    fn profile_2_destination_construction_failure_aborts_completion_and_runtime() {
        let (prepared, _first, _second) = direct_profile_2_process_bundle();
        let runtime = prepared
            .bundle()
            .and_then(PreparedSnapshotV2MultiBlockBundle::async_runtime)
            .expect("mixed fixture should own one runtime")
            .clone();
        let error = prepared
            .construct_destination(|bundle| {
                bundle
                    .abort()
                    .expect("injected construction should release the runtime");
                Err::<(), _>(io::Error::other("injected destination construction"))
            })
            .expect_err("injected destination construction should fail");
        assert!(!error.is_terminal());
        assert!(matches!(
            error,
            PreparedSnapshotV2MultiBlockDestinationConstructionError::Construction {
                completion_abort: None,
                ..
            }
        ));
        assert_eq!(runtime.generation_count().expect("runtime should lock"), 0);
    }

    #[test]
    fn profile_2_invalid_destination_state_is_terminal() {
        let invalid = PreparedSnapshotV2MultiBlockRestoreBundle {
            bundle: None,
            completion: None,
        };
        let error = invalid
            .construct_destination(Ok::<_, std::convert::Infallible>)
            .expect_err("consumed destination state must fail");
        assert!(error.is_terminal());
        assert!(matches!(
            error,
            PreparedSnapshotV2MultiBlockDestinationConstructionError::InvalidState {
                bundle_cleanup: None
            }
        ));
    }

    #[test]
    fn profile_2_controller_failure_destroys_destination_before_completion_abort() {
        let (prepared, _first, _second) = direct_profile_2_process_bundle();
        let runtime = prepared
            .bundle()
            .and_then(PreparedSnapshotV2MultiBlockBundle::async_runtime)
            .expect("mixed fixture should own one runtime")
            .clone();
        let destroyed = Rc::new(Cell::new(false));
        let destination = prepared
            .construct_destination({
                let destroyed = Rc::clone(&destroyed);
                move |bundle| {
                    Ok::<_, std::convert::Infallible>(TestPrivateMultiBlockDestination {
                        bundle: Some(bundle),
                        destroyed,
                    })
                }
            })
            .expect("private destination should construct");
        let error = destination
            .commit(
                |destination| {
                    Err::<(TestPrivateMultiBlockDestination, ()), _>((
                        destination,
                        io::Error::other("injected controller preparation"),
                    ))
                },
                TestPrivateMultiBlockDestination::destroy,
            )
            .expect_err("injected controller preparation should fail");
        assert!(destroyed.get());
        assert!(!error.is_terminal());
        assert!(matches!(
            error,
            PreparedSnapshotV2MultiBlockDestinationCommitError::Controller {
                destination_cleanup: None,
                completion_abort: None,
                ..
            }
        ));
        assert_eq!(runtime.generation_count().expect("runtime should lock"), 0);
    }

    #[test]
    fn profile_2_completion_failure_destroys_destination_and_is_terminal() {
        let (mut prepared, _first, _second) = direct_profile_2_process_bundle();
        prepared
            .completion
            .take()
            .expect("direct completion should exist")
            .abort()
            .expect("direct completion should abort before injection");

        let root = TempRoot::new("multi-completion", b"root");
        let fixture = contained_restore_authority_for_test(root.path(), false);
        let root_owner = contained_requested_root()
            .prepare(Some(fixture.authority()), || false)
            .expect("contained root should prepare")
            .into_root()
            .expect("contained root should take exactly");
        let (backing, completion) = root_owner.into_parts();
        drop(backing);
        let PreparedSnapshotRootRestoreCompletion {
            lease,
            contained_transaction,
        } = completion;
        let PreparedSnapshotRootBackingLease {
            selector: _,
            claim,
            consumed: _,
        } = lease;
        prepared.completion = Some(PreparedSnapshotDriveRestoreCompletion {
            claims: claim.into_iter().collect(),
            contained_transaction,
        });

        let runtime = prepared
            .bundle()
            .and_then(PreparedSnapshotV2MultiBlockBundle::async_runtime)
            .expect("mixed fixture should own one runtime")
            .clone();
        let destroyed = Rc::new(Cell::new(false));
        let destination = prepared
            .construct_destination({
                let destroyed = Rc::clone(&destroyed);
                move |bundle| {
                    Ok::<_, std::convert::Infallible>(TestPrivateMultiBlockDestination {
                        bundle: Some(bundle),
                        destroyed,
                    })
                }
            })
            .expect("private destination should construct");
        fixture.invalidate_generation();
        let error = destination
            .commit(
                |destination| {
                    Ok::<_, (TestPrivateMultiBlockDestination, std::convert::Infallible)>((
                        destination,
                        (),
                    ))
                },
                TestPrivateMultiBlockDestination::destroy,
            )
            .expect_err("stale contained completion should fail");
        assert!(destroyed.get());
        assert!(error.is_terminal());
        assert!(matches!(
            error,
            PreparedSnapshotV2MultiBlockDestinationCommitError::Completion {
                destination_cleanup: None,
                ..
            }
        ));
        assert_eq!(runtime.generation_count().expect("runtime should lock"), 0);
    }

    #[test]
    fn profile_2_process_bundle_failure_aborts_aggregate_completion() {
        let template = multi_fixture_graph(None, None);
        let first_path = unique_short_path();
        let second_path = unique_short_path();
        let first = TempRoot::new_at(
            first_path,
            &vec![
                0x41;
                usize::try_from(template.records()[0].block().backing_bytes())
                    .expect("fixture length should fit")
            ],
        );
        let second = TempRoot::new_at(
            second_path,
            &vec![
                0x42;
                usize::try_from(template.records()[1].block().backing_bytes())
                    .expect("fixture length should fit")
            ],
        );
        let graph = multi_fixture_graph(Some(first.path()), Some(second.path()));
        let memory = multi_restore_memory(&graph);
        let error =
            RequestedSnapshotRestoreResources::prepare_native_v2_multi_block_restore_bundle_with(
                graph,
                &memory,
                Instant::now(),
                None,
                || false,
                |_plan, _configs, _backings| Err(SnapshotV2MultiBlockBundleError::Allocation),
            )
            .expect_err("injected bundle construction should fail");
        assert_eq!(
            error.disposition(),
            SnapshotRestoreResourceDisposition::Retryable
        );
        assert!(matches!(
            error,
            SnapshotV2MultiBlockRestoreBundleError::Bundle {
                completion_abort: None,
                ..
            }
        ));
        assert!(first.path().is_file());
        assert!(second.path().is_file());
    }

    #[test]
    fn profile_2_contained_batch_has_no_path_fallback_and_aborts_as_one_vector() {
        let graph = multi_fixture_graph(None, None);
        let selectors = [
            PathBuf::from("bangbang-grant:drive-ro"),
            PathBuf::from("bangbang-grant:drive-rw"),
        ];
        let expected_lengths = [
            fs::metadata(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/contained_session.rs"))
                .expect("read-only grant metadata should read")
                .len(),
            fs::metadata(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/vmm.rs"))
                .expect("read-write grant metadata should read")
                .len(),
        ];
        let mut drives = Vec::new();
        let mut drive_keys = Vec::new();
        let mut resources = Vec::new();
        let mut configs = DriveConfigs::new();
        for ((record, selector), expected_len) in
            graph.records().iter().zip(&selectors).zip(expected_lengths)
        {
            let public_id = SnapshotRestorePublicId::try_from(record.config().drive_id())
                .expect("fixture public ID should validate");
            let key = SnapshotRestoreResourceKey::new(
                record.key(),
                public_id,
                SnapshotRestoreResourceClass::BlockBacking,
            );
            let mut input = DriveConfigInput::new(
                record.config().drive_id(),
                record.config().drive_id(),
                selector,
                record.is_root(),
            )
            .with_is_read_only(record.config().is_read_only())
            .with_cache_type(record.config().cache_type())
            .with_io_engine(record.config().io_engine());
            if let Some(partuuid) = record.config().partuuid() {
                input = input.with_partuuid(partuuid);
            }
            if let Some(rate_limiter) = record.config().rate_limiter() {
                input = input.with_rate_limiter(rate_limiter);
            }
            configs
                .insert(input)
                .expect("contained projected config should validate");
            drives.push(RequestedSnapshotDriveRestoreResource {
                key: key.clone(),
                selector: selector.clone(),
                is_read_only: record.config().is_read_only(),
                expected_len,
                kind: SnapshotRestoreFileKind::Block,
            });
            drive_keys.push(key.clone());
            resources.push(key);
        }
        let manifest = SnapshotRestoreManifest::try_new(resources, Vec::new())
            .expect("contained manifest should validate");
        let bindings = manifest
            .try_into_bindings()
            .expect("contained bindings should allocate");
        let request = RequestedSnapshotMultiDriveRestoreResources {
            drives,
            drive_keys,
            drive_configs: configs,
            bindings,
            file_set_kind: SnapshotRestoreFileSetKind::MultiBlock,
        };
        let fixture = contained_restore_authority_with_grants_for_test(
            file_grant_authority_for_test(),
            false,
        );
        let prepared = request
            .prepare(Some(fixture.authority()), || false)
            .expect("contained mixed vector should prepare");
        let batch = prepared
            .into_drive_batch()
            .expect("contained bindings should become one batch");
        let diagnostic = format!("{batch:?}");
        for selector in &selectors {
            assert!(!diagnostic.contains(selector.to_string_lossy().as_ref()));
        }
        let (configs, backings, completion) = batch.into_parts();
        assert_eq!(configs.as_slice().len(), 2);
        assert_eq!(backings.len(), 2);
        for (((config, backing), selector), expected_len) in configs
            .as_slice()
            .iter()
            .zip(&backings)
            .zip(&selectors)
            .zip(expected_lengths)
        {
            assert_eq!(config.path_on_host(), Some(selector.as_path()));
            assert_eq!(backing.len(), expected_len);
        }
        assert!(backings[0].is_read_only());
        assert!(!backings[1].is_read_only());
        drop(backings);
        completion
            .abort()
            .expect("contained claims and generation should abort once");
        for (selector, access) in [
            (&selectors[0], GrantAccess::ReadOnly),
            (&selectors[1], GrantAccess::ReadWrite),
        ] {
            fixture
                .grants()
                .prepare_drive_backing_claim(selector, access)
                .expect("batch abort should restore every grant")
                .expect("selector should remain contained")
                .abort()
                .expect("restored grant should remain reusable");
        }
    }

    #[test]
    fn profile_2_contained_authority_failures_are_preconstruction_retryable_and_reusable() {
        let exact_selectors = [
            Path::new("bangbang-grant:drive-ro"),
            Path::new("bangbang-grant:drive-rw"),
        ];
        let exact_access = [true, false];
        let exact_lengths = contained_profile_2_lengths();
        let mut diagnostics = Vec::new();

        let missing_backing = TempRoot::new(
            "profile-2-missing",
            &vec![0x71; usize::try_from(exact_lengths[0]).expect("fixture length should fit")],
        );
        let missing = contained_restore_authority_with_grants_for_test(
            root_file_grant_authority_for_test(missing_backing.path()),
            false,
        );
        let error = contained_profile_2_request(exact_selectors, exact_access, exact_lengths)
            .prepare(Some(missing.authority()), || false)
            .expect_err("missing second selector must fail complete-set reservation");
        assert_eq!(
            error.stage,
            SnapshotRestoreResourceStage::ContainedReservation
        );
        assert_eq!(
            error.disposition,
            SnapshotRestoreResourceDisposition::Retryable
        );
        assert!(!error.cleanup_failed);
        diagnostics.push(format!("{error:?} {error}"));
        missing
            .grants()
            .prepare_drive_backing_claim(exact_selectors[0], GrantAccess::ReadOnly)
            .expect("missing-set failure should retain the present claim")
            .expect("present claim should remain contained")
            .abort()
            .expect("present claim should restore");

        for (name, selectors, access, lengths, with_vsock) in [
            (
                "alias",
                [exact_selectors[0], exact_selectors[0]],
                [true, true],
                [exact_lengths[0], exact_lengths[0]],
                false,
            ),
            (
                "wrong-access",
                exact_selectors,
                [false, false],
                exact_lengths,
                false,
            ),
            (
                "wrong-role",
                [Path::new("bangbang-grant:kernel"), exact_selectors[1]],
                exact_access,
                [
                    fs::metadata(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
                        .expect("kernel-role fixture metadata should read")
                        .len(),
                    exact_lengths[1],
                ],
                false,
            ),
            (
                "wrong-kind",
                [
                    Path::new("bangbang-grant:vsock-directory"),
                    exact_selectors[1],
                ],
                exact_access,
                exact_lengths,
                true,
            ),
            (
                "wrong-size",
                exact_selectors,
                exact_access,
                [exact_lengths[0].saturating_add(1), exact_lengths[1]],
                false,
            ),
        ] {
            let fixture = contained_restore_authority_with_grants_for_test(
                file_grant_authority_for_test(),
                with_vsock,
            );
            let error = contained_profile_2_request(selectors, access, lengths)
                .prepare(Some(fixture.authority()), || false)
                .unwrap_err();
            assert_eq!(
                error.stage,
                SnapshotRestoreResourceStage::ContainedReservation,
                "{name} should fail during complete-set reservation"
            );
            assert_eq!(
                error.disposition,
                SnapshotRestoreResourceDisposition::Retryable,
                "{name} should remain retryable"
            );
            assert!(!error.cleanup_failed, "{name} cleanup should remain exact");
            diagnostics.push(format!("{error:?} {error}"));
            assert_profile_2_grants_reusable(fixture.grants());
            fixture.assert_authorities_available(with_vsock);
        }

        let swapped = contained_restore_authority_with_grants_for_test(
            file_grant_authority_for_test(),
            false,
        );
        let request = contained_profile_2_request(exact_selectors, exact_access, exact_lengths);
        let contained_requests = request
            .drives
            .iter()
            .map(|drive| {
                ContainedSnapshotRestoreDriveRequest::new(
                    &drive.selector,
                    if drive.is_read_only {
                        GrantAccess::ReadOnly
                    } else {
                        GrantAccess::ReadWrite
                    },
                    Some(drive.expected_len),
                )
            })
            .collect::<Vec<_>>();
        let reserved = swapped
            .authority()
            .prepare_drives(&contained_requests, None, &|| false)
            .expect("swapped-claim fixture should reserve");
        let (mut claims, vsock, transaction) = reserved
            .into_drive_parts()
            .expect("swapped-claim fixture should split");
        assert!(vsock.is_none());
        claims.swap(0, 1);
        let failure = prepare_contained_drives(
            &request.drives,
            claims,
            SnapshotRestoreFileSetKind::MultiBlock,
            &|| false,
        )
        .expect_err("swapped descriptor bindings must fail before construction");
        let (error, resources, claims) = *failure;
        assert_eq!(error.stage, SnapshotRestoreResourceStage::DrivePreparation);
        assert_eq!(
            error.disposition,
            SnapshotRestoreResourceDisposition::Retryable
        );
        let outcome = abort_prepared_drive_claims(claims)
            .merge(abort_prepared_drive_resources(resources))
            .merge(abort_contained_transaction(Some(transaction)));
        assert_eq!(
            outcome.disposition,
            SnapshotRestoreResourceDisposition::Retryable
        );
        assert!(!outcome.cleanup_failed);
        diagnostics.push(format!("{error:?} {error}"));
        assert_profile_2_grants_reusable(swapped.grants());

        let extra = contained_restore_authority_with_grants_for_test(
            file_grant_authority_for_test(),
            false,
        );
        let extra_claims = contained_restore_authority_with_grants_for_test(
            file_grant_authority_for_test(),
            false,
        );
        let request = contained_profile_2_request(exact_selectors, exact_access, exact_lengths);
        let contained_requests = request
            .drives
            .iter()
            .map(|drive| {
                ContainedSnapshotRestoreDriveRequest::new(
                    &drive.selector,
                    if drive.is_read_only {
                        GrantAccess::ReadOnly
                    } else {
                        GrantAccess::ReadWrite
                    },
                    Some(drive.expected_len),
                )
            })
            .collect::<Vec<_>>();
        let reserved = extra
            .authority()
            .prepare_drives(&contained_requests, None, &|| false)
            .expect("extra-claim fixture should reserve the exact vector");
        let (mut claims, vsock, transaction) = reserved
            .into_drive_parts()
            .expect("extra-claim fixture should split");
        assert!(vsock.is_none());
        claims.push(
            extra_claims
                .grants()
                .prepare_drive_backing_claim(exact_selectors[0], GrantAccess::ReadOnly)
                .expect("extra claim should validate")
                .expect("extra selector should remain contained"),
        );
        let failure = prepare_contained_drives(
            &request.drives,
            claims,
            SnapshotRestoreFileSetKind::MultiBlock,
            &|| false,
        )
        .expect_err("extra prepared claims must fail before construction");
        let (error, resources, claims) = *failure;
        assert_eq!(error.stage, SnapshotRestoreResourceStage::DrivePreflight);
        assert_eq!(
            error.disposition,
            SnapshotRestoreResourceDisposition::Terminal
        );
        assert!(resources.is_empty());
        let outcome = abort_prepared_drive_claims(claims)
            .merge(abort_prepared_drive_resources(resources))
            .merge(abort_contained_transaction(Some(transaction)));
        assert_eq!(
            outcome.disposition,
            SnapshotRestoreResourceDisposition::Retryable
        );
        assert!(!outcome.cleanup_failed);
        diagnostics.push(format!("{error:?} {error}"));
        assert_profile_2_grants_reusable(extra.grants());
        assert_profile_2_grants_reusable(extra_claims.grants());

        let changed_backing = TempRoot::new("profile-2-changed-geometry", &[0x72; 4096]);
        let changed = contained_restore_authority_with_grants_for_test(
            root_file_grant_authority_for_test(changed_backing.path()),
            false,
        );
        let mut request = contained_profile_2_request(exact_selectors, exact_access, exact_lengths);
        request.drives[0].expected_len = 4096;
        let contained_request = [ContainedSnapshotRestoreDriveRequest::new(
            &request.drives[0].selector,
            GrantAccess::ReadOnly,
            Some(request.drives[0].expected_len),
        )];
        let reserved = changed
            .authority()
            .prepare_drives(&contained_request, None, &|| false)
            .expect("changed-geometry fixture should reserve");
        fs::write(changed_backing.path(), [0x73; 4097])
            .expect("reserved backing geometry should change before adoption");
        let (claims, vsock, transaction) = reserved
            .into_drive_parts()
            .expect("changed-geometry fixture should split");
        assert!(vsock.is_none());
        let failure = prepare_contained_drives(
            &request.drives[..1],
            claims,
            SnapshotRestoreFileSetKind::MultiBlock,
            &|| false,
        )
        .expect_err("changed backing geometry must fail before construction");
        let (error, resources, claims) = *failure;
        assert_eq!(error.stage, SnapshotRestoreResourceStage::DrivePreflight);
        assert_eq!(
            error.disposition,
            SnapshotRestoreResourceDisposition::Retryable
        );
        assert!(resources.is_empty());
        let outcome = abort_prepared_drive_claims(claims)
            .merge(abort_prepared_drive_resources(resources))
            .merge(abort_contained_transaction(Some(transaction)));
        assert_eq!(
            outcome.disposition,
            SnapshotRestoreResourceDisposition::Retryable
        );
        assert!(!outcome.cleanup_failed);
        diagnostics.push(format!("{error:?} {error}"));
        fs::write(changed_backing.path(), [0x72; 4096])
            .expect("changed backing geometry should reset after cleanup");
        changed
            .grants()
            .prepare_drive_backing_claim(exact_selectors[0], GrantAccess::ReadOnly)
            .expect("changed-geometry authority should remain valid")
            .expect("changed-geometry selector should remain contained")
            .abort()
            .expect("changed-geometry claim should restore");

        let consumed = contained_restore_authority_with_grants_for_test(
            file_grant_authority_for_test(),
            false,
        );
        consumed
            .grants()
            .prepare_drive_backing_claim(exact_selectors[0], GrantAccess::ReadOnly)
            .expect("consumed fixture claim should validate")
            .expect("consumed fixture selector should remain contained")
            .commit();
        let error = contained_profile_2_request(exact_selectors, exact_access, exact_lengths)
            .prepare(Some(consumed.authority()), || false)
            .expect_err("already-consumed claim must fail complete-set reservation");
        assert_eq!(
            error.stage,
            SnapshotRestoreResourceStage::ContainedReservation
        );
        assert_eq!(
            error.disposition,
            SnapshotRestoreResourceDisposition::Retryable
        );
        diagnostics.push(format!("{error:?} {error}"));
        consumed
            .grants()
            .prepare_drive_backing_claim(exact_selectors[1], GrantAccess::ReadWrite)
            .expect("consumed-set failure should retain the other claim")
            .expect("other claim should remain contained")
            .abort()
            .expect("other claim should restore");

        let cancelled = contained_restore_authority_with_grants_for_test(
            file_grant_authority_for_test(),
            false,
        );
        let error = contained_profile_2_request(exact_selectors, exact_access, exact_lengths)
            .prepare(Some(cancelled.authority()), || true)
            .expect_err("outer cancellation must stop before authority use");
        assert_eq!(error.stage, SnapshotRestoreResourceStage::Cancellation);
        assert_eq!(
            error.disposition,
            SnapshotRestoreResourceDisposition::Retryable
        );
        diagnostics.push(format!("{error:?} {error}"));
        assert_profile_2_grants_reusable(cancelled.grants());

        let diagnostic = diagnostics.join(" ");
        for private in [
            missing_backing.path().to_string_lossy(),
            std::borrow::Cow::Borrowed("bangbang-grant:drive-ro"),
            std::borrow::Cow::Borrowed("bangbang-grant:drive-rw"),
            std::borrow::Cow::Borrowed("bangbang-grant:kernel"),
            std::borrow::Cow::Borrowed("bangbang-grant:vsock-directory"),
            changed_backing.path().to_string_lossy(),
        ] {
            assert!(!diagnostic.contains(private.as_ref()));
        }
    }

    #[test]
    fn profile_2_direct_preflight_rejects_alias_geometry_grants_and_cancellation() {
        let graph = multi_fixture_graph(None, None);
        let shared = TempRoot::new("multi-alias", &[0x33; 4096]);
        let requests = graph
            .records()
            .iter()
            .map(|record| {
                let public_id = SnapshotRestorePublicId::try_from(record.config().drive_id())
                    .expect("fixture public ID should validate");
                RequestedSnapshotDriveRestoreResource {
                    key: SnapshotRestoreResourceKey::new(
                        record.key(),
                        public_id,
                        SnapshotRestoreResourceClass::BlockBacking,
                    ),
                    selector: shared.path().to_path_buf(),
                    is_read_only: record.config().is_read_only(),
                    expected_len: 4096,
                    kind: SnapshotRestoreFileKind::Block,
                }
            })
            .collect::<Vec<_>>();
        let alias =
            prepare_direct_drives(&requests, SnapshotRestoreFileSetKind::MultiBlock, &|| false)
                .expect_err("same descriptor identity must reject");
        assert_eq!(alias.stage, SnapshotRestoreResourceStage::DrivePreflight);
        assert!(matches!(
            alias.kind,
            SnapshotRestoreResourceErrorKind::InvalidDriveSet
        ));

        let first = TempRoot::new("multi-geometry-0", &[0x44; 4096]);
        let second = TempRoot::new("multi-geometry-1", &[0x55; 8192]);
        let mut distinct = requests;
        distinct[0].selector = first.path().to_path_buf();
        distinct[1].selector = second.path().to_path_buf();
        distinct[1].expected_len = 8193;
        let geometry =
            prepare_direct_drives(&distinct, SnapshotRestoreFileSetKind::MultiBlock, &|| false)
                .expect_err("wrong expected geometry must reject");
        assert_eq!(geometry.stage, SnapshotRestoreResourceStage::DrivePreflight);

        distinct[1].expected_len = 8192;
        let calls = Cell::new(0_u8);
        let changed =
            prepare_direct_drives(&distinct, SnapshotRestoreFileSetKind::MultiBlock, &|| {
                let next = calls.get().saturating_add(1);
                calls.set(next);
                if next == 6 {
                    fs::write(first.path(), [0x66; 4097])
                        .expect("reserved direct fixture should change before adoption");
                }
                false
            })
            .expect_err("changed preflight observation must reject before binding");
        assert_eq!(
            changed.stage,
            SnapshotRestoreResourceStage::DrivePreparation
        );
        fs::write(first.path(), [0x44; 4096])
            .expect("changed direct fixture should reset for later checks");

        distinct[0].selector = PathBuf::from(ROOT_REFERENCE);
        let grant =
            prepare_direct_drives(&distinct, SnapshotRestoreFileSetKind::MultiBlock, &|| false)
                .expect_err("direct mode must not treat a grant reference as a path");
        assert_eq!(grant.stage, SnapshotRestoreResourceStage::DrivePreflight);

        distinct[0].selector = first.path().to_path_buf();
        let calls = Cell::new(0_u8);
        let cancelled =
            prepare_direct_drives(&distinct, SnapshotRestoreFileSetKind::MultiBlock, &|| {
                let next = calls.get().saturating_add(1);
                calls.set(next);
                next >= 4
            })
            .expect_err("mid-vector cancellation must unwind earlier reservations");
        assert_eq!(cancelled.stage, SnapshotRestoreResourceStage::Cancellation);
        let diagnostic = format!("{alias:?} {geometry:?} {changed:?} {grant:?} {cancelled:?}");
        for private in [
            shared.path(),
            first.path(),
            second.path(),
            Path::new(ROOT_REFERENCE),
        ] {
            assert!(!diagnostic.contains(private.to_string_lossy().as_ref()));
        }
    }

    #[test]
    fn composed_caller_orders_share_one_phase_trace_and_exact_direct_binding() {
        let root = TempRoot::new("composed-order", b"root");
        for reverse_caller_order in [false, true] {
            let captured_path = socket_path("captured-order");
            let destination = socket_path("destination-order");
            let captured = selector(&captured_path);
            let requested_override = SnapshotVsockOverride::new(&destination);
            let (request, _root_key, vsock_key) = composed_request(
                root.path(),
                &captured,
                Some(&requested_override),
                reverse_caller_order,
            );
            assert_eq!(
                request
                    .bindings
                    .manifest()
                    .resources()
                    .iter()
                    .map(SnapshotRestoreResourceKey::resource_class)
                    .collect::<Vec<_>>(),
                [
                    SnapshotRestoreResourceClass::BlockBacking,
                    SnapshotRestoreResourceClass::VsockEndpoint,
                ]
            );
            assert_eq!(
                request
                    .bindings
                    .manifest()
                    .overrides()
                    .cloned()
                    .collect::<Vec<_>>(),
                [vsock_key]
            );

            let events = RefCell::new(Vec::new());
            let prepared = request
                .prepare_with_independent_observer(
                    None,
                    None,
                    None,
                    None,
                    || false,
                    |step| events.borrow_mut().push(step),
                )
                .expect("real direct root/vsock batch should prepare");
            assert_eq!(
                events.into_inner(),
                [
                    SnapshotRestorePreparationStep::RootReservation,
                    SnapshotRestorePreparationStep::VsockReservation,
                    SnapshotRestorePreparationStep::RootLocalPreparation,
                    SnapshotRestorePreparationStep::VsockLocalPreparation,
                    SnapshotRestorePreparationStep::VsockPublication,
                    SnapshotRestorePreparationStep::RootBinding,
                    SnapshotRestorePreparationStep::VsockBinding,
                    SnapshotRestorePreparationStep::Completion,
                ]
            );
            assert!(!captured_path.exists());
            assert!(destination.exists());

            let (root, vsock) = prepared
                .into_root_and_optional_vsock()
                .expect("complete batch should take every exact owner");
            let vsock = vsock.expect("composed batch should retain vsock");
            assert_eq!(
                vsock.abort().expect("direct vsock abort should clean"),
                VsockRestoreDisposition::Retryable
            );
            assert!(!destination.exists());
            root.abort()
                .expect("direct root abort should be infallible");
        }
    }

    #[test]
    fn composed_success_consumes_vsock_once_and_moves_the_active_guard() {
        let root = TempRoot::new("composed-adoption", b"root");
        let captured_path = socket_path("adoption-source");
        let destination = socket_path("adoption-target");
        let config = VsockConfigInput::new(42, captured_path.to_string_lossy())
            .validate()
            .expect("source config should validate");
        let source = PreparedVsockDevice::from_config_with_host_socket(&config)
            .expect("source listener should prepare");
        let (_, _, config_space, device) = source.into_parts();
        let source = VirtioMmioRegisterHandler::with_device_config_and_activation(
            VIRTIO_VSOCK_DEVICE_ID,
            config_space.available_features(),
            &VIRTIO_VSOCK_QUEUE_SIZES,
            config_space,
            device,
        )
        .expect("source handler should build");
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), 0x20_000)
                .expect("guest range should validate"),
        ])
        .expect("guest layout should validate");
        let memory = GuestMemory::allocate(&layout).expect("guest memory should allocate");
        let (captured, _) = source
            .capture_vsock_state(&config, &memory, VirtioVsockTransportResetAttempt::Inactive)
            .expect("source should capture");
        let requested_override = SnapshotVsockOverride::new(&destination);
        let (request, _, _) = composed_request(
            root.path(),
            captured.device().backend_selector(),
            Some(&requested_override),
            true,
        );
        let (root_owner, vsock) = request
            .prepare(None, || false)
            .expect("composed owners should prepare before construction")
            .into_root_and_optional_vsock()
            .expect("exact owners should be taken before construction");
        let vsock = vsock.expect("captured vsock should have one exact owner");
        let (reconstructed, active_guard) = vsock
            .adopt(|resource| captured.reconstruct_snapshot_device(&memory, resource))
            .expect("one reconstruction should consume and commit the vsock resource");

        assert_eq!(reconstructed.uds_path(), destination);
        assert!(destination.exists());
        assert!(!format!("{active_guard:?}").contains(destination.to_string_lossy().as_ref()));
        drop(reconstructed);
        assert!(
            destination.exists(),
            "active guard must retain the published socket after device drop"
        );
        drop(active_guard);
        assert!(!destination.exists());
        root_owner
            .abort()
            .expect("direct root owner should abort infallibly");
        drop(source);
        assert!(!captured_path.exists());
    }

    #[test]
    fn later_direct_vsock_failure_restores_the_earlier_root_authority() {
        let root = TempRoot::new("composed-later-failure", b"root");
        let authority = root_file_grant_authority_for_test(root.path());
        let missing_parent = socket_path("missing-parent");
        let destination = missing_parent.join("backend.sock");
        let captured = selector(&destination);
        let (request, _, _) = composed_request(Path::new(ROOT_REFERENCE), &captured, None, false);
        let events = RefCell::new(Vec::new());
        let error = request
            .prepare_with_independent_observer(
                Some(&authority),
                None,
                None,
                None,
                || false,
                |step| events.borrow_mut().push(step),
            )
            .expect_err("later direct socket preparation should fail");

        assert_eq!(
            events.into_inner(),
            [
                SnapshotRestorePreparationStep::RootReservation,
                SnapshotRestorePreparationStep::VsockReservation,
                SnapshotRestorePreparationStep::RootLocalPreparation,
                SnapshotRestorePreparationStep::VsockLocalPreparation,
            ]
        );
        assert_eq!(
            error.disposition,
            SnapshotRestoreResourceDisposition::Retryable
        );
        assert!(!error.cleanup_failed);
        assert!(matches!(
            error.kind,
            SnapshotRestoreResourceErrorKind::Vsock(_)
        ));
        assert!(!destination.exists());
        assert_root_claim_restored(&authority);
    }

    #[test]
    fn contained_vsock_reservation_failure_restores_both_authorities_before_local_work() {
        let root = TempRoot::new("composed-contained-reservation", b"root");
        let root_authority = root_file_grant_authority_for_test(root.path());
        let (directory_authority, _directory) = vsock_directory_authority_for_test();
        let reference = Path::new("bangbang-grant:vsock-directory/restored.sock");
        let captured = selector(reference);
        let (request, _, _) = composed_request(Path::new(ROOT_REFERENCE), &captured, None, true);
        let events = RefCell::new(Vec::new());
        let error = request
            .prepare_with_independent_observer(
                Some(&root_authority),
                Some(&directory_authority),
                None,
                None,
                || false,
                |step| events.borrow_mut().push(step),
            )
            .expect_err("missing broker must fail during reversible reservation");

        assert_eq!(
            events.into_inner(),
            [
                SnapshotRestorePreparationStep::RootReservation,
                SnapshotRestorePreparationStep::VsockReservation,
            ]
        );
        assert_eq!(
            error.disposition,
            SnapshotRestoreResourceDisposition::Retryable
        );
        assert_root_claim_restored(&root_authority);
        let restored = directory_authority
            .prepare_socket_directory(reference, ResourceRole::VsockSocketDirectory)
            .expect("directory registry should remain usable")
            .expect("exact directory claim should be restored");
        drop(restored);
    }

    #[test]
    fn composed_logical_failures_precede_every_authority_and_socket_operation() {
        let root = TempRoot::new("composed-logical-preflight", b"root");
        let root_authority = root_file_grant_authority_for_test(root.path());
        let captured_path = socket_path("logical-captured");
        let destination = socket_path("logical-destination");
        let captured = selector(&captured_path);

        let without_device = RequestedVsockRestoreResource::resolve(
            None,
            Some(&SnapshotVsockOverride::new(&destination)),
        )
        .expect_err("override without captured device must fail");
        assert_eq!(
            without_device.stage(),
            crate::vsock_restore::VsockRestoreStage::Selection
        );

        let invalid = RequestedVsockRestoreResource::resolve(
            Some(&captured),
            Some(&SnapshotVsockOverride::new("bad\nselector")),
        )
        .expect_err("invalid override must fail during pure selection");
        assert_eq!(
            invalid.stage(),
            crate::vsock_restore::VsockRestoreStage::Selection
        );

        let graph = fixture_graph(SnapshotV2DeviceTransportKind::Mmio);
        let root_key = SnapshotRestoreResourceKey::new(
            graph.root_key(),
            SnapshotRestorePublicId::try_from(graph.record().config().drive_id())
                .expect("root ID should validate"),
            SnapshotRestoreResourceClass::BlockBacking,
        );
        let vsock_key = SnapshotRestoreResourceKey::new(
            graph.root_key(),
            SnapshotRestorePublicId::try_from("vsock0").expect("vsock ID should validate"),
            SnapshotRestoreResourceClass::VsockEndpoint,
        );
        let first = RequestedVsockRestoreResource::resolve(
            Some(&captured),
            Some(&SnapshotVsockOverride::new(&destination)),
        )
        .expect("first override should resolve")
        .expect("captured device should exist");
        let second = RequestedVsockRestoreResource::resolve(
            Some(&captured),
            Some(&SnapshotVsockOverride::new(&destination)),
        )
        .expect("second override should resolve")
        .expect("captured device should exist");
        let duplicate = RequestedSnapshotRestoreResources::try_from_exact_requests(vec![
            RequestedSnapshotRestoreResource::Root {
                key: root_key.clone(),
                selector: PathBuf::from(ROOT_REFERENCE),
            },
            RequestedSnapshotRestoreResource::Vsock {
                key: vsock_key.clone(),
                request: first,
            },
            RequestedSnapshotRestoreResource::Vsock {
                key: vsock_key.clone(),
                request: second,
            },
        ])
        .expect_err("duplicate override identity must fail before preparation");
        assert!(matches!(
            duplicate.kind,
            SnapshotRestoreResourceErrorKind::Manifest(
                SnapshotRestoreManifestError::DuplicateResource
            )
        ));

        let wrong_class_request = RequestedVsockRestoreResource::resolve(
            Some(&captured),
            Some(&SnapshotVsockOverride::new(&destination)),
        )
        .expect("wrong-class selectors should still resolve")
        .expect("captured device should exist");
        let wrong_class = RequestedSnapshotRestoreResources::try_from_exact_requests(vec![
            RequestedSnapshotRestoreResource::Root {
                key: vsock_key,
                selector: PathBuf::from(ROOT_REFERENCE),
            },
            RequestedSnapshotRestoreResource::Vsock {
                key: root_key,
                request: wrong_class_request,
            },
        ])
        .expect_err("logical class swap must fail before preparation");
        assert!(matches!(
            wrong_class.kind,
            SnapshotRestoreResourceErrorKind::OwnerClassMismatch
        ));

        assert!(!captured_path.exists());
        assert!(!destination.exists());
        assert_root_claim_restored(&root_authority);
    }

    #[test]
    fn post_publication_binding_failure_aborts_vsock_then_root() {
        let root = TempRoot::new("composed-reverse-abort", b"root");
        let authority = root_file_grant_authority_for_test(root.path());
        let destination = socket_path("reverse-abort");
        let captured = selector(&destination);
        let (mut request, _, _) =
            composed_request(Path::new(ROOT_REFERENCE), &captured, None, true);
        request.root_key = SnapshotRestoreResourceKey::new(
            request.root_key.device_key(),
            SnapshotRestorePublicId::try_from("swapped-root")
                .expect("swapped root ID should validate"),
            SnapshotRestoreResourceClass::BlockBacking,
        );
        let events = RefCell::new(Vec::new());
        let error = request
            .prepare_with_independent_observer(
                Some(&authority),
                None,
                None,
                None,
                || false,
                |step| events.borrow_mut().push(step),
            )
            .expect_err("hostile root binding should fail after both owners prepare");

        assert_eq!(
            events.into_inner(),
            [
                SnapshotRestorePreparationStep::RootReservation,
                SnapshotRestorePreparationStep::VsockReservation,
                SnapshotRestorePreparationStep::RootLocalPreparation,
                SnapshotRestorePreparationStep::VsockLocalPreparation,
                SnapshotRestorePreparationStep::VsockPublication,
                SnapshotRestorePreparationStep::RootBinding,
                SnapshotRestorePreparationStep::VsockAbort,
                SnapshotRestorePreparationStep::RootAbort,
            ]
        );
        assert_eq!(
            error.disposition,
            SnapshotRestoreResourceDisposition::Terminal
        );
        assert!(matches!(
            error.kind,
            SnapshotRestoreResourceErrorKind::Binding(
                SnapshotRestoreBindingRejectionReason::ExtraBinding
            )
        ));
        assert!(!destination.exists());
        assert_root_claim_restored(&authority);
    }

    #[test]
    fn terminal_but_clean_vsock_evidence_keeps_the_batch_terminal() {
        let outcome = vsock_abort_outcome(Ok(VsockRestoreDisposition::Terminal));
        assert_eq!(
            outcome,
            SnapshotRestoreAbortOutcome {
                disposition: SnapshotRestoreResourceDisposition::Terminal,
                cleanup_failed: false,
            }
        );
        let error = cancelled_batch_error().with_abort_outcome(outcome);
        assert_eq!(
            error.disposition,
            SnapshotRestoreResourceDisposition::Terminal
        );
        assert!(
            !error.cleanup_failed,
            "ordinary cleanup success must remain distinct from retry safety"
        );
    }

    #[test]
    fn composed_vsock_take_and_finish_are_exact_single_use() {
        let root = TempRoot::new("composed-consumption", b"root");
        let authority = root_file_grant_authority_for_test(root.path());
        let destination = socket_path("take-once");
        let captured = selector(&destination);
        let (request, root_key, vsock_key) =
            composed_request(Path::new(ROOT_REFERENCE), &captured, None, false);
        let mut prepared = request
            .prepare_with_independent(Some(&authority), None, None, None, || false)
            .expect("composed batch should prepare");
        let vsock = prepared
            .take_vsock(&vsock_key)
            .expect("first exact vsock take should succeed");
        let repeated = prepared
            .take_vsock(&vsock_key)
            .expect_err("second exact vsock take must fail");
        assert!(matches!(
            repeated.kind,
            SnapshotRestoreResourceErrorKind::Take(SnapshotRestoreTakeError::AlreadyTaken)
        ));
        let root_owner = prepared
            .take_root(&root_key)
            .expect("exact root take should succeed");
        prepared
            .finish()
            .expect("both exact takes should satisfy finish");
        assert_eq!(
            vsock.abort().expect("direct vsock should clean"),
            VsockRestoreDisposition::Retryable
        );
        root_owner.abort().expect("root should restore its claim");
        assert!(!destination.exists());
        assert_root_claim_restored(&authority);

        let second_authority = root_file_grant_authority_for_test(root.path());
        let second_destination = socket_path("unconsumed");
        let second_captured = selector(&second_destination);
        let (request, _, _) =
            composed_request(Path::new(ROOT_REFERENCE), &second_captured, None, false);
        let unconsumed = request
            .prepare_with_independent(Some(&second_authority), None, None, None, || false)
            .expect("second composed batch should prepare")
            .finish()
            .expect_err("finish must reject both unconsumed owners");
        assert_eq!(
            unconsumed.disposition,
            SnapshotRestoreResourceDisposition::Terminal
        );
        assert!(matches!(
            unconsumed.kind,
            SnapshotRestoreResourceErrorKind::Unconsumed {
                unconsumed_count: 2
            }
        ));
        assert!(!second_destination.exists());
        assert_root_claim_restored(&second_authority);
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
        completion
            .commit()
            .expect("contained completion should commit its exact claim");
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
