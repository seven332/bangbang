//! Process-owned destination transaction for restored vsock devices.

use std::fmt;

use bangbang_runtime::snapshot::{
    SnapshotVsockOverride, SnapshotVsockSelectorError, SnapshotVsockSelectors,
    resolve_snapshot_vsock_selectors,
};
use bangbang_runtime::vsock::{
    DirectVsockRestoreCleanupError, DirectVsockRestoreError, DirectVsockSocketGuard,
    SuppliedVsockListener, VirtioVsockReconstructionResource, VsockBackendSelector,
    prepare_direct_vsock_restore,
};
use bangbang_session::ResourceRole;
use bangbang_session::macos::runtime::WorkerSocketNamespace;

use crate::anchored_socket::{AnchoredSocketError, AnchoredSocketGuard, bind_prepared_vsock};
use crate::contained_session::{
    DirectoryGrantAuthority, PreparedSocketBrokerEndpoint, PreparedSocketDirectoryClaim,
    SocketBrokerAuthority, socket_directory_reference,
};

/// Whether the same process may safely retry a failed restore preparation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VsockRestoreDisposition {
    Retryable,
    Terminal,
}

/// Redacted stage at which vsock destination preparation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VsockRestoreStage {
    Selection,
    Cancellation,
    DirectPreparation,
    ContainedClaim,
    ContainedPublication,
    Adoption,
    Cleanup,
}

/// Value-redacted process failure for a vsock restore destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VsockRestoreErrorKind {
    Selector(SnapshotVsockSelectorError),
    Cancelled,
    ContainedAuthorityUnavailable,
    ContainedReferenceRequired,
    Grant,
    Namespace,
    Direct(DirectVsockRestoreError),
    Anchored(AnchoredSocketError),
    ContainedResourceIncomplete,
    ResourceNotConsumed,
    Cleanup(DirectVsockRestoreCleanupError),
}

/// Stage-aware restore destination failure with an explicit retry policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VsockRestoreError {
    stage: VsockRestoreStage,
    kind: VsockRestoreErrorKind,
    disposition: VsockRestoreDisposition,
}

impl VsockRestoreError {
    const fn retryable(stage: VsockRestoreStage, kind: VsockRestoreErrorKind) -> Self {
        Self {
            stage,
            kind,
            disposition: VsockRestoreDisposition::Retryable,
        }
    }

    const fn terminal(stage: VsockRestoreStage, kind: VsockRestoreErrorKind) -> Self {
        Self {
            stage,
            kind,
            disposition: VsockRestoreDisposition::Terminal,
        }
    }

    pub(crate) const fn stage(&self) -> VsockRestoreStage {
        self.stage
    }

    pub(crate) const fn kind(&self) -> VsockRestoreErrorKind {
        self.kind
    }

    pub(crate) const fn disposition(&self) -> VsockRestoreDisposition {
        self.disposition
    }
}

impl fmt::Display for VsockRestoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "vsock restore resource failed at {:?}: {:?} ({:?})",
            self.stage(),
            self.kind(),
            self.disposition()
        )
    }
}

impl std::error::Error for VsockRestoreError {}

enum VsockRestoreGuard {
    Direct(DirectVsockSocketGuard),
    Contained(AnchoredSocketGuard),
}

/// Cleanup authority retained for the lifetime of a committed restored VM.
pub(crate) enum ActiveVsockRestoreGuard {
    Direct(DirectVsockSocketGuard),
    Contained(AnchoredSocketGuard),
}

impl ActiveVsockRestoreGuard {
    pub(crate) fn cleanup(self) -> Result<(), ()> {
        match self {
            Self::Direct(guard) => guard.cleanup().map_err(|_| ()),
            Self::Contained(guard) => {
                drop(guard);
                Ok(())
            }
        }
    }
}

impl fmt::Debug for ActiveVsockRestoreGuard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Direct(guard) => formatter
                .debug_tuple("ActiveVsockRestoreGuard::Direct")
                .field(guard)
                .finish(),
            Self::Contained(guard) => formatter
                .debug_tuple("ActiveVsockRestoreGuard::Contained")
                .field(guard)
                .finish(),
        }
    }
}

/// Single-use listener/connector transaction prepared for #1490 reconstruction.
pub(crate) struct PreparedVsockRestoreResource {
    // Keep the runtime resource before the guard so ordinary drop closes live
    // listener/connector descriptors before removing the published name.
    resource: VirtioVsockReconstructionResource,
    guard: VsockRestoreGuard,
}

