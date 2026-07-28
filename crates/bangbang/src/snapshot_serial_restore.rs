//! Fresh destination serial endpoints for native-v2 snapshot restore.

use std::convert::Infallible;
use std::fmt;

use bangbang_runtime::serial::{
    SerialMmioDevice, SerialStdio, SerialStdioError, SerialStdioInput, SerialStdioRestoration,
    SerialStdioRestorationError, SharedSerialOutput,
};
use bangbang_runtime::snapshot_device_v2_6::SnapshotV2StorageDeviceGraph;
use bangbang_runtime::snapshot_restore::{
    NATIVE_V2_SERIAL_RESTORE_PUBLIC_ID, SnapshotRestoreResourceClass,
};
use bangbang_runtime::snapshot_serial_v2_7::{
    SnapshotV2SerialEndpointIntent, SnapshotV2SerialState,
};

use crate::contained_session::ContainedSnapshotRestoreAuthority;
use crate::snapshot_restore_resources::{
    PreparedSnapshotBlockRestoreBacking, PreparedSnapshotDriveRestoreCompletion,
    PreparedSnapshotDriveRestoreCompletionError, PreparedSnapshotPmemRestoreBacking,
    PreparedSnapshotSerialRestoreBatch, PreparedSnapshotSerialRestoreOutput,
    RequestedSnapshotRestoreResources, SnapshotRestoreResourceDisposition,
    SnapshotRestoreResourceError,
};

/// Complete pathless owners required to install one restored serial device.
pub(crate) struct PreparedSnapshotV2SerialRestoreOwners {
    blocks: Vec<PreparedSnapshotBlockRestoreBacking>,
    pmems: Vec<PreparedSnapshotPmemRestoreBacking>,
    serial: Option<SerialMmioDevice<SharedSerialOutput>>,
    input: Option<SerialStdioInput>,
    restoration: Option<SerialStdioRestoration>,
}

pub(crate) type PreparedSnapshotV2SerialRestoreOwnerParts = (
    Vec<PreparedSnapshotBlockRestoreBacking>,
    Vec<PreparedSnapshotPmemRestoreBacking>,
    Option<SerialMmioDevice<SharedSerialOutput>>,
    Option<SerialStdioInput>,
    Option<SerialStdioRestoration>,
);

impl PreparedSnapshotV2SerialRestoreOwners {
    pub(crate) fn block_count(&self) -> usize {
        self.blocks.len()
    }

    pub(crate) fn pmem_count(&self) -> usize {
        self.pmems.len()
    }

    pub(crate) fn into_parts(mut self) -> PreparedSnapshotV2SerialRestoreOwnerParts {
        (
            std::mem::take(&mut self.blocks),
            std::mem::take(&mut self.pmems),
            self.serial.take(),
            self.input.take(),
            self.restoration.take(),
        )
    }

    pub(crate) fn abort(mut self) -> Result<(), PreparedSnapshotV2SerialRestoreOwnerCleanupError> {
        match self.cleanup() {
            Some(source) => Err(PreparedSnapshotV2SerialRestoreOwnerCleanupError { source }),
            None => Ok(()),
        }
    }

    fn cleanup(&mut self) -> Option<SerialStdioRestorationError> {
        {
            let _input = self.input.take();
        }
        {
            let _serial = self.serial.take();
        }
        let restoration = self
            .restoration
            .take()
            .and_then(|restoration| restoration.finish().err());
        release_restore_backings(&mut self.blocks, &mut self.pmems);
        restoration
    }
}

impl Drop for PreparedSnapshotV2SerialRestoreOwners {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

impl fmt::Debug for PreparedSnapshotV2SerialRestoreOwners {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2SerialRestoreOwners")
            .field("block_count", &self.blocks.len())
            .field("pmem_count", &self.pmems.len())
            .field("has_serial", &self.serial.is_some())
            .field("has_input", &self.input.is_some())
            .field("state", &"<redacted>")
            .finish()
    }
}

#[derive(Debug)]
pub(crate) struct PreparedSnapshotV2SerialRestoreOwnerCleanupError {
    source: SerialStdioRestorationError,
}

impl fmt::Display for PreparedSnapshotV2SerialRestoreOwnerCleanupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("snapshot serial endpoint cleanup failed")
    }
}

impl std::error::Error for PreparedSnapshotV2SerialRestoreOwnerCleanupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Provisional fresh serial endpoints and their aggregate restore authority.
pub(crate) struct PreparedSnapshotV2SerialRestoreBundle {
    owners: Option<PreparedSnapshotV2SerialRestoreOwners>,
    completion: Option<PreparedSnapshotDriveRestoreCompletion>,
}

impl PreparedSnapshotV2SerialRestoreBundle {
    pub(crate) const fn owners(&self) -> Option<&PreparedSnapshotV2SerialRestoreOwners> {
        self.owners.as_ref()
    }

    pub(crate) fn construct_destination<D, E>(
        mut self,
        construct: impl FnOnce(
            PreparedSnapshotV2SerialRestoreOwners,
        ) -> Result<D, (Box<PreparedSnapshotV2SerialRestoreOwners>, E)>,
    ) -> Result<
        PreparedSnapshotV2SerialDestination<D>,
        PreparedSnapshotV2SerialDestinationConstructionError<E>,
    > {
        let Some(owners) = self.owners.take() else {
            return Err(
                PreparedSnapshotV2SerialDestinationConstructionError::InvalidState {
                    owners_cleanup: None,
                    completion_abort: self
                        .completion
                        .take()
                        .and_then(|completion| completion.abort().err()),
                },
            );
        };
        let Some(completion) = self.completion.take() else {
            return Err(
                PreparedSnapshotV2SerialDestinationConstructionError::InvalidState {
                    owners_cleanup: owners.abort().err(),
                    completion_abort: None,
                },
            );
        };
        match construct(owners) {
            Ok(destination) => Ok(PreparedSnapshotV2SerialDestination {
                destination: Some(destination),
                completion: Some(completion),
            }),
            Err((owners, source)) => {
                let owners_cleanup = (*owners).abort().err();
                let completion_abort = completion.abort().err();
                Err(
                    PreparedSnapshotV2SerialDestinationConstructionError::Construction {
                        source,
                        owners_cleanup,
                        completion_abort,
                    },
                )
            }
        }
    }