impl fmt::Debug for PreparedVsockRestoreResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedVsockRestoreResource")
            .field("resource", &"<owned>")
            .field("guard", &"<owned>")
            .finish()
    }
}

/// Reconstruction failure paired with the process retry policy after cleanup.
pub(crate) enum VsockRestoreAdoptionError<E> {
    Reconstruction {
        source: E,
        disposition: VsockRestoreDisposition,
    },
    Contract(VsockRestoreError),
}

impl<E> VsockRestoreAdoptionError<E> {
    pub(crate) const fn disposition(&self) -> VsockRestoreDisposition {
        match self {
            Self::Reconstruction { disposition, .. } => *disposition,
            Self::Contract(error) => error.disposition(),
        }
    }
}

impl<E> fmt::Debug for VsockRestoreAdoptionError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reconstruction {
                source,
                disposition,
            } => formatter
                .debug_struct("VsockRestoreAdoptionError::Reconstruction")
                .field("source", &"<redacted>")
                .field("source_type", &std::any::type_name_of_val(source))
                .field("disposition", disposition)
                .finish(),
            Self::Contract(error) => formatter
                .debug_tuple("VsockRestoreAdoptionError::Contract")
                .field(error)
                .finish(),
        }
    }
}

impl<E> fmt::Display for VsockRestoreAdoptionError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Reconstruction { .. } => "vsock reconstruction failed after resource adoption",
            Self::Contract(_) => "vsock restore adoption contract failed",
        };
        write!(formatter, "{message} ({:?})", self.disposition())
    }
}

impl PreparedVsockRestoreResource {
    /// Runs exactly one reconstruction attempt and commits only a consumed resource.
    pub(crate) fn adopt<T, E>(
        self,
        reconstruct: impl FnOnce(&mut VirtioVsockReconstructionResource) -> Result<T, E>,
    ) -> Result<(T, ActiveVsockRestoreGuard), VsockRestoreAdoptionError<E>> {
        let Self {
            mut resource,
            guard,
        } = self;
        match reconstruct(&mut resource) {
            Ok(value) if resource.is_consumed() => {
                let guard = match guard {
                    VsockRestoreGuard::Direct(guard) => ActiveVsockRestoreGuard::Direct(guard),
                    VsockRestoreGuard::Contained(guard) => {
                        ActiveVsockRestoreGuard::Contained(guard)
                    }
                };
                Ok((value, guard))
            }
            Ok(_) => {
                drop(resource);
                cleanup_guard(guard).map_err(VsockRestoreAdoptionError::Contract)?;
                Err(VsockRestoreAdoptionError::Contract(
                    VsockRestoreError::terminal(
                        VsockRestoreStage::Adoption,
                        VsockRestoreErrorKind::ResourceNotConsumed,
                    ),
                ))
            }
            Err(source) => {
                let consumed = resource.is_consumed();
                drop(resource);
                let disposition = failed_adoption_disposition(guard, consumed)
                    .map_err(VsockRestoreAdoptionError::Contract)?;
                Err(VsockRestoreAdoptionError::Reconstruction {
                    source,
                    disposition,
                })
            }
        }
    }

    /// Aborts before reconstruction and verifies any retryable direct cleanup.
    pub(crate) fn abort(self) -> Result<VsockRestoreDisposition, VsockRestoreError> {
        match self.adopt::<(), ()>(|_| Err(())) {
            Err(VsockRestoreAdoptionError::Reconstruction { disposition, .. }) => Ok(disposition),
            Err(VsockRestoreAdoptionError::Contract(error)) => Err(error),
            Ok(_) => Err(VsockRestoreError::terminal(
                VsockRestoreStage::Adoption,
                VsockRestoreErrorKind::ResourceNotConsumed,
            )),
        }
    }
}

fn cleanup_guard(guard: VsockRestoreGuard) -> Result<(), VsockRestoreError> {
    match guard {
        VsockRestoreGuard::Direct(guard) => guard.cleanup().map_err(|source| {
            VsockRestoreError::terminal(
                VsockRestoreStage::Cleanup,
                VsockRestoreErrorKind::Cleanup(source),
            )
        }),
        VsockRestoreGuard::Contained(guard) => {
            drop(guard);
            Ok(())
        }
    }
}

fn failed_adoption_disposition(
    guard: VsockRestoreGuard,
    resource_consumed: bool,
) -> Result<VsockRestoreDisposition, VsockRestoreError> {
    match guard {
        VsockRestoreGuard::Direct(guard) => {
            guard.cleanup().map_err(|source| {
                VsockRestoreError::terminal(
                    VsockRestoreStage::Cleanup,
                    VsockRestoreErrorKind::Cleanup(source),
                )
            })?;
            Ok(if resource_consumed {
                VsockRestoreDisposition::Terminal
            } else {
                VsockRestoreDisposition::Retryable
            })
        }
        VsockRestoreGuard::Contained(guard) => {
            drop(guard);
            Ok(VsockRestoreDisposition::Terminal)
        }
    }
}

/// Host-operation-free logical request for one captured vsock endpoint.
pub(crate) struct RequestedVsockRestoreResource {
    selectors: SnapshotVsockSelectors,
    overridden: bool,
}

impl RequestedVsockRestoreResource {
    /// Accepts selectors already resolved by the checked exact-2.12 runtime
    /// topology without repeating caller override resolution.
    pub(crate) const fn from_resolved(selectors: SnapshotVsockSelectors, overridden: bool) -> Self {
        Self {
            selectors,
            overridden,
        }
    }

    /// Resolves captured and destination selectors before cancellation or authority access.
    pub(crate) fn resolve(
        captured: Option<&VsockBackendSelector>,
        requested_override: Option<&SnapshotVsockOverride>,
    ) -> Result<Option<Self>, VsockRestoreError> {
        resolve_snapshot_vsock_selectors(captured, requested_override)
            .map(|selectors| {
                selectors.map(|selectors| Self {
                    selectors,
                    overridden: requested_override.is_some(),
                })
            })
            .map_err(|source| {
                VsockRestoreError::retryable(
                    VsockRestoreStage::Selection,
                    VsockRestoreErrorKind::Selector(source),
                )
            })
    }

    pub(crate) const fn is_overridden(&self) -> bool {
        self.overridden
    }

    pub(crate) fn destination_reference(&self) -> &std::path::Path {
        self.selectors.destination().path()
    }

    /// Reserves every contained authority without publishing an endpoint.
    pub(crate) fn reserve(
        self,
        directory_authority: Option<&DirectoryGrantAuthority>,
        broker_authority: Option<&SocketBrokerAuthority>,
        namespace: Option<&WorkerSocketNamespace>,
        cancelled: &impl Fn() -> bool,
    ) -> Result<ReservedVsockRestoreResource, VsockRestoreError> {
        if cancelled() {
            return Err(cancelled_error());
        }
        if directory_authority.is_none() && broker_authority.is_none() && namespace.is_none() {
            if socket_directory_reference(self.destination_reference())
                .map_err(|_| contained_authority_error())?
                .is_some()
            {
                return Err(contained_authority_error());
            }
            return Ok(ReservedVsockRestoreResource::Direct(self.selectors));
        }

        let directory_authority = directory_authority.ok_or_else(contained_authority_error)?;
        let reference = self.selectors.destination().path();
        let claim = directory_authority
            .prepare_socket_directory(reference, ResourceRole::VsockSocketDirectory)
            .map_err(|_| {
                VsockRestoreError::retryable(
                    VsockRestoreStage::ContainedClaim,
                    VsockRestoreErrorKind::Grant,
                )
            })?
            .ok_or_else(|| {
                VsockRestoreError::retryable(
                    VsockRestoreStage::ContainedClaim,
                    VsockRestoreErrorKind::ContainedReferenceRequired,
                )
            })?;
        let broker_authority = broker_authority.ok_or_else(contained_authority_error)?;
        let namespace = namespace.ok_or_else(contained_authority_error)?;
        let broker = broker_authority.prepare_endpoint().map_err(|_| {
            VsockRestoreError::retryable(
                VsockRestoreStage::ContainedClaim,
                VsockRestoreErrorKind::Grant,
            )
        })?;
        let namespace = namespace.try_clone().map_err(|_| {
            VsockRestoreError::retryable(
                VsockRestoreStage::ContainedClaim,
                VsockRestoreErrorKind::Namespace,
            )
        })?;
        Ok(ReservedVsockRestoreResource::Contained(Box::new(
            ReservedContainedVsockRestoreResource {
                selectors: self.selectors,
                claim,
                broker,
                namespace,
            },
        )))
    }