    pub(crate) fn abort(mut self) -> Result<(), PreparedSnapshotV2SerialRestoreAbortError> {
        let owners = self.owners.take().and_then(|owners| owners.abort().err());
        let completion = self
            .completion
            .take()
            .and_then(|completion| completion.abort().err());
        if owners.is_some() || completion.is_some() {
            Err(PreparedSnapshotV2SerialRestoreAbortError { owners, completion })
        } else {
            Ok(())
        }
    }
}

impl Drop for PreparedSnapshotV2SerialRestoreBundle {
    fn drop(&mut self) {
        if let Some(owners) = self.owners.take() {
            let _ = owners.abort();
        }
        if let Some(completion) = self.completion.take() {
            let _ = completion.abort();
        }
    }
}

impl fmt::Debug for PreparedSnapshotV2SerialRestoreBundle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2SerialRestoreBundle")
            .field(
                "block_count",
                &self.owners.as_ref().map_or(0, |owners| owners.blocks.len()),
            )
            .field(
                "pmem_count",
                &self.owners.as_ref().map_or(0, |owners| owners.pmems.len()),
            )
            .field(
                "has_input",
                &self
                    .owners
                    .as_ref()
                    .is_some_and(|owners| owners.input.is_some()),
            )
            .field("state", &"<private-provisional>")
            .finish()
    }
}

/// One destination whose backing authority remains provisional until commit.
pub(crate) struct PreparedSnapshotV2SerialDestination<D> {
    destination: Option<D>,
    completion: Option<PreparedSnapshotDriveRestoreCompletion>,
}

impl<D> PreparedSnapshotV2SerialDestination<D> {
    pub(crate) fn commit<C, E, T>(
        mut self,
        prepare_controller: impl FnOnce(D) -> Result<(D, C), (Box<D>, E)>,
        destroy_destination: impl FnOnce(D) -> Result<(), T>,
    ) -> Result<(D, C), PreparedSnapshotV2SerialDestinationCommitError<E, T>> {
        let destination = self
            .destination
            .take()
            .ok_or(PreparedSnapshotV2SerialDestinationCommitError::InvalidState)?;
        let completion = self
            .completion
            .take()
            .ok_or(PreparedSnapshotV2SerialDestinationCommitError::InvalidState)?;
        let (destination, controller) = match prepare_controller(destination) {
            Ok(prepared) => prepared,
            Err((destination, source)) => {
                let destination_cleanup = destroy_destination(*destination).err();
                let completion_abort = completion.abort().err();
                return Err(PreparedSnapshotV2SerialDestinationCommitError::Controller {
                    source,
                    destination_cleanup,
                    completion_abort,
                });
            }
        };
        match completion.commit() {
            Ok(()) => Ok((destination, controller)),
            Err(source) => {
                let destination_cleanup = destroy_destination(destination).err();
                Err(PreparedSnapshotV2SerialDestinationCommitError::Completion {
                    source,
                    destination_cleanup,
                })
            }
        }
    }

    pub(crate) fn abort<T>(
        mut self,
        destroy_destination: impl FnOnce(D) -> Result<(), T>,
    ) -> Result<(), PreparedSnapshotV2SerialDestinationAbortError<T>> {
        let destination = self
            .destination
            .take()
            .and_then(|destination| destroy_destination(destination).err());
        let completion = self
            .completion
            .take()
            .and_then(|completion| completion.abort().err());
        match (destination, completion) {
            (None, None) => Ok(()),
            (destination, completion) => Err(PreparedSnapshotV2SerialDestinationAbortError {
                destination,
                completion,
            }),
        }
    }
}

impl<D> Drop for PreparedSnapshotV2SerialDestination<D> {
    fn drop(&mut self) {
        {
            let _destination = self.destination.take();
        }
        if let Some(completion) = self.completion.take() {
            let _ = completion.abort();
        }
    }
}

impl<D> fmt::Debug for PreparedSnapshotV2SerialDestination<D> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2SerialDestination")
            .field("state", &"<private-provisional>")
            .finish()
    }
}

pub(crate) enum PreparedSnapshotV2SerialDestinationConstructionError<E> {
    InvalidState {
        owners_cleanup: Option<PreparedSnapshotV2SerialRestoreOwnerCleanupError>,
        completion_abort: Option<PreparedSnapshotDriveRestoreCompletionError>,
    },
    Construction {
        source: E,
        owners_cleanup: Option<PreparedSnapshotV2SerialRestoreOwnerCleanupError>,
        completion_abort: Option<PreparedSnapshotDriveRestoreCompletionError>,
    },
}

impl<E> PreparedSnapshotV2SerialDestinationConstructionError<E> {
    pub(crate) const fn is_terminal(&self) -> bool {
        match self {
            Self::InvalidState { .. } => true,
            Self::Construction {
                owners_cleanup,
                completion_abort,
                ..
            } => owners_cleanup.is_some() || completion_abort.is_some(),
        }
    }
}

impl<E> fmt::Debug for PreparedSnapshotV2SerialDestinationConstructionError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::InvalidState { .. } => "invalid-state",
            Self::Construction { .. } => "construction",
        };
        formatter
            .debug_struct("PreparedSnapshotV2SerialDestinationConstructionError")
            .field("kind", &kind)
            .field("terminal", &self.is_terminal())
            .field("state", &"<redacted>")
            .finish()
    }
}

impl<E> fmt::Display for PreparedSnapshotV2SerialDestinationConstructionError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidState { .. } => {
                "snapshot serial destination construction state is invalid"
            }
            Self::Construction { .. } => "snapshot serial destination construction failed",
        })
    }
}

impl<E> std::error::Error for PreparedSnapshotV2SerialDestinationConstructionError<E>
where
    E: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidState {
                owners_cleanup,
                completion_abort,
            } => owners_cleanup
                .as_ref()
                .map(|source| source as &(dyn std::error::Error + 'static))
                .or_else(|| {
                    completion_abort
                        .as_ref()
                        .map(|source| source as &(dyn std::error::Error + 'static))
                }),
            Self::Construction { source, .. } => Some(source),
        }
    }
}

pub(crate) enum PreparedSnapshotV2SerialDestinationCommitError<E, T> {
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

impl<E, T> PreparedSnapshotV2SerialDestinationCommitError<E, T> {
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

impl<E, T> fmt::Debug for PreparedSnapshotV2SerialDestinationCommitError<E, T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::InvalidState => "invalid-state",
            Self::Controller { .. } => "controller",
            Self::Completion { .. } => "completion",
        };
        formatter
            .debug_struct("PreparedSnapshotV2SerialDestinationCommitError")
            .field("kind", &kind)
            .field("terminal", &self.is_terminal())
            .field("state", &"<redacted>")
            .finish()
    }
}