    /// Accepts only the coherent facets reserved by a contained restore batch.
    pub(crate) fn reserve_contained(
        self,
        claim: PreparedSocketDirectoryClaim,
        broker: PreparedSocketBrokerEndpoint,
        namespace: WorkerSocketNamespace,
    ) -> ReservedVsockRestoreResource {
        ReservedVsockRestoreResource::Contained(Box::new(ReservedContainedVsockRestoreResource {
            selectors: self.selectors,
            claim,
            broker,
            namespace,
        }))
    }
}

impl fmt::Debug for RequestedVsockRestoreResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RequestedVsockRestoreResource")
            .field("selectors", &"<redacted>")
            .field("overridden", &self.overridden)
            .finish()
    }
}

fn cancelled_error() -> VsockRestoreError {
    VsockRestoreError::retryable(
        VsockRestoreStage::Cancellation,
        VsockRestoreErrorKind::Cancelled,
    )
}

fn contained_authority_error() -> VsockRestoreError {
    VsockRestoreError::retryable(
        VsockRestoreStage::ContainedClaim,
        VsockRestoreErrorKind::ContainedAuthorityUnavailable,
    )
}

pub(crate) struct ReservedContainedVsockRestoreResource {
    // Declaration order preserves reverse restoration during conservative
    // drop; explicit abort additionally reports incomplete restoration.
    namespace: WorkerSocketNamespace,
    broker: PreparedSocketBrokerEndpoint,
    claim: PreparedSocketDirectoryClaim,
    selectors: SnapshotVsockSelectors,
}

impl ReservedContainedVsockRestoreResource {
    fn abort(self) -> Result<(), VsockRestoreError> {
        let Self {
            selectors: _,
            claim,
            broker,
            namespace,
        } = self;
        drop(namespace);
        let cleanup_failed = broker.abort().is_err() | claim.abort().is_err();
        if cleanup_failed {
            Err(VsockRestoreError::terminal(
                VsockRestoreStage::Cleanup,
                VsockRestoreErrorKind::Grant,
            ))
        } else {
            Ok(())
        }
    }
}

/// One vsock request after reversible authority reservation.
pub(crate) enum ReservedVsockRestoreResource {
    Direct(SnapshotVsockSelectors),
    Contained(Box<ReservedContainedVsockRestoreResource>),
}

impl ReservedVsockRestoreResource {
    /// Performs rollbackable local preparation without contained publication.
    pub(crate) fn prepare_local(
        self,
        cancelled: &impl Fn() -> bool,
    ) -> Result<LocallyPreparedVsockRestoreResource, VsockRestoreError> {
        match self {
            Self::Direct(selectors) => {
                let prepared = prepare_direct_vsock_restore(selectors).map_err(|source| {
                    let kind = VsockRestoreErrorKind::Direct(source);
                    if matches!(source, DirectVsockRestoreError::Cleanup(_)) {
                        VsockRestoreError::terminal(VsockRestoreStage::Cleanup, kind)
                    } else {
                        VsockRestoreError::retryable(VsockRestoreStage::DirectPreparation, kind)
                    }
                })?;
                if cancelled() {
                    prepared.abort().map_err(|source| {
                        VsockRestoreError::terminal(
                            VsockRestoreStage::Cleanup,
                            VsockRestoreErrorKind::Cleanup(source),
                        )
                    })?;
                    return Err(cancelled_error());
                }
                let (resource, guard) = prepared.into_parts();
                Ok(LocallyPreparedVsockRestoreResource::Direct(
                    PreparedVsockRestoreResource {
                        resource,
                        guard: VsockRestoreGuard::Direct(guard),
                    },
                ))
            }
            Self::Contained(contained) => {
                if cancelled() {
                    contained.abort()?;
                    return Err(cancelled_error());
                }
                Ok(LocallyPreparedVsockRestoreResource::Contained(*contained))
            }
        }
    }

    pub(crate) fn abort(self) -> Result<VsockRestoreDisposition, VsockRestoreError> {
        match self {
            Self::Direct(_) => Ok(VsockRestoreDisposition::Retryable),
            Self::Contained(contained) => {
                contained.abort()?;
                Ok(VsockRestoreDisposition::Retryable)
            }
        }
    }
}

impl fmt::Debug for ReservedVsockRestoreResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReservedVsockRestoreResource")
            .field(
                "kind",
                &match self {
                    Self::Direct(_) => "direct",
                    Self::Contained(_) => "contained",
                },
            )
            .field("state", &"<reserved>")
            .finish()
    }
}

/// One vsock request after rollbackable local preparation.
pub(crate) enum LocallyPreparedVsockRestoreResource {
    Direct(PreparedVsockRestoreResource),
    Contained(ReservedContainedVsockRestoreResource),
}

impl LocallyPreparedVsockRestoreResource {
    /// Crosses the externally visible contained publication boundary last.
    pub(crate) fn publish(
        self,
        cancelled: &impl Fn() -> bool,
    ) -> Result<PreparedVsockRestoreResource, VsockRestoreError> {
        match self {
            Self::Direct(prepared) => Ok(prepared),
            Self::Contained(contained) => {
                if cancelled() {
                    contained.abort()?;
                    return Err(cancelled_error());
                }
                let ReservedContainedVsockRestoreResource {
                    selectors,
                    claim,
                    broker,
                    namespace,
                } = contained;
                let socket =
                    bind_prepared_vsock(namespace, claim, broker, cancelled).map_err(|source| {
                        let kind = VsockRestoreErrorKind::Anchored(source);
                        if matches!(
                            source,
                            AnchoredSocketError::Broker | AnchoredSocketError::Cleanup
                        ) {
                            VsockRestoreError::terminal(
                                VsockRestoreStage::ContainedPublication,
                                kind,
                            )
                        } else if source == AnchoredSocketError::Cancelled {
                            VsockRestoreError::retryable(VsockRestoreStage::Cancellation, kind)
                        } else {
                            VsockRestoreError::retryable(
                                VsockRestoreStage::ContainedPublication,
                                kind,
                            )
                        }
                    })?;
                let (listener, guard, connector) = socket.into_vsock_parts().map_err(|_| {
                    VsockRestoreError::terminal(
                        VsockRestoreStage::ContainedPublication,
                        VsockRestoreErrorKind::ContainedResourceIncomplete,
                    )
                })?;
                let (captured_selector, destination_selector) = selectors.into_parts();
                let resource = VirtioVsockReconstructionResource::with_destination_selector(
                    captured_selector,
                    destination_selector,
                    SuppliedVsockListener::new(listener).with_guest_connector(connector),
                );
                Ok(PreparedVsockRestoreResource {
                    resource,
                    guard: VsockRestoreGuard::Contained(guard),
                })
            }
        }
    }

    pub(crate) fn abort(self) -> Result<VsockRestoreDisposition, VsockRestoreError> {
        match self {
            Self::Direct(prepared) => prepared.abort(),
            Self::Contained(contained) => {
                contained.abort()?;
                Ok(VsockRestoreDisposition::Retryable)
            }
        }
    }
}

impl fmt::Debug for LocallyPreparedVsockRestoreResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocallyPreparedVsockRestoreResource")
            .field(
                "kind",
                &match self {
                    Self::Direct(_) => "direct",
                    Self::Contained(_) => "contained",
                },
            )
            .field("state", &"<owned>")
            .finish()
    }
}