impl<E, T> fmt::Display for PreparedSnapshotV2SerialDestinationCommitError<E, T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidState => "snapshot serial destination commit state is invalid",
            Self::Controller { .. } => {
                "snapshot serial controller preparation failed before completion"
            }
            Self::Completion { .. } => {
                "snapshot serial authority completion failed after destination construction"
            }
        })
    }
}

impl<E, T> std::error::Error for PreparedSnapshotV2SerialDestinationCommitError<E, T>
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
pub(crate) struct PreparedSnapshotV2SerialDestinationAbortError<T> {
    destination: Option<T>,
    completion: Option<PreparedSnapshotDriveRestoreCompletionError>,
}

impl<T> fmt::Display for PreparedSnapshotV2SerialDestinationAbortError<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("snapshot serial destination cleanup failed")
    }
}

impl<T> std::error::Error for PreparedSnapshotV2SerialDestinationAbortError<T>
where
    T: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.destination
            .as_ref()
            .map(|source| source as &(dyn std::error::Error + 'static))
            .or_else(|| {
                self.completion
                    .as_ref()
                    .map(|source| source as &(dyn std::error::Error + 'static))
            })
    }
}

#[derive(Debug)]
pub(crate) struct PreparedSnapshotV2SerialRestoreAbortError {
    owners: Option<PreparedSnapshotV2SerialRestoreOwnerCleanupError>,
    completion: Option<PreparedSnapshotDriveRestoreCompletionError>,
}

impl fmt::Display for PreparedSnapshotV2SerialRestoreAbortError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("snapshot serial restore bundle cleanup failed")
    }
}

impl std::error::Error for PreparedSnapshotV2SerialRestoreAbortError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.owners
            .as_ref()
            .map(|source| source as &(dyn std::error::Error + 'static))
            .or_else(|| {
                self.completion
                    .as_ref()
                    .map(|source| source as &(dyn std::error::Error + 'static))
            })
    }
}

pub(crate) enum SnapshotV2SerialRestoreBundleError {
    Resources(SnapshotRestoreResourceError),
    Stdio {
        source: SerialStdioError,
        completion_abort: Option<PreparedSnapshotDriveRestoreCompletionError>,
    },
    InvalidResourceSet {
        owners_cleanup: Option<PreparedSnapshotV2SerialRestoreOwnerCleanupError>,
        completion_abort: Option<PreparedSnapshotDriveRestoreCompletionError>,
    },
    Cancelled {
        owners_cleanup: Option<PreparedSnapshotV2SerialRestoreOwnerCleanupError>,
        completion_abort: Option<PreparedSnapshotDriveRestoreCompletionError>,
    },
}

impl SnapshotV2SerialRestoreBundleError {
    pub(crate) const fn disposition(&self) -> SnapshotRestoreResourceDisposition {
        match self {
            Self::Resources(source) => source.disposition(),
            Self::InvalidResourceSet { .. } => SnapshotRestoreResourceDisposition::Terminal,
            Self::Stdio {
                source,
                completion_abort: None,
                ..
            } if !source.cleanup_failed() => SnapshotRestoreResourceDisposition::Retryable,
            Self::Cancelled {
                owners_cleanup: None,
                completion_abort: None,
            } => SnapshotRestoreResourceDisposition::Retryable,
            Self::Stdio { .. } | Self::Cancelled { .. } => {
                SnapshotRestoreResourceDisposition::Terminal
            }
        }
    }
}

impl fmt::Debug for SnapshotV2SerialRestoreBundleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::Resources(_) => "resources",
            Self::Stdio { .. } => "stdio",
            Self::InvalidResourceSet { .. } => "invalid-resource-set",
            Self::Cancelled { .. } => "cancelled",
        };
        formatter
            .debug_struct("SnapshotV2SerialRestoreBundleError")
            .field("kind", &kind)
            .field("disposition", &self.disposition())
            .field("state", &"<redacted>")
            .finish()
    }
}

impl fmt::Display for SnapshotV2SerialRestoreBundleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "snapshot serial restore preparation failed ({:?})",
            self.disposition()
        )
    }
}