/// Resolves intent before any authority access, then prepares one destination.
pub(crate) fn prepare_vsock_restore_resource(
    captured: Option<&VsockBackendSelector>,
    requested_override: Option<&SnapshotVsockOverride>,
    directory_authority: Option<&DirectoryGrantAuthority>,
    broker_authority: Option<&SocketBrokerAuthority>,
    namespace: Option<&WorkerSocketNamespace>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<PreparedVsockRestoreResource>, VsockRestoreError> {
    let Some(requested) = RequestedVsockRestoreResource::resolve(captured, requested_override)?
    else {
        return Ok(None);
    };
    let reserved =
        requested.reserve(directory_authority, broker_authority, namespace, &cancelled)?;
    let local = reserved.prepare_local(&cancelled)?;
    local.publish(&cancelled).map(Some)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use bangbang_runtime::memory::{
        GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange,
    };
    use bangbang_runtime::virtio_mmio::VirtioMmioRegisterHandler;
    use bangbang_runtime::vsock::{
        PreparedVsockDevice, VIRTIO_VSOCK_DEVICE_ID, VIRTIO_VSOCK_QUEUE_SIZES,
        VirtioVsockTransportResetAttempt, VsockConfigInput,
    };

    use crate::contained_session::vsock_directory_authority_for_test;

    use super::*;

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    fn test_path(name: &str) -> PathBuf {
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        PathBuf::from("/tmp").join(format!(
            "bb-process-vsr-{name}-{}-{id}.sock",
            std::process::id()
        ))
    }

    fn selector(path: &Path) -> VsockBackendSelector {
        VsockBackendSelector::try_from_path(path).expect("test selector should validate")
    }

    #[test]
    fn override_without_device_fails_before_cancellation_or_resource_access() {
        let (authority, _directory) = vsock_directory_authority_for_test();
        let secret = PathBuf::from("bangbang-grant:vsock-directory/private-secret.sock");
        let cancellation_checks = Cell::new(0);
        let error = prepare_vsock_restore_resource(
            None,
            Some(&SnapshotVsockOverride::new(&secret)),
            Some(&authority),
            None,
            None,
            || {
                cancellation_checks.set(cancellation_checks.get() + 1);
                false
            },
        )
        .expect_err("override without captured device should fail");

        assert_eq!(error.stage(), VsockRestoreStage::Selection);
        assert_eq!(
            error.kind(),
            VsockRestoreErrorKind::Selector(SnapshotVsockSelectorError::OverrideWithoutDevice)
        );
        assert_eq!(error.disposition(), VsockRestoreDisposition::Retryable);
        assert_eq!(cancellation_checks.get(), 0);
        assert!(!secret.exists());
        let untouched = authority
            .prepare_socket_directory(&secret, ResourceRole::VsockSocketDirectory)
            .expect("selector failure must leave the grant registry accessible")
            .expect("selector failure must leave the exact grant unconsumed");
        drop(untouched);
        let diagnostic = format!("{error:?} {error}");
        assert!(!diagnostic.contains(secret.to_string_lossy().as_ref()));
    }

    #[test]
    fn direct_restore_uses_override_and_aborts_with_verified_retryable_cleanup() {
        let captured_path = test_path("captured");
        let destination = test_path("destination");
        let captured = selector(&captured_path);
        let prepared = prepare_vsock_restore_resource(
            Some(&captured),
            Some(&SnapshotVsockOverride::new(&destination)),
            None,
            None,
            None,
            || false,
        )
        .expect("direct destination should prepare")
        .expect("captured device should produce a resource");

        assert!(!captured_path.exists());
        assert!(destination.exists());
        let diagnostic = format!("{prepared:?}");
        assert!(!diagnostic.contains(captured_path.to_string_lossy().as_ref()));
        assert!(!diagnostic.contains(destination.to_string_lossy().as_ref()));
        assert_eq!(
            prepared.abort().expect("direct abort should clean"),
            VsockRestoreDisposition::Retryable
        );
        assert!(!destination.exists());
    }

    #[test]
    fn direct_cancellation_after_publication_cleans_before_retry() {
        let destination = test_path("cancelled");
        let captured = selector(&destination);
        let cancellation_checks = Cell::new(0);
        let error = prepare_vsock_restore_resource(Some(&captured), None, None, None, None, || {
            let current = cancellation_checks.get();
            cancellation_checks.set(current + 1);
            current > 0
        })
        .expect_err("second cancellation check should abort direct preparation");

        assert_eq!(error.stage(), VsockRestoreStage::Cancellation);
        assert_eq!(error.kind(), VsockRestoreErrorKind::Cancelled);
        assert_eq!(error.disposition(), VsockRestoreDisposition::Retryable);
        assert_eq!(cancellation_checks.get(), 2);
        assert!(!destination.exists());
    }

    #[test]
    fn direct_adoption_failure_is_retryable_only_while_resource_is_unconsumed() {
        let destination = test_path("adoption-failure");
        let captured = selector(&destination);
        let prepared =
            prepare_vsock_restore_resource(Some(&captured), None, None, None, None, || false)
                .expect("direct resource should prepare")
                .expect("captured device should produce a resource");
        let private_source = "private reconstruction detail";
        let error = prepared
            .adopt::<(), _>(|resource| {
                assert!(!resource.is_consumed());
                Err(private_source)
            })
            .expect_err("reconstruction failure should abort the resource");

        assert_eq!(error.disposition(), VsockRestoreDisposition::Retryable);
        assert!(matches!(
            error,
            VsockRestoreAdoptionError::Reconstruction {
                source,
                disposition: VsockRestoreDisposition::Retryable,
            } if source == private_source
        ));
        assert!(!format!("{error:?} {error}").contains(private_source));
        assert!(!destination.exists());
    }

    #[test]
    fn adoption_success_without_consumption_is_terminal_and_cleans() {
        let destination = test_path("not-consumed");
        let captured = selector(&destination);
        let prepared =
            prepare_vsock_restore_resource(Some(&captured), None, None, None, None, || false)
                .expect("direct resource should prepare")
                .expect("captured device should produce a resource");
        let error = prepared
            .adopt::<(), ()>(|_| Ok(()))
            .expect_err("successful reconstruction must consume the resource");

        assert_eq!(error.disposition(), VsockRestoreDisposition::Terminal);
        assert!(matches!(
            error,
            VsockRestoreAdoptionError::Contract(VsockRestoreError {
                stage: VsockRestoreStage::Adoption,
                kind: VsockRestoreErrorKind::ResourceNotConsumed,
                disposition: VsockRestoreDisposition::Terminal,
            })
        ));
        assert!(!destination.exists());
    }

    #[test]
    fn direct_adoption_commits_consumed_resource_and_retains_cleanup_guard() {
        let captured_path = test_path("adopt-source");
        let destination = test_path("adopt-target");
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
        let prepared = prepare_vsock_restore_resource(
            Some(captured.device().backend_selector()),
            Some(&SnapshotVsockOverride::new(&destination)),
            None,
            None,
            None,
            || false,
        )
        .expect("direct resource should prepare")
        .expect("captured device should produce a resource");

        let (reconstructed, active_guard) = prepared
            .adopt(|resource| captured.reconstruct_snapshot_device(&memory, resource))
            .expect("one reconstruction should consume and commit the resource");
        assert_eq!(reconstructed.uds_path(), destination);
        assert!(destination.exists());
        assert!(!format!("{active_guard:?}").contains(destination.to_string_lossy().as_ref()));
        drop(reconstructed);
        assert!(
            destination.exists(),
            "active guard must retain the socket name"
        );
        drop(active_guard);
        assert!(!destination.exists());

        let duplicate_destination = test_path("adopt-duplicate");
        let duplicate = prepare_vsock_restore_resource(
            Some(captured.device().backend_selector()),
            Some(&SnapshotVsockOverride::new(&duplicate_destination)),
            None,
            None,
            None,
            || false,
        )
        .expect("second direct resource should prepare")
        .expect("captured device should produce a resource");
        let duplicate_error = duplicate
            .adopt(|resource| {
                drop(captured.reconstruct_snapshot_device(&memory, resource)?);
                captured.reconstruct_snapshot_device(&memory, resource)
            })
            .expect_err("a duplicate reconstruction adoption must fail");
        assert_eq!(
            duplicate_error.disposition(),
            VsockRestoreDisposition::Terminal
        );
        assert!(matches!(
            duplicate_error,
            VsockRestoreAdoptionError::Reconstruction {
                source: bangbang_runtime::vsock::VirtioVsockReconstructionError::ResourceConsumed,
                disposition: VsockRestoreDisposition::Terminal,
            }
        ));
        assert!(!duplicate_destination.exists());

        drop(source);
        assert!(!captured_path.exists());
    }

    #[test]
    fn contained_mode_requires_exact_reference_and_restores_failed_claim() {
        let (authority, _directory) = vsock_directory_authority_for_test();
        let ordinary = selector(Path::new("ordinary-vsock.sock"));
        let ordinary_error = prepare_vsock_restore_resource(
            Some(&ordinary),
            None,
            Some(&authority),
            None,
            None,
            || false,
        )
        .expect_err("contained mode must not use an ambient path");
        assert_eq!(
            ordinary_error.kind(),
            VsockRestoreErrorKind::ContainedReferenceRequired
        );

        let reference = selector(Path::new("bangbang-grant:vsock-directory/restored.sock"));
        for _ in 0..2 {
            let error = prepare_vsock_restore_resource(
                Some(&reference),
                None,
                Some(&authority),
                None,
                None,
                || false,
            )
            .expect_err("missing broker must fail before activation");
            assert_eq!(
                error.kind(),
                VsockRestoreErrorKind::ContainedAuthorityUnavailable
            );
            assert_eq!(error.disposition(), VsockRestoreDisposition::Retryable);
        }
    }
}