impl std::error::Error for SnapshotV2SerialRestoreBundleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Resources(source) => Some(source),
            Self::Stdio { source, .. } => Some(source),
            Self::InvalidResourceSet {
                owners_cleanup,
                completion_abort,
            }
            | Self::Cancelled {
                owners_cleanup,
                completion_abort,
            } => owners_cleanup
                .as_ref()
                .map(|source| source as &(dyn std::error::Error + 'static))
                .or_else(|| {
                    completion_abort
                        .as_ref()
                        .map(|source| source as &(dyn std::error::Error + 'static))
                }),
        }
    }
}

/// Prepares fresh destination endpoints from one complete exact-2.7 value.
pub(crate) fn prepare_native_v2_serial_restore_bundle<F>(
    graph: Option<&SnapshotV2StorageDeviceGraph>,
    state: SnapshotV2SerialState,
    authority: Option<&ContainedSnapshotRestoreAuthority>,
    cancelled: F,
) -> Result<PreparedSnapshotV2SerialRestoreBundle, SnapshotV2SerialRestoreBundleError>
where
    F: Fn() -> bool,
{
    let _owners_parts = PreparedSnapshotV2SerialRestoreOwners::into_parts;
    let _block_count = PreparedSnapshotV2SerialRestoreOwners::block_count;
    let _pmem_count = PreparedSnapshotV2SerialRestoreOwners::pmem_count;
    let _owners = PreparedSnapshotV2SerialRestoreBundle::owners;
    let _construct = |prepared: PreparedSnapshotV2SerialRestoreBundle| {
        prepared.construct_destination(|owners| {
            Ok::<_, (Box<PreparedSnapshotV2SerialRestoreOwners>, Infallible)>(owners)
        })
    };
    let _commit = |destination: PreparedSnapshotV2SerialDestination<
        PreparedSnapshotV2SerialRestoreOwners,
    >| {
        destination.commit(
            |owners| {
                Ok::<_, (Box<PreparedSnapshotV2SerialRestoreOwners>, Infallible)>((owners, ()))
            },
            PreparedSnapshotV2SerialRestoreOwners::abort,
        )
    };
    let _destination_abort =
        |destination: PreparedSnapshotV2SerialDestination<
            PreparedSnapshotV2SerialRestoreOwners,
        >| { destination.abort(PreparedSnapshotV2SerialRestoreOwners::abort) };
    let _bundle_abort = PreparedSnapshotV2SerialRestoreBundle::abort;

    let batch = RequestedSnapshotRestoreResources::prepare_native_v2_serial_state(
        graph, &state, authority, &cancelled,
    )
    .map_err(SnapshotV2SerialRestoreBundleError::Resources)?;
    prepare_native_v2_serial_restore_bundle_with(state, batch, &cancelled, || {
        SerialStdio::from_process_standard_streams()
    })
}

fn prepare_native_v2_serial_restore_bundle_with<F, S>(
    state: SnapshotV2SerialState,
    batch: PreparedSnapshotSerialRestoreBatch,
    cancelled: &F,
    prepare_stdio: S,
) -> Result<PreparedSnapshotV2SerialRestoreBundle, SnapshotV2SerialRestoreBundleError>
where
    F: Fn() -> bool,
    S: FnOnce() -> Result<SerialStdio, SerialStdioError>,
{
    let (blocks, pmems, mut prepared_serial, completion) = batch.into_parts();
    if cancelled() {
        let completion_abort = abort_unassembled(blocks, pmems, prepared_serial, completion);
        return Err(SnapshotV2SerialRestoreBundleError::Cancelled {
            owners_cleanup: None,
            completion_abort,
        });
    }

    let (endpoint_intent, rate_limiter, device_state) = state.into_parts();
    let (output, input, restoration) = match endpoint_intent {
        SnapshotV2SerialEndpointIntent::DefaultProcessStdio => {
            if prepared_serial.is_some() {
                let completion_abort =
                    abort_unassembled(blocks, pmems, prepared_serial, completion);
                return Err(SnapshotV2SerialRestoreBundleError::InvalidResourceSet {
                    owners_cleanup: None,
                    completion_abort,
                });
            }
            let stdio = match prepare_stdio() {
                Ok(stdio) => stdio,
                Err(source) => {
                    let completion_abort =
                        abort_unassembled(blocks, pmems, prepared_serial, completion);
                    return Err(SnapshotV2SerialRestoreBundleError::Stdio {
                        source,
                        completion_abort,
                    });
                }
            };
            let (output, input, restoration) = stdio.into_restorable_parts();
            (
                SharedSerialOutput::with_rate_limiter(output, rate_limiter),
                input,
                Some(restoration),
            )
        }
        SnapshotV2SerialEndpointIntent::ConfiguredOutput { selector: _ } => {
            let Some(serial) = prepared_serial.as_ref() else {
                let completion_abort =
                    abort_unassembled(blocks, pmems, prepared_serial, completion);
                return Err(SnapshotV2SerialRestoreBundleError::InvalidResourceSet {
                    owners_cleanup: None,
                    completion_abort,
                });
            };
            let key = serial.key();
            if key.resource_class() != SnapshotRestoreResourceClass::SerialSink
                || key.device_key().kind() != 3
                || key.device_key().instance() != 0
                || key.public_id().as_str() != NATIVE_V2_SERIAL_RESTORE_PUBLIC_ID
            {
                let completion_abort =
                    abort_unassembled(blocks, pmems, prepared_serial, completion);
                return Err(SnapshotV2SerialRestoreBundleError::InvalidResourceSet {
                    owners_cleanup: None,
                    completion_abort,
                });
            }
            let Some(serial) = prepared_serial.take() else {
                let completion_abort =
                    abort_unassembled(blocks, pmems, prepared_serial, completion);
                return Err(SnapshotV2SerialRestoreBundleError::InvalidResourceSet {
                    owners_cleanup: None,
                    completion_abort,
                });
            };
            let (_, output) = serial.into_parts();
            (
                SharedSerialOutput::with_rate_limiter(output, rate_limiter),
                None,
                None,
            )
        }
    };
    let serial = SerialMmioDevice::from_capture_state_with_shared_output(output, device_state);
    let owners = PreparedSnapshotV2SerialRestoreOwners {
        blocks,
        pmems,
        serial: Some(serial),
        input,
        restoration,
    };
    if cancelled() {
        let owners_cleanup = owners.abort().err();
        let completion_abort = completion.abort().err();
        return Err(SnapshotV2SerialRestoreBundleError::Cancelled {
            owners_cleanup,
            completion_abort,
        });
    }
    Ok(PreparedSnapshotV2SerialRestoreBundle {
        owners: Some(owners),
        completion: Some(completion),
    })
}

fn abort_unassembled(
    mut blocks: Vec<PreparedSnapshotBlockRestoreBacking>,
    mut pmems: Vec<PreparedSnapshotPmemRestoreBacking>,
    serial: Option<PreparedSnapshotSerialRestoreOutput>,
    completion: PreparedSnapshotDriveRestoreCompletion,
) -> Option<PreparedSnapshotDriveRestoreCompletionError> {
    {
        let _serial = serial;
    }
    release_restore_backings(&mut blocks, &mut pmems);
    completion.abort().err()
}

fn release_restore_backings(
    blocks: &mut Vec<PreparedSnapshotBlockRestoreBacking>,
    pmems: &mut Vec<PreparedSnapshotPmemRestoreBacking>,
) {
    while pmems.pop().is_some() {}
    while blocks.pop().is_some() {}
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::env;
    use std::fs::{self, File, OpenOptions};
    use std::io::{Read as _, Write as _};
    use std::mem::MaybeUninit;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use bangbang_runtime::serial::{
        SERIAL_INTERRUPT_IDENTIFICATION_RECEIVED_DATA_AVAILABLE, SERIAL_LINE_STATUS_DATA_READY,
        SERIAL_LINE_STATUS_DEFAULT, SERIAL_LINE_STATUS_OVERRUN_ERROR, SerialMmioCaptureState,
        SerialMmioCaptureStateParts, SerialMmioState, SerialOutput, SerialRateLimiterConfig,
    };
    use bangbang_runtime::snapshot_device_v2_6::{
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION, SnapshotV2StorageDeviceGraph,
    };
    use bangbang_runtime::snapshot_serial_v2_7::SnapshotV2SerialEndpointIntent;
    use bangbang_session::{GrantAccess, ResourceRole};

    use super::*;
    use crate::contained_session::{
        contained_restore_authority_with_grants_for_test, snapshot_storage_grant_authority_for_test,
    };

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    struct TempFile {
        path: PathBuf,
    }

    impl TempFile {
        fn new(name: &str, bytes: &[u8]) -> Self {
            let path = unique_path(name);
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .expect("temporary serial file should be created once");
            file.write_all(bytes)
                .expect("temporary serial bytes should write");
            file.sync_all()
                .expect("temporary serial file should synchronize");
            Self { path }
        }

        fn sized(path: PathBuf, len: u64) -> Self {
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .expect("temporary restore backing should be created once");
            file.set_len(len)
                .expect("temporary restore backing length should set");
            file.sync_all()
                .expect("temporary restore backing should synchronize");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    fn unique_path(name: &str) -> PathBuf {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        env::temp_dir().join(format!(
            "bangbang-serial-restore-{name}-{}-{id}",
            std::process::id()
        ))
    }

    fn unique_block_path() -> PathBuf {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        PathBuf::from(format!("/tmp/bb{id:011x}"))
    }

    fn unique_pmem_path() -> PathBuf {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        PathBuf::from(format!("/tmp/p{id:09x}"))
    }

    fn pipe_files() -> (File, File) {
        let mut descriptors = [-1; 2];
        // SAFETY: `descriptors` has space for both fresh descriptors returned
        // by a successful pipe call.
        assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
        // SAFETY: Successful pipe creation returned two fresh owned
        // descriptors.
        let reader = unsafe { File::from_raw_fd(descriptors[0]) };
        // SAFETY: Successful pipe creation returned two fresh owned
        // descriptors.
        let writer = unsafe { File::from_raw_fd(descriptors[1]) };
        (reader, writer)
    }

    fn status_flags(descriptor: libc::c_int) -> libc::c_int {
        // SAFETY: `F_GETFL` only inspects one borrowed live descriptor.
        let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
        assert!(flags >= 0);
        flags
    }

    fn terminal_attributes(descriptor: libc::c_int) -> libc::termios {
        let mut attributes = MaybeUninit::<libc::termios>::uninit();
        // SAFETY: `attributes` can receive one complete terminal state and the
        // descriptor is borrowed.
        let result = unsafe { libc::tcgetattr(descriptor, attributes.as_mut_ptr()) };
        assert_eq!(result, 0);
        // SAFETY: Successful `tcgetattr` initialized the complete value.
        unsafe { attributes.assume_init() }
    }

    fn capture_state() -> SerialMmioCaptureState {
        SerialMmioCaptureState::try_from_parts(SerialMmioCaptureStateParts {
            legacy_state: SerialMmioState::new(1, 3, 8, 0x5a, 12, 0),
            interrupt_identification: SERIAL_INTERRUPT_IDENTIFICATION_RECEIVED_DATA_AVAILABLE,
            line_status: SERIAL_LINE_STATUS_DEFAULT
                | SERIAL_LINE_STATUS_DATA_READY
                | SERIAL_LINE_STATUS_OVERRUN_ERROR,
            modem_status: 0,
            receive_bytes: b"abc".to_vec(),
            receive_interrupt_intent_pending: true,
            input_ready_intent_pending: false,
        })
        .expect("serial restore fixture should validate")
    }

    fn serial_state(
        endpoint: SnapshotV2SerialEndpointIntent,
        rate_limiter: Option<SerialRateLimiterConfig>,
    ) -> SnapshotV2SerialState {
        SnapshotV2SerialState::try_new(endpoint, rate_limiter, capture_state())
            .expect("serial restore state should validate")
    }

    fn batch(
        graph: Option<&SnapshotV2StorageDeviceGraph>,
        state: &SnapshotV2SerialState,
    ) -> PreparedSnapshotSerialRestoreBatch {
        RequestedSnapshotRestoreResources::prepare_native_v2_serial_state(
            graph,
            state,
            None,
            || false,
        )
        .expect("serial restore resources should prepare")
    }

    fn storage_graph(block_selector: &Path, pmem_selector: &Path) -> SnapshotV2StorageDeviceGraph {
        let mut bytes = include_str!(
            "../../runtime/src/snapshot_device_v2_6/fixtures/mixed-block-root-mmio.hex"
        )
        .trim()
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            u8::from_str_radix(
                std::str::from_utf8(pair).expect("fixture pair should be UTF-8"),
                16,
            )
            .expect("fixture should be hexadecimal")
        })
        .collect::<Vec<_>>();
        for (captured, replacement) in [
            (b"logical-selector-0".as_slice(), block_selector),
            (b"pmem-selector-0".as_slice(), pmem_selector),
        ] {
            let replacement = replacement
                .to_str()
                .expect("test selector should be UTF-8")
                .as_bytes();
            assert_eq!(captured.len(), replacement.len());
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
        .expect("storage fixture should decode")
    }

    #[test]
    fn default_fifo_endpoints_preserve_uart_and_start_fresh_limiter_and_metrics() {
        let captured = capture_state();
        let state = SnapshotV2SerialState::try_new(
            SnapshotV2SerialEndpointIntent::default_process_stdio(),
            Some(SerialRateLimiterConfig::new(1, None, 60_000)),
            captured.clone(),
        )
        .expect("default serial state should validate");
        let resources = batch(None, &state);
        let (input_reader, mut input_writer) = pipe_files();
        let (mut output_reader, output_writer) = pipe_files();
        let original_input_flags = status_flags(input_reader.as_raw_fd());
        let original_output_flags = status_flags(output_writer.as_raw_fd());
        let cancelled = || false;
        let mut prepared =
            prepare_native_v2_serial_restore_bundle_with(state, resources, &cancelled, || {
                SerialStdio::from_descriptors(input_reader.as_raw_fd(), output_writer.as_raw_fd())
            })
            .expect("default FIFO endpoints should prepare");

        let owners = prepared
            .owners
            .as_mut()
            .expect("prepared bundle should retain owners");
        assert_eq!(owners.block_count(), 0);
        assert_eq!(owners.pmem_count(), 0);
        assert!(owners.input.is_some());
        assert_ne!(status_flags(input_reader.as_raw_fd()) & libc::O_NONBLOCK, 0);
        assert_ne!(
            status_flags(output_writer.as_raw_fd()) & libc::O_NONBLOCK,
            0
        );
        let serial = owners.serial.as_mut().expect("UART should be prepared");
        assert_eq!(
            serial.capture_state().expect("UART should recapture"),
            captured
        );
        assert!(serial.metrics().is_empty());
        serial
            .output_mut()
            .write_byte(b'A')
            .expect("first limited byte should write");
        serial
            .output_mut()
            .write_byte(b'B')
            .expect("over-budget byte should be dropped without an endpoint error");
        assert_eq!(serial.metrics().write_count(), 1);
        assert_eq!(serial.metrics().rate_limiter_dropped_bytes(), 1);
        let mut byte = [0];
        output_reader
            .read_exact(&mut byte)
            .expect("one admitted byte should reach the fresh output");
        assert_eq!(byte, *b"A");
        input_writer
            .write_all(b"I")
            .expect("FIFO input byte should write");
        owners
            .input
            .as_mut()
            .expect("FIFO input should remain attached")
            .read(&mut byte)
            .expect("FIFO input byte should read");
        assert_eq!(byte, *b"I");

        prepared
            .abort()
            .expect("default FIFO bundle should abort cleanly");
        assert_eq!(
            status_flags(input_reader.as_raw_fd()) & (libc::O_ACCMODE | libc::O_NONBLOCK),
            original_input_flags & (libc::O_ACCMODE | libc::O_NONBLOCK)
        );
        assert_eq!(
            status_flags(output_writer.as_raw_fd()) & (libc::O_ACCMODE | libc::O_NONBLOCK),
            original_output_flags & (libc::O_ACCMODE | libc::O_NONBLOCK)
        );
    }

    #[test]
    fn default_unsupported_input_is_absent_and_drop_restores_output() {
        let state = serial_state(
            SnapshotV2SerialEndpointIntent::default_process_stdio(),
            None,
        );
        let resources = batch(None, &state);
        let unsupported_input = File::open("/dev/null").expect("null input should open");
        let (_output_reader, output_writer) = pipe_files();
        let original_output_flags = status_flags(output_writer.as_raw_fd());
        let calls = Cell::new(0);
        let cancelled = || false;
        let prepared =
            prepare_native_v2_serial_restore_bundle_with(state, resources, &cancelled, || {
                calls.set(calls.get() + 1);
                SerialStdio::from_descriptors(
                    unsupported_input.as_raw_fd(),
                    output_writer.as_raw_fd(),
                )
            })
            .expect("unsupported input should leave an output-only bundle");
        assert_eq!(calls.get(), 1);
        assert!(
            prepared
                .owners()
                .expect("owners should exist")
                .input
                .is_none()
        );

        drop(prepared);
        assert_eq!(
            status_flags(output_writer.as_raw_fd()) & (libc::O_ACCMODE | libc::O_NONBLOCK),
            original_output_flags & (libc::O_ACCMODE | libc::O_NONBLOCK)
        );
    }

    #[test]
    fn default_terminal_is_raw_until_explicit_abort_then_fully_restored() {
        let mut master_descriptor = -1;
        let mut slave_descriptor = -1;
        assert_eq!(
            // SAFETY: Both output pointers are valid and null optional settings
            // request the platform defaults.
            unsafe {
                libc::openpty(
                    &raw mut master_descriptor,
                    &raw mut slave_descriptor,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        // SAFETY: Successful `openpty` returned two fresh owned descriptors.
        let _master = unsafe { File::from_raw_fd(master_descriptor) };
        // SAFETY: Successful `openpty` returned one fresh owned descriptor.
        let slave = unsafe { File::from_raw_fd(slave_descriptor) };
        let original = terminal_attributes(slave.as_raw_fd());
        let state = serial_state(
            SnapshotV2SerialEndpointIntent::default_process_stdio(),
            None,
        );
        let resources = batch(None, &state);
        let cancelled = || false;
        let prepared =
            prepare_native_v2_serial_restore_bundle_with(state, resources, &cancelled, || {
                SerialStdio::from_descriptors(slave.as_raw_fd(), slave.as_raw_fd())
            })
            .expect("terminal endpoints should prepare");
        assert_eq!(
            terminal_attributes(slave.as_raw_fd()).c_lflag & (libc::ICANON | libc::ECHO),
            0
        );

        prepared
            .abort()
            .expect("terminal bundle should abort cleanly");
        let restored = terminal_attributes(slave.as_raw_fd());
        assert_eq!(restored.c_iflag, original.c_iflag);
        assert_eq!(restored.c_oflag, original.c_oflag);
        assert_eq!(restored.c_cflag, original.c_cflag);
        assert_eq!(
            restored.c_lflag & !libc::PENDIN,
            original.c_lflag & !libc::PENDIN
        );
        assert_eq!(restored.c_cc, original.c_cc);
        assert_eq!(restored.c_ispeed, original.c_ispeed);
        assert_eq!(restored.c_ospeed, original.c_ospeed);
    }

    #[test]
    fn configured_output_skips_stdio_and_commits_through_explicit_lifecycle() {
        let sink = TempFile::new("configured", b"seed");
        let state = serial_state(
            SnapshotV2SerialEndpointIntent::try_configured_output(
                sink.path()
                    .to_str()
                    .expect("temporary path should be UTF-8"),
            )
            .expect("configured endpoint should validate"),
            None,
        );
        let resources = batch(None, &state);
        let calls = Cell::new(0);
        let cancelled = || false;
        let prepared = prepare_native_v2_serial_restore_bundle_with(
            state,
            resources,
            &cancelled,
            || -> Result<SerialStdio, SerialStdioError> {
                calls.set(calls.get() + 1);
                panic!("configured output must not inspect process stdio")
            },
        )
        .expect("configured output should prepare");
        assert_eq!(calls.get(), 0);
        assert!(!format!("{prepared:?}").contains(sink.path().to_string_lossy().as_ref()));

        let destination = prepared
            .construct_destination(|mut owners| {
                assert!(owners.input.is_none());
                assert!(owners.restoration.is_none());
                let serial = owners.serial.as_mut().expect("UART should exist");
                assert!(serial.metrics().is_empty());
                serial
                    .output_mut()
                    .write_byte(b'!')
                    .expect("configured output should write");
                Ok::<_, (Box<PreparedSnapshotV2SerialRestoreOwners>, Infallible)>(owners)
            })
            .expect("destination should construct");
        let (owners, controller) = destination
            .commit(
                |owners| {
                    Ok::<_, (Box<PreparedSnapshotV2SerialRestoreOwners>, Infallible)>((
                        owners, "ready",
                    ))
                },
                PreparedSnapshotV2SerialRestoreOwners::abort,
            )
            .expect("destination should commit");
        assert_eq!(controller, "ready");
        owners
            .abort()
            .expect("committed configured owners should clean up");
        assert_eq!(
            fs::read(sink.path()).expect("configured output should read"),
            b"seed!"
        );
    }

    #[test]
    fn contained_configured_output_is_output_only_and_rebinds_after_abort() {
        let sink = TempFile::new("contained-configured", b"seed");
        let reference = PathBuf::from("bangbang-grant:serial0");
        let state = serial_state(
            SnapshotV2SerialEndpointIntent::try_configured_output(
                reference
                    .to_str()
                    .expect("contained reference should be UTF-8"),
            )
            .expect("contained endpoint should validate"),
            None,
        );
        let fixture = contained_restore_authority_with_grants_for_test(
            snapshot_storage_grant_authority_for_test(&[(
                "serial0",
                ResourceRole::SerialSink,
                GrantAccess::WriteOnly,
                sink.path(),
            )]),
            false,
        );

        for byte in *b"ab" {
            let mut prepared = prepare_native_v2_serial_restore_bundle(
                None,
                state.clone(),
                Some(fixture.authority()),
                || false,
            )
            .expect("contained configured output should prepare");
            let owners = prepared
                .owners
                .as_mut()
                .expect("contained owners should exist");
            assert!(owners.input.is_none());
            assert!(owners.restoration.is_none());
            owners
                .serial
                .as_mut()
                .expect("contained UART should exist")
                .output_mut()
                .write_byte(byte)
                .expect("contained output byte should write");
            prepared
                .abort()
                .expect("contained output abort should restore the claim");
        }
        assert_eq!(
            fs::read(sink.path()).expect("contained output should read"),
            b"seedab"
        );
    }

    #[test]
    fn configured_and_default_intents_reject_missing_or_extra_output() {
        let sink = TempFile::new("intent-mismatch", b"seed");
        let default = serial_state(
            SnapshotV2SerialEndpointIntent::default_process_stdio(),
            None,
        );
        let configured = serial_state(
            SnapshotV2SerialEndpointIntent::try_configured_output(
                sink.path()
                    .to_str()
                    .expect("temporary path should be UTF-8"),
            )
            .expect("configured endpoint should validate"),
            None,
        );
        let missing = batch(None, &default);
        let cancelled = || false;
        let error = prepare_native_v2_serial_restore_bundle_with(
            configured.clone(),
            missing,
            &cancelled,
            || -> Result<SerialStdio, SerialStdioError> {
                panic!("missing configured output must fail before stdio")
            },
        )
        .expect_err("configured intent should reject a missing output");
        assert!(matches!(
            error,
            SnapshotV2SerialRestoreBundleError::InvalidResourceSet { .. }
        ));
        assert_eq!(
            error.disposition(),
            SnapshotRestoreResourceDisposition::Terminal
        );

        let extra = batch(None, &configured);
        let error = prepare_native_v2_serial_restore_bundle_with(
            default,
            extra,
            &cancelled,
            || -> Result<SerialStdio, SerialStdioError> {
                panic!("extra configured output must fail before stdio")
            },
        )
        .expect_err("default intent should reject an extra output");
        assert!(matches!(
            error,
            SnapshotV2SerialRestoreBundleError::InvalidResourceSet { .. }
        ));

        let mut mismatched = batch(None, &configured);
        mismatched.corrupt_serial_resource_class_for_test();
        let error = prepare_native_v2_serial_restore_bundle_with(
            configured,
            mismatched,
            &cancelled,
            || -> Result<SerialStdio, SerialStdioError> {
                panic!("mismatched configured output must fail before stdio")
            },
        )
        .expect_err("configured intent should reject a mismatched output");
        assert!(matches!(
            error,
            SnapshotV2SerialRestoreBundleError::InvalidResourceSet { .. }
        ));
        assert_eq!(
            fs::read(sink.path()).expect("unused sink should read"),
            b"seed"
        );
    }

    #[test]
    fn stdio_failure_and_post_endpoint_cancellation_are_retryable_after_cleanup() {
        let state = serial_state(
            SnapshotV2SerialEndpointIntent::default_process_stdio(),
            None,
        );
        let resources = batch(None, &state);
        let cancelled = || false;
        let error = prepare_native_v2_serial_restore_bundle_with(
            state.clone(),
            resources,
            &cancelled,
            || Err(SerialStdioError::OutputNotWritable),
        )
        .expect_err("stdio failure should abort the resource batch");
        assert!(matches!(
            error,
            SnapshotV2SerialRestoreBundleError::Stdio { .. }
        ));
        assert_eq!(
            error.disposition(),
            SnapshotRestoreResourceDisposition::Retryable
        );

        let resources = batch(None, &state);
        let (input_reader, _input_writer) = pipe_files();
        let (_output_reader, output_writer) = pipe_files();
        let original_input_flags = status_flags(input_reader.as_raw_fd());
        let original_output_flags = status_flags(output_writer.as_raw_fd());
        let cancellation_calls = Cell::new(0);
        let cancelled = || {
            let call = cancellation_calls.get();
            cancellation_calls.set(call + 1);
            call == 1
        };
        let error =
            prepare_native_v2_serial_restore_bundle_with(state, resources, &cancelled, || {
                SerialStdio::from_descriptors(input_reader.as_raw_fd(), output_writer.as_raw_fd())
            })
            .expect_err("post-endpoint cancellation should roll back endpoints");
        assert!(matches!(
            error,
            SnapshotV2SerialRestoreBundleError::Cancelled { .. }
        ));
        assert_eq!(
            error.disposition(),
            SnapshotRestoreResourceDisposition::Retryable
        );
        assert_eq!(
            status_flags(input_reader.as_raw_fd()) & (libc::O_ACCMODE | libc::O_NONBLOCK),
            original_input_flags & (libc::O_ACCMODE | libc::O_NONBLOCK)
        );
        assert_eq!(
            status_flags(output_writer.as_raw_fd()) & (libc::O_ACCMODE | libc::O_NONBLOCK),
            original_output_flags & (libc::O_ACCMODE | libc::O_NONBLOCK)
        );
    }

    #[test]
    fn construction_failure_and_repeated_bundles_restore_shared_stdio_lifetime() {
        let (input_reader, _input_writer) = pipe_files();
        let (_output_reader, output_writer) = pipe_files();
        let original_input_flags = status_flags(input_reader.as_raw_fd());
        let original_output_flags = status_flags(output_writer.as_raw_fd());

        for iteration in 0..2 {
            let state = serial_state(
                SnapshotV2SerialEndpointIntent::default_process_stdio(),
                None,
            );
            let resources = batch(None, &state);
            let cancelled = || false;
            let prepared =
                prepare_native_v2_serial_restore_bundle_with(state, resources, &cancelled, || {
                    SerialStdio::from_descriptors(
                        input_reader.as_raw_fd(),
                        output_writer.as_raw_fd(),
                    )
                })
                .expect("repeated stdio bundle should prepare");
            assert!(
                prepared
                    .owners()
                    .and_then(|owners| owners.serial.as_ref())
                    .expect("repeated UART should exist")
                    .metrics()
                    .is_empty()
            );
            if iteration == 0 {
                let error = prepared
                    .construct_destination(
                        |owners| -> Result<
                            (),
                            (Box<PreparedSnapshotV2SerialRestoreOwners>, &'static str),
                        > {
                            Err((Box::new(owners), "construction failed"))
                        },
                    )
                    .expect_err("construction failure should return and clean owners");
                assert!(!error.is_terminal());
            } else {
                prepared
                    .abort()
                    .expect("repeated bundle should explicitly abort");
            }
            assert_eq!(
                status_flags(input_reader.as_raw_fd()) & (libc::O_ACCMODE | libc::O_NONBLOCK),
                original_input_flags & (libc::O_ACCMODE | libc::O_NONBLOCK)
            );
            assert_eq!(
                status_flags(output_writer.as_raw_fd()) & (libc::O_ACCMODE | libc::O_NONBLOCK),
                original_output_flags & (libc::O_ACCMODE | libc::O_NONBLOCK)
            );
        }
    }

    #[test]
    fn retained_output_clone_surfaces_split_lifetime_cleanup_failure() {
        let state = serial_state(
            SnapshotV2SerialEndpointIntent::default_process_stdio(),
            None,
        );
        let resources = batch(None, &state);
        let (input_reader, _input_writer) = pipe_files();
        let (_output_reader, output_writer) = pipe_files();
        let original_input_flags = status_flags(input_reader.as_raw_fd());
        let original_output_flags = status_flags(output_writer.as_raw_fd());
        let cancelled = || false;
        let prepared =
            prepare_native_v2_serial_restore_bundle_with(state, resources, &cancelled, || {
                SerialStdio::from_descriptors(input_reader.as_raw_fd(), output_writer.as_raw_fd())
            })
            .expect("split lifetime bundle should prepare");
        let retained_output = prepared
            .owners()
            .and_then(|owners| owners.serial.as_ref())
            .expect("split lifetime UART should exist")
            .output()
            .clone();

        let error = prepared
            .abort()
            .expect_err("a retained output clone must make cleanup failure explicit");
        assert_eq!(
            error.to_string(),
            "snapshot serial restore bundle cleanup failed"
        );
        assert_ne!(status_flags(input_reader.as_raw_fd()) & libc::O_NONBLOCK, 0);
        assert_ne!(
            status_flags(output_writer.as_raw_fd()) & libc::O_NONBLOCK,
            0
        );

        drop(retained_output);
        assert_eq!(
            status_flags(input_reader.as_raw_fd()) & (libc::O_ACCMODE | libc::O_NONBLOCK),
            original_input_flags & (libc::O_ACCMODE | libc::O_NONBLOCK)
        );
        assert_eq!(
            status_flags(output_writer.as_raw_fd()) & (libc::O_ACCMODE | libc::O_NONBLOCK),
            original_output_flags & (libc::O_ACCMODE | libc::O_NONBLOCK)
        );
    }

    #[test]
    fn storage_and_serial_owners_remain_one_graph_ordered_abort_lifetime() {
        let block_path = unique_block_path();
        let pmem_path = unique_pmem_path();
        let graph = storage_graph(&block_path, &pmem_path);
        let _block = TempFile::sized(block_path, graph.block_records()[0].block().backing_bytes());
        let _pmem = TempFile::sized(pmem_path, graph.pmem_records()[0].pmem().file_bytes());
        let state = serial_state(
            SnapshotV2SerialEndpointIntent::default_process_stdio(),
            None,
        );
        let resources = batch(Some(&graph), &state);
        let (input_reader, _input_writer) = pipe_files();
        let (_output_reader, output_writer) = pipe_files();
        let cancelled = || false;
        let prepared =
            prepare_native_v2_serial_restore_bundle_with(state, resources, &cancelled, || {
                SerialStdio::from_descriptors(input_reader.as_raw_fd(), output_writer.as_raw_fd())
            })
            .expect("mixed storage and serial owners should prepare");
        let owners = prepared.owners().expect("owners should remain aggregated");
        assert_eq!(owners.block_count(), graph.block_records().len());
        assert_eq!(owners.pmem_count(), graph.pmem_records().len());
        assert!(owners.serial.is_some());

        prepared
            .abort()
            .expect("aggregate storage and serial lifetime should abort");
    }
}
