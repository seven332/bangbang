#[cfg(test)]
use std::cell::RefCell;
#[cfg(test)]
use std::collections::VecDeque;
use std::ffi::CString;
use std::fs::{File, OpenOptions, Permissions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use crate::snapshot_memory_v2::{FileFacts, inspect_file, inspect_file_facts};

use super::*;

const STAGING_PREFIX: &[u8] = b".bangbang-snapshot-edit-";
const STAGING_RANDOM_BYTES: usize = 16;
const STAGING_CREATE_ATTEMPTS: usize = 16;
const CONTENT_COMPARE_BYTES: usize = 8192;

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: libc::dev_t,
    inode: libc::ino_t,
}

impl fmt::Debug for FileIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("FileIdentity(<redacted>)")
    }
}

struct SplitPath {
    parent: PathBuf,
    component: CString,
}

impl fmt::Debug for SplitPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SplitPath")
            .field("parent", &REDACTED)
            .field("component", &REDACTED)
            .finish()
    }
}

struct OpenedPath {
    role: SnapshotStateEditPathRole,
    parent: PathBuf,
    directory: File,
    directory_identity: FileIdentity,
    component: CString,
}

impl fmt::Debug for OpenedPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenedPath")
            .field("role", &self.role)
            .field("parent", &REDACTED)
            .field("component", &REDACTED)
            .field("directory_identity", &REDACTED)
            .finish()
    }
}

struct AdoptedInput {
    path: OpenedPath,
    file: File,
    identity: FileIdentity,
    facts: FileFacts,
}

impl fmt::Debug for AdoptedInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdoptedInput")
            .field("path", &self.path)
            .field("file", &REDACTED)
            .field("identity", &REDACTED)
            .field("facts", &REDACTED)
            .finish()
    }
}

struct StagingFile<'directory> {
    directory: &'directory File,
    name: CString,
    file: File,
    identity: FileIdentity,
    verified_facts: Option<FileFacts>,
    active: bool,
    cleanup_on_drop: bool,
}

impl fmt::Debug for StagingFile<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StagingFile")
            .field("name", &REDACTED)
            .field("identity", &REDACTED)
            .field("active", &self.active)
            .finish()
    }
}

impl StagingFile<'_> {
    fn cleanup(&mut self) -> Option<SnapshotStateEditCleanup> {
        if !self.active {
            return None;
        }
        self.cleanup_on_drop = false;
        let cleanup = clean_owned_entry(self.directory, &self.name, self.identity);
        self.active = false;
        Some(cleanup)
    }

    fn abandon_cleanup(&mut self) {
        self.cleanup_on_drop = false;
        self.active = false;
    }
}

impl Drop for StagingFile<'_> {
    fn drop(&mut self) {
        if self.active && self.cleanup_on_drop {
            let _ = clean_owned_entry(self.directory, &self.name, self.identity);
        }
    }
}

struct SystemEditPolicy<C> {
    is_cancelled: C,
}

impl<C> SystemEditPolicy<C>
where
    C: FnMut(SnapshotStateEditStage) -> bool,
{
    fn checkpoint(&mut self, stage: SnapshotStateEditStage) -> Result<(), SnapshotStateEditError> {
        enter_state_edit_stage(stage)
            .map_err(|kind| precommit_error(stage, SnapshotStateEditFailure::Io { kind }))?;
        if (self.is_cancelled)(stage) {
            Err(precommit_error(stage, SnapshotStateEditFailure::Cancelled))
        } else {
            Ok(())
        }
    }
}

pub(super) fn publish_edited_snapshot_state_unix_with_cancel<
    T,
    E,
    Transform,
    Encode,
    Verify,
    Cancel,
>(
    paths: &SnapshotStateEditPaths,
    transform: Transform,
    encode: Encode,
    verify: Verify,
    is_cancelled: Cancel,
) -> Result<SnapshotStateEditOutcome<T>, SnapshotStateEditTransactionError<E>>
where
    Transform: FnOnce(&[u8]) -> Result<T, E>,
    Encode: FnOnce(&T) -> Result<Vec<u8>, E>,
    Verify: FnOnce(&[u8], &T) -> Result<(), E>,
    Cancel: FnMut(SnapshotStateEditStage) -> bool,
{
    let mut policy = SystemEditPolicy { is_cancelled };
    policy.checkpoint(SnapshotStateEditStage::PlatformCheck)?;

    let input = adopt_input(paths.input(), &mut policy)?;
    let output = open_output(paths.output(), &mut policy)?;

    policy.checkpoint(SnapshotStateEditStage::AliasCheck)?;
    if same_named_path(&input.path, &output) {
        return Err(precommit_error(
            SnapshotStateEditStage::AliasCheck,
            SnapshotStateEditFailure::InputOutputAlias,
        )
        .into());
    }

    policy.checkpoint(SnapshotStateEditStage::OutputPreflight)?;
    verify_output_absent(&input, &output, SnapshotStateEditStage::OutputPreflight)?;

    policy.checkpoint(SnapshotStateEditStage::InputRead)?;
    let input_bytes = read_input(&input)?;

    policy.checkpoint(SnapshotStateEditStage::Transform)?;
    let product = transform(&input_bytes).map_err(|source| {
        SnapshotStateEditTransactionError::Operation(operation_error(
            SnapshotStateEditStage::Transform,
            source,
        ))
    })?;

    policy.checkpoint(SnapshotStateEditStage::Encode)?;
    let encoded = encode(&product).map_err(|source| {
        SnapshotStateEditTransactionError::Operation(operation_error(
            SnapshotStateEditStage::Encode,
            source,
        ))
    })?;
    if encoded.is_empty() || encoded.len() > SNAPSHOT_STATE_EDIT_MAX_FILE_BYTES {
        return Err(precommit_error(
            SnapshotStateEditStage::Encode,
            SnapshotStateEditFailure::InvalidEncodedStateLength {
                maximum: SNAPSHOT_STATE_EDIT_MAX_FILE_BYTES,
            },
        )
        .into());
    }

    policy.checkpoint(SnapshotStateEditStage::StagingCreate)?;
    let mut staging = create_staging(&output.directory)?;

    if let Err(error) =
        write_and_verify_staging(&encoded, &product, &mut staging, verify, &mut policy)
    {
        return Err(cleanup_transaction_error(&mut staging, error));
    }

    for (stage, check) in [
        (
            SnapshotStateEditStage::SourceStability,
            verify_source as fn(&AdoptedInput, &OpenedPath, &StagingFile<'_>, &[u8]) -> _,
        ),
        (
            SnapshotStateEditStage::DirectoryStability,
            verify_directories,
        ),
        (SnapshotStateEditStage::EntryStability, verify_entries),
    ] {
        if let Err(error) = policy
            .checkpoint(stage)
            .and_then(|()| check(&input, &output, &staging, &encoded))
        {
            return Err(cleanup_publication_error(&mut staging, error).into());
        }
    }

    // This is the final caller-controlled checkpoint. Every fact, directory,
    // entry, and byte check is repeated after it. No callback, allocator, or
    // test action runs between those checks and the hard-link syscall.
    if let Err(error) = policy.checkpoint(SnapshotStateEditStage::Commit) {
        return Err(cleanup_publication_error(&mut staging, error).into());
    }
    if let Err(error) = verify_source(&input, &output, &staging, &encoded)
        .and_then(|()| verify_directories(&input, &output, &staging, &encoded))
        .and_then(|()| verify_entries(&input, &output, &staging, &encoded))
    {
        return Err(cleanup_publication_error(&mut staging, error).into());
    }

    if let Err(kind) = hard_link_no_clobber(&output.directory, &staging.name, &output.component) {
        let failure = if kind == io::ErrorKind::AlreadyExists {
            SnapshotStateEditFailure::OutputAlreadyExists
        } else {
            SnapshotStateEditFailure::HardLinkUnavailable { kind }
        };
        let error = precommit_error(SnapshotStateEditStage::Commit, failure);
        return Err(cleanup_publication_error(&mut staging, error).into());
    }

    // `linkat` is the commit point. From here onward this function cannot
    // return `Err`, invoke caller/cancellation code, or remove the final name.
    Ok(finish_committed_edit(
        product,
        &input,
        &output,
        &mut staging,
    ))
}

fn adopt_input<C>(
    path: &Path,
    policy: &mut SystemEditPolicy<C>,
) -> Result<AdoptedInput, SnapshotStateEditError>
where
    C: FnMut(SnapshotStateEditStage) -> bool,
{
    policy.checkpoint(SnapshotStateEditStage::InputPathValidation)?;
    let split = split_path(path).ok_or_else(|| {
        precommit_error(
            SnapshotStateEditStage::InputPathValidation,
            SnapshotStateEditFailure::InvalidPath {
                path: SnapshotStateEditPathRole::Input,
            },
        )
    })?;

    policy.checkpoint(SnapshotStateEditStage::InputDirectoryOpen)?;
    let directory = open_directory(&split.parent).map_err(|kind| {
        precommit_error(
            SnapshotStateEditStage::InputDirectoryOpen,
            SnapshotStateEditFailure::Io { kind },
        )
    })?;
    let directory_identity = file_identity(&directory).map_err(|kind| {
        precommit_error(
            SnapshotStateEditStage::InputDirectoryOpen,
            SnapshotStateEditFailure::Io { kind },
        )
    })?;

    policy.checkpoint(SnapshotStateEditStage::InputFileOpen)?;
    let file = open_input(&directory, &split.component).map_err(|kind| {
        precommit_error(
            SnapshotStateEditStage::InputFileOpen,
            SnapshotStateEditFailure::Io { kind },
        )
    })?;
    let identity = file_identity(&file).map_err(|kind| {
        precommit_error(
            SnapshotStateEditStage::InputFileOpen,
            SnapshotStateEditFailure::Io { kind },
        )
    })?;

    policy.checkpoint(SnapshotStateEditStage::InputValidation)?;
    let facts = inspect_file(&file).map_err(|_| {
        precommit_error(
            SnapshotStateEditStage::InputValidation,
            SnapshotStateEditFailure::InvalidInput,
        )
    })?;
    if facts.length() > u64::try_from(SNAPSHOT_STATE_EDIT_MAX_FILE_BYTES).unwrap_or(u64::MAX) {
        return Err(precommit_error(
            SnapshotStateEditStage::InputValidation,
            SnapshotStateEditFailure::InputTooLarge {
                maximum: SNAPSHOT_STATE_EDIT_MAX_FILE_BYTES,
            },
        ));
    }
    if entry_identity(&directory, &split.component).map_err(|kind| {
        precommit_error(
            SnapshotStateEditStage::InputValidation,
            SnapshotStateEditFailure::Io { kind },
        )
    })? != Some(identity)
        || inspect_file(&file).ok() != Some(facts)
    {
        return Err(precommit_error(
            SnapshotStateEditStage::InputValidation,
            SnapshotStateEditFailure::SourceChanged,
        ));
    }

    Ok(AdoptedInput {
        path: OpenedPath {
            role: SnapshotStateEditPathRole::Input,
            parent: split.parent,
            directory,
            directory_identity,
            component: split.component,
        },
        file,
        identity,
        facts,
    })
}

fn open_output<C>(
    path: &Path,
    policy: &mut SystemEditPolicy<C>,
) -> Result<OpenedPath, SnapshotStateEditError>
where
    C: FnMut(SnapshotStateEditStage) -> bool,
{
    policy.checkpoint(SnapshotStateEditStage::OutputPathValidation)?;
    let split = split_path(path).ok_or_else(|| {
        precommit_error(
            SnapshotStateEditStage::OutputPathValidation,
            SnapshotStateEditFailure::InvalidPath {
                path: SnapshotStateEditPathRole::Output,
            },
        )
    })?;

    policy.checkpoint(SnapshotStateEditStage::OutputDirectoryOpen)?;
    let directory = open_directory(&split.parent).map_err(|kind| {
        precommit_error(
            SnapshotStateEditStage::OutputDirectoryOpen,
            SnapshotStateEditFailure::Io { kind },
        )
    })?;
    let directory_identity = file_identity(&directory).map_err(|kind| {
        precommit_error(
            SnapshotStateEditStage::OutputDirectoryOpen,
            SnapshotStateEditFailure::Io { kind },
        )
    })?;
    Ok(OpenedPath {
        role: SnapshotStateEditPathRole::Output,
        parent: split.parent,
        directory,
        directory_identity,
        component: split.component,
    })
}

fn same_named_path(input: &OpenedPath, output: &OpenedPath) -> bool {
    input.directory_identity == output.directory_identity
        && input.component.as_bytes() == output.component.as_bytes()
}

fn verify_output_absent(
    input: &AdoptedInput,
    output: &OpenedPath,
    stage: SnapshotStateEditStage,
) -> Result<(), SnapshotStateEditError> {
    match entry_identity(&output.directory, &output.component) {
        Ok(None) => Ok(()),
        Ok(Some(identity)) if identity == input.identity => Err(precommit_error(
            stage,
            SnapshotStateEditFailure::InputOutputAlias,
        )),
        Ok(Some(_)) => Err(precommit_error(
            stage,
            SnapshotStateEditFailure::OutputAlreadyExists,
        )),
        Err(kind) => Err(precommit_error(
            stage,
            SnapshotStateEditFailure::Io { kind },
        )),
    }
}

fn read_input(input: &AdoptedInput) -> Result<Vec<u8>, SnapshotStateEditError> {
    let stage = SnapshotStateEditStage::InputRead;
    let length = usize::try_from(input.facts.length()).map_err(|_| {
        precommit_error(
            stage,
            SnapshotStateEditFailure::InputTooLarge {
                maximum: SNAPSHOT_STATE_EDIT_MAX_FILE_BYTES,
            },
        )
    })?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|source| precommit_error(stage, SnapshotStateEditFailure::Allocation(source)))?;
    bytes.resize(length, 0);
    read_exact_at(&input.file, &mut bytes, 0).map_err(|source| {
        precommit_error(
            stage,
            SnapshotStateEditFailure::Io {
                kind: source.kind(),
            },
        )
    })?;
    let mut extra = [0_u8; 1];
    let offset = u64::try_from(length)
        .map_err(|_| precommit_error(stage, SnapshotStateEditFailure::SourceChanged))?;
    let trailing = input.file.read_at(&mut extra, offset).map_err(|source| {
        precommit_error(
            stage,
            SnapshotStateEditFailure::Io {
                kind: source.kind(),
            },
        )
    })?;
    if trailing != 0 || inspect_file(&input.file).ok() != Some(input.facts) {
        return Err(precommit_error(
            stage,
            SnapshotStateEditFailure::SourceChanged,
        ));
    }
    Ok(bytes)
}

fn create_staging(directory: &File) -> Result<StagingFile<'_>, SnapshotStateEditError> {
    let stage = SnapshotStateEditStage::StagingCreate;
    for _ in 0..STAGING_CREATE_ATTEMPTS {
        let name = staging_name()?;
        match open_staging(directory, &name) {
            Ok(file) => {
                let identity = match file_identity(&file) {
                    Ok(identity) => identity,
                    Err(kind) => {
                        let mut error =
                            precommit_error(stage, SnapshotStateEditFailure::Io { kind });
                        error.staging_cleanup = Some(SnapshotStateEditCleanup::Failed(kind));
                        return Err(error);
                    }
                };
                let mut staging = StagingFile {
                    directory,
                    name,
                    file,
                    identity,
                    verified_facts: None,
                    active: true,
                    cleanup_on_drop: true,
                };
                if let Err(source) = staging.file.set_permissions(Permissions::from_mode(0o600)) {
                    let error = precommit_error(
                        stage,
                        SnapshotStateEditFailure::Io {
                            kind: source.kind(),
                        },
                    );
                    return Err(cleanup_publication_error(&mut staging, error));
                }
                let position = staging.file.stream_position().map_err(|source| {
                    cleanup_publication_error(
                        &mut staging,
                        precommit_error(
                            stage,
                            SnapshotStateEditFailure::Io {
                                kind: source.kind(),
                            },
                        ),
                    )
                })?;
                let facts = inspect_file_facts(&staging.file).map_err(|_| {
                    cleanup_publication_error(
                        &mut staging,
                        precommit_error(stage, SnapshotStateEditFailure::InvalidStaging),
                    )
                })?;
                if !valid_staging_facts(facts, 0)
                    || position != 0
                    || file_identity(&staging.file).ok() != Some(staging.identity)
                    || entry_identity(staging.directory, &staging.name)
                        .ok()
                        .flatten()
                        != Some(staging.identity)
                {
                    let error = precommit_error(stage, SnapshotStateEditFailure::InvalidStaging);
                    return Err(cleanup_publication_error(&mut staging, error));
                }
                return Ok(staging);
            }
            Err(io::ErrorKind::AlreadyExists) => {}
            Err(kind) => {
                return Err(precommit_error(
                    stage,
                    SnapshotStateEditFailure::Io { kind },
                ));
            }
        }
    }
    Err(precommit_error(
        stage,
        SnapshotStateEditFailure::StagingNameExhausted,
    ))
}

fn write_and_verify_staging<T, E, Verify, C>(
    encoded: &[u8],
    product: &T,
    staging: &mut StagingFile<'_>,
    verify: Verify,
    policy: &mut SystemEditPolicy<C>,
) -> Result<(), SnapshotStateEditTransactionError<E>>
where
    Verify: FnOnce(&[u8], &T) -> Result<(), E>,
    C: FnMut(SnapshotStateEditStage) -> bool,
{
    policy.checkpoint(SnapshotStateEditStage::StagingWrite)?;
    staging.file.write_all(encoded).map_err(|source| {
        precommit_error(
            SnapshotStateEditStage::StagingWrite,
            SnapshotStateEditFailure::Io {
                kind: source.kind(),
            },
        )
    })?;
    let expected_length = u64::try_from(encoded.len()).map_err(|_| {
        precommit_error(
            SnapshotStateEditStage::StagingWrite,
            SnapshotStateEditFailure::InvalidEncodedStateLength {
                maximum: SNAPSHOT_STATE_EDIT_MAX_FILE_BYTES,
            },
        )
    })?;
    if staging.file.stream_position().ok() != Some(expected_length)
        || staging.file.metadata().ok().map(|metadata| metadata.len()) != Some(expected_length)
    {
        return Err(precommit_error(
            SnapshotStateEditStage::StagingWrite,
            SnapshotStateEditFailure::StagingChanged,
        )
        .into());
    }

    policy.checkpoint(SnapshotStateEditStage::StagingFlush)?;
    staging.file.flush().map_err(|source| {
        precommit_error(
            SnapshotStateEditStage::StagingFlush,
            SnapshotStateEditFailure::Io {
                kind: source.kind(),
            },
        )
    })?;

    policy.checkpoint(SnapshotStateEditStage::StagingFileSync)?;
    staging.file.sync_all().map_err(|source| {
        precommit_error(
            SnapshotStateEditStage::StagingFileSync,
            SnapshotStateEditFailure::Io {
                kind: source.kind(),
            },
        )
    })?;

    policy.checkpoint(SnapshotStateEditStage::StagingSeek)?;
    if staging.file.seek(SeekFrom::Start(0)).map_err(|source| {
        precommit_error(
            SnapshotStateEditStage::StagingSeek,
            SnapshotStateEditFailure::Io {
                kind: source.kind(),
            },
        )
    })? != 0
    {
        return Err(precommit_error(
            SnapshotStateEditStage::StagingSeek,
            SnapshotStateEditFailure::StagingChanged,
        )
        .into());
    }

    policy.checkpoint(SnapshotStateEditStage::StagingRead)?;
    let mut reread = Vec::new();
    reread.try_reserve_exact(encoded.len()).map_err(|source| {
        precommit_error(
            SnapshotStateEditStage::StagingRead,
            SnapshotStateEditFailure::Allocation(source),
        )
    })?;
    reread.resize(encoded.len(), 0);
    staging.file.read_exact(&mut reread).map_err(|source| {
        precommit_error(
            SnapshotStateEditStage::StagingRead,
            SnapshotStateEditFailure::Io {
                kind: source.kind(),
            },
        )
    })?;
    let mut extra = [0_u8; 1];
    if staging.file.read(&mut extra).map_err(|source| {
        precommit_error(
            SnapshotStateEditStage::StagingRead,
            SnapshotStateEditFailure::Io {
                kind: source.kind(),
            },
        )
    })? != 0
        || reread != encoded
    {
        return Err(precommit_error(
            SnapshotStateEditStage::StagingRead,
            SnapshotStateEditFailure::StagingContentMismatch,
        )
        .into());
    }

    policy.checkpoint(SnapshotStateEditStage::StagingVerify)?;
    verify(&reread, product).map_err(|source| {
        SnapshotStateEditTransactionError::Operation(operation_error(
            SnapshotStateEditStage::StagingVerify,
            source,
        ))
    })?;

    let facts = inspect_file_facts(&staging.file).map_err(|_| {
        precommit_error(
            SnapshotStateEditStage::StagingVerify,
            SnapshotStateEditFailure::InvalidStaging,
        )
    })?;
    if !valid_staging_facts(facts, expected_length)
        || file_identity(&staging.file).ok() != Some(staging.identity)
        || entry_identity(staging.directory, &staging.name)
            .ok()
            .flatten()
            != Some(staging.identity)
    {
        return Err(precommit_error(
            SnapshotStateEditStage::StagingVerify,
            SnapshotStateEditFailure::StagingChanged,
        )
        .into());
    }
    staging.verified_facts = Some(facts);
    Ok(())
}

fn valid_staging_facts(facts: FileFacts, expected_length: u64) -> bool {
    facts.permissions() == 0o600
        && facts.is_regular()
        && facts.is_read_write()
        && facts.is_close_on_exec()
        && !facts.is_append()
        && facts.length() == expected_length
}

fn verify_source(
    input: &AdoptedInput,
    _output: &OpenedPath,
    _staging: &StagingFile<'_>,
    _encoded: &[u8],
) -> Result<(), SnapshotStateEditError> {
    if inspect_file(&input.file).ok() == Some(input.facts)
        && file_identity(&input.file).ok() == Some(input.identity)
    {
        Ok(())
    } else {
        Err(precommit_error(
            SnapshotStateEditStage::SourceStability,
            SnapshotStateEditFailure::SourceChanged,
        ))
    }
}

fn verify_directories(
    input: &AdoptedInput,
    output: &OpenedPath,
    _staging: &StagingFile<'_>,
    _encoded: &[u8],
) -> Result<(), SnapshotStateEditError> {
    for path in [&input.path, output] {
        let retained = file_identity(&path.directory).ok();
        let reopened = open_directory(&path.parent)
            .ok()
            .and_then(|directory| file_identity(&directory).ok());
        if retained != Some(path.directory_identity) || reopened != Some(path.directory_identity) {
            return Err(precommit_error(
                SnapshotStateEditStage::DirectoryStability,
                SnapshotStateEditFailure::DirectoryChanged { path: path.role },
            ));
        }
    }
    Ok(())
}

fn verify_entries(
    input: &AdoptedInput,
    output: &OpenedPath,
    staging: &StagingFile<'_>,
    encoded: &[u8],
) -> Result<(), SnapshotStateEditError> {
    let stage = SnapshotStateEditStage::EntryStability;
    if entry_identity(&input.path.directory, &input.path.component)
        .ok()
        .flatten()
        != Some(input.identity)
    {
        return Err(precommit_error(
            stage,
            SnapshotStateEditFailure::EntryChanged {
                path: SnapshotStateEditPathRole::Input,
            },
        ));
    }
    verify_output_absent(input, output, stage)?;
    if entry_identity(staging.directory, &staging.name)
        .ok()
        .flatten()
        != Some(staging.identity)
        || file_identity(&staging.file).ok() != Some(staging.identity)
        || staging.verified_facts.is_none()
        || inspect_file_facts(&staging.file).ok() != staging.verified_facts
    {
        return Err(precommit_error(
            stage,
            SnapshotStateEditFailure::StagingChanged,
        ));
    }
    verify_staging_content(&staging.file, encoded, stage)
}

fn verify_staging_content(
    file: &File,
    expected: &[u8],
    stage: SnapshotStateEditStage,
) -> Result<(), SnapshotStateEditError> {
    let mut buffer = [0_u8; CONTENT_COMPARE_BYTES];
    let mut offset = 0_usize;
    while offset < expected.len() {
        let count = (expected.len() - offset).min(buffer.len());
        let Some(chunk) = buffer.get_mut(..count) else {
            return Err(precommit_error(
                stage,
                SnapshotStateEditFailure::StagingContentMismatch,
            ));
        };
        let file_offset = u64::try_from(offset).map_err(|_| {
            precommit_error(stage, SnapshotStateEditFailure::StagingContentMismatch)
        })?;
        read_exact_at(file, chunk, file_offset).map_err(|source| {
            precommit_error(
                stage,
                SnapshotStateEditFailure::Io {
                    kind: source.kind(),
                },
            )
        })?;
        let end = offset.checked_add(count).ok_or_else(|| {
            precommit_error(stage, SnapshotStateEditFailure::StagingContentMismatch)
        })?;
        if expected.get(offset..end) != Some(chunk) {
            return Err(precommit_error(
                stage,
                SnapshotStateEditFailure::StagingContentMismatch,
            ));
        }
        offset = end;
    }
    let mut extra = [0_u8; 1];
    let file_offset = u64::try_from(offset)
        .map_err(|_| precommit_error(stage, SnapshotStateEditFailure::StagingContentMismatch))?;
    if file.read_at(&mut extra, file_offset).map_err(|source| {
        precommit_error(
            stage,
            SnapshotStateEditFailure::Io {
                kind: source.kind(),
            },
        )
    })? != 0
    {
        return Err(precommit_error(
            stage,
            SnapshotStateEditFailure::StagingContentMismatch,
        ));
    }
    Ok(())
}

fn finish_committed_edit<T>(
    product: T,
    input: &AdoptedInput,
    output: &OpenedPath,
    staging: &mut StagingFile<'_>,
) -> SnapshotStateEditOutcome<T> {
    let mut first_uncertainty = None;

    if let Err(kind) = enter_state_edit_stage(SnapshotStateEditStage::CommitVerification) {
        record_uncertainty(
            &mut first_uncertainty,
            SnapshotStateEditStage::CommitVerification,
            SnapshotStateEditCommitFailure::Io(kind),
        );
    }
    for path in [&input.path, output] {
        let retained = file_identity(&path.directory).ok();
        let reopened = open_directory(&path.parent)
            .ok()
            .and_then(|directory| file_identity(&directory).ok());
        if retained != Some(path.directory_identity) || reopened != Some(path.directory_identity) {
            record_uncertainty(
                &mut first_uncertainty,
                SnapshotStateEditStage::CommitVerification,
                SnapshotStateEditCommitFailure::DirectoryChanged { path: path.role },
            );
        }
    }
    if inspect_file(&input.file).ok() != Some(input.facts)
        || entry_identity(&input.path.directory, &input.path.component)
            .ok()
            .flatten()
            != Some(input.identity)
    {
        record_uncertainty(
            &mut first_uncertainty,
            SnapshotStateEditStage::CommitVerification,
            SnapshotStateEditCommitFailure::InputChanged,
        );
    }
    if entry_identity(&output.directory, &output.component)
        .ok()
        .flatten()
        != Some(staging.identity)
    {
        record_uncertainty(
            &mut first_uncertainty,
            SnapshotStateEditStage::CommitVerification,
            SnapshotStateEditCommitFailure::OutputEntryChanged,
        );
    }
    if entry_identity(staging.directory, &staging.name)
        .ok()
        .flatten()
        != Some(staging.identity)
    {
        record_uncertainty(
            &mut first_uncertainty,
            SnapshotStateEditStage::CommitVerification,
            SnapshotStateEditCommitFailure::StagingEntryChanged,
        );
    }

    let staging_cleanup = match enter_state_edit_stage(SnapshotStateEditStage::StagingCleanup) {
        Ok(()) => staging
            .cleanup()
            .unwrap_or(SnapshotStateEditCleanup::AlreadyAbsent),
        Err(kind) => {
            staging.abandon_cleanup();
            SnapshotStateEditCleanup::Failed(kind)
        }
    };
    if matches!(
        staging_cleanup,
        SnapshotStateEditCleanup::ChangedRefused | SnapshotStateEditCleanup::Failed(_)
    ) {
        record_uncertainty(
            &mut first_uncertainty,
            SnapshotStateEditStage::StagingCleanup,
            SnapshotStateEditCommitFailure::Cleanup,
        );
    }

    let directory_sync = match enter_state_edit_stage(SnapshotStateEditStage::OutputDirectorySync) {
        Ok(()) => output
            .directory
            .sync_all()
            .err()
            .map(|source| source.kind()),
        Err(kind) => Some(kind),
    };
    if let Some(kind) = directory_sync {
        record_uncertainty(
            &mut first_uncertainty,
            SnapshotStateEditStage::OutputDirectorySync,
            SnapshotStateEditCommitFailure::Io(kind),
        );
    }

    let _ = enter_state_edit_stage(SnapshotStateEditStage::Complete);
    let commit = match first_uncertainty {
        None => SnapshotStateEditCommit::Durable,
        Some((stage, failure)) => SnapshotStateEditCommit::Uncertain {
            stage,
            failure,
            staging_cleanup,
            directory_sync,
        },
    };
    SnapshotStateEditOutcome { product, commit }
}

fn cleanup_transaction_error<E>(
    staging: &mut StagingFile<'_>,
    error: SnapshotStateEditTransactionError<E>,
) -> SnapshotStateEditTransactionError<E> {
    match error {
        SnapshotStateEditTransactionError::Publication(error) => {
            cleanup_publication_error(staging, error).into()
        }
        SnapshotStateEditTransactionError::Operation(mut error) => {
            error.staging_cleanup = staging.cleanup();
            SnapshotStateEditTransactionError::Operation(error)
        }
    }
}

fn cleanup_publication_error(
    staging: &mut StagingFile<'_>,
    mut error: SnapshotStateEditError,
) -> SnapshotStateEditError {
    error.staging_cleanup = staging.cleanup();
    error
}

fn record_uncertainty(
    first: &mut Option<(SnapshotStateEditStage, SnapshotStateEditCommitFailure)>,
    stage: SnapshotStateEditStage,
    failure: SnapshotStateEditCommitFailure,
) {
    if first.is_none() {
        *first = Some((stage, failure));
    }
}

fn split_path(path: &Path) -> Option<SplitPath> {
    let raw_path = path.as_os_str().as_bytes();
    if raw_path.contains(&0) {
        return None;
    }
    let raw_component = raw_path.rsplit(|byte| *byte == b'/').next()?;
    if raw_component.is_empty() || raw_component == b"." || raw_component == b".." {
        return None;
    }
    let Component::Normal(component) = path.components().next_back()? else {
        return None;
    };
    if component.as_bytes() != raw_component {
        return None;
    }
    let component = CString::new(raw_component).ok()?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    Some(SplitPath { parent, component })
}

fn open_directory(path: &Path) -> Result<File, io::ErrorKind> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY)
        .open(path)
        .map_err(|source| source.kind())
}

fn open_input(directory: &File, component: &CString) -> Result<File, io::ErrorKind> {
    // SAFETY: `directory` is live and `component` is one NUL-terminated final
    // component. No-follow/nonblocking reject final-symlink traversal and avoid
    // blocking on special files before descriptor validation. Success returns
    // one fresh descriptor owned by this function.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            component.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if descriptor < 0 {
        Err(io::Error::last_os_error().kind())
    } else {
        // SAFETY: successful `openat` returned one fresh owned descriptor.
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

fn open_staging(directory: &File, name: &CString) -> Result<File, io::ErrorKind> {
    #[cfg(test)]
    let mode = STATE_EDIT_TEST_HOOK.with(|hook| hook.borrow().staging_mode.unwrap_or(0o600));
    #[cfg(not(test))]
    let mode: libc::c_int = 0o600;
    // SAFETY: `directory` is live and `name` is one generated NUL-terminated
    // component. Exclusive no-follow creation returns one fresh descriptor;
    // the promoted mode argument has the variadic ABI's integer type.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            mode,
        )
    };
    if descriptor < 0 {
        Err(io::Error::last_os_error().kind())
    } else {
        // SAFETY: successful `openat` returned one fresh owned descriptor.
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

fn staging_name() -> Result<CString, SnapshotStateEditError> {
    let stage = SnapshotStateEditStage::StagingCreate;
    let mut random = [0_u8; STAGING_RANDOM_BYTES];
    fill_staging_random(&mut random)
        .map_err(|()| precommit_error(stage, SnapshotStateEditFailure::RandomnessUnavailable))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(STAGING_PREFIX.len() + STAGING_RANDOM_BYTES * 2)
        .map_err(|source| precommit_error(stage, SnapshotStateEditFailure::Allocation(source)))?;
    bytes.extend_from_slice(STAGING_PREFIX);
    for byte in random {
        bytes.push(hex_digit(byte >> 4));
        bytes.push(hex_digit(byte & 0x0f));
    }
    CString::new(bytes)
        .map_err(|_| precommit_error(stage, SnapshotStateEditFailure::RandomnessUnavailable))
}

fn fill_staging_random(destination: &mut [u8; STAGING_RANDOM_BYTES]) -> Result<(), ()> {
    #[cfg(test)]
    if STATE_EDIT_TEST_HOOK.with(|hook| hook.borrow().random_failure) {
        return Err(());
    }
    #[cfg(test)]
    if let Some(random) =
        STATE_EDIT_TEST_HOOK.with(|hook| hook.borrow_mut().random_names.pop_front())
    {
        *destination = random;
        return Ok(());
    }
    getrandom::fill(destination).map_err(|_| ())
}

const fn hex_digit(nibble: u8) -> u8 {
    match nibble {
        0..=9 => b'0' + nibble,
        _ => b'a' + (nibble - 10),
    }
}

fn file_identity(file: &File) -> Result<FileIdentity, io::ErrorKind> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `file` owns a live descriptor and `stat` points to writable
    // storage for the complete result.
    if unsafe { libc::fstat(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error().kind());
    }
    // SAFETY: successful `fstat` initialized the complete structure.
    let stat = unsafe { stat.assume_init() };
    Ok(FileIdentity {
        device: stat.st_dev,
        inode: stat.st_ino,
    })
}

fn entry_identity(
    directory: &File,
    component: &CString,
) -> Result<Option<FileIdentity>, io::ErrorKind> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `directory` is live, `component` is NUL-terminated, and `stat`
    // points to writable storage. No-follow inspects the final entry itself.
    let result = unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            component.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        // SAFETY: successful `fstatat` initialized the complete structure.
        let stat = unsafe { stat.assume_init() };
        return Ok(Some(FileIdentity {
            device: stat.st_dev,
            inode: stat.st_ino,
        }));
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::NotFound {
        Ok(None)
    } else {
        Err(error.kind())
    }
}

fn hard_link_no_clobber(
    directory: &File,
    staging: &CString,
    output: &CString,
) -> Result<(), io::ErrorKind> {
    #[cfg(test)]
    if let Some(kind) = STATE_EDIT_TEST_HOOK.with(|hook| hook.borrow().hard_link_failure) {
        return Err(kind);
    }
    // SAFETY: both names are NUL-terminated single components and both
    // descriptors are the retained output directory. `linkat` never replaces
    // an existing destination; flags zero never requests source-symlink follow.
    let result = unsafe {
        libc::linkat(
            directory.as_raw_fd(),
            staging.as_ptr(),
            directory.as_raw_fd(),
            output.as_ptr(),
            0,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error().kind())
    }
}

fn clean_owned_entry(
    directory: &File,
    name: &CString,
    expected: FileIdentity,
) -> SnapshotStateEditCleanup {
    #[cfg(test)]
    if let Some(kind) = STATE_EDIT_TEST_HOOK.with(|hook| hook.borrow().cleanup_failure) {
        return SnapshotStateEditCleanup::Failed(kind);
    }
    match entry_identity(directory, name) {
        Ok(None) => SnapshotStateEditCleanup::AlreadyAbsent,
        Ok(Some(actual)) if actual != expected => SnapshotStateEditCleanup::ChangedRefused,
        Ok(Some(_)) => {
            // SAFETY: `directory` is live and `name` is one NUL-terminated
            // generated component. The immediate identity check is best effort;
            // trusted directory mutation authority remains required because
            // POSIX has no conditional unlink by expected inode.
            let result = unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) };
            if result == 0 {
                SnapshotStateEditCleanup::Removed
            } else {
                let kind = io::Error::last_os_error().kind();
                if kind == io::ErrorKind::NotFound {
                    SnapshotStateEditCleanup::AlreadyAbsent
                } else {
                    SnapshotStateEditCleanup::Failed(kind)
                }
            }
        }
        Err(kind) => SnapshotStateEditCleanup::Failed(kind),
    }
}

fn read_exact_at(file: &File, mut bytes: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !bytes.is_empty() {
        match file.read_at(bytes, offset) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
            Ok(count) => {
                let Some(remaining) = bytes.get_mut(count..) else {
                    return Err(io::Error::from(io::ErrorKind::InvalidData));
                };
                bytes = remaining;
                offset = offset
                    .checked_add(
                        u64::try_from(count)
                            .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?,
                    )
                    .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
            }
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) => return Err(source),
        }
    }
    Ok(())
}

#[cfg(not(test))]
fn enter_state_edit_stage(_stage: SnapshotStateEditStage) -> Result<(), io::ErrorKind> {
    Ok(())
}

#[cfg(test)]
type StageAction = Box<dyn FnOnce()>;

#[cfg(test)]
#[derive(Default)]
struct StateEditTestHook {
    failures: VecDeque<(SnapshotStateEditStage, io::ErrorKind)>,
    actions: VecDeque<(SnapshotStateEditStage, StageAction)>,
    random_failure: bool,
    random_names: VecDeque<[u8; STAGING_RANDOM_BYTES]>,
    hard_link_failure: Option<io::ErrorKind>,
    cleanup_failure: Option<io::ErrorKind>,
    staging_mode: Option<libc::c_int>,
}

#[cfg(test)]
thread_local! {
    static STATE_EDIT_TEST_HOOK: RefCell<StateEditTestHook> =
        RefCell::new(StateEditTestHook::default());
}

#[cfg(test)]
fn enter_state_edit_stage(stage: SnapshotStateEditStage) -> Result<(), io::ErrorKind> {
    let action = STATE_EDIT_TEST_HOOK.with(|hook| {
        let mut hook = hook.borrow_mut();
        hook.actions
            .iter()
            .position(|(candidate, _)| *candidate == stage)
            .and_then(|position| hook.actions.remove(position))
            .map(|(_, action)| action)
    });
    if let Some(action) = action {
        action();
    }
    STATE_EDIT_TEST_HOOK.with(|hook| {
        let mut hook = hook.borrow_mut();
        hook.failures
            .iter()
            .position(|(candidate, _)| *candidate == stage)
            .and_then(|position| hook.failures.remove(position))
            .map_or(Ok(()), |(_, kind)| Err(kind))
    })
}

#[cfg(test)]
pub(super) fn with_state_edit_failures<T>(
    failures: impl IntoIterator<Item = (SnapshotStateEditStage, io::ErrorKind)>,
    operation: impl FnOnce() -> T,
) -> T {
    with_test_hook(
        StateEditTestHook {
            failures: failures.into_iter().collect(),
            ..StateEditTestHook::default()
        },
        operation,
    )
}

#[cfg(test)]
pub(super) fn with_state_edit_action<T>(
    stage: SnapshotStateEditStage,
    action: impl FnOnce() + 'static,
    operation: impl FnOnce() -> T,
) -> T {
    let mut actions = VecDeque::new();
    actions.push_back((stage, Box::new(action) as StageAction));
    with_test_hook(
        StateEditTestHook {
            actions,
            ..StateEditTestHook::default()
        },
        operation,
    )
}

#[cfg(test)]
pub(super) fn with_state_edit_random_failure<T>(operation: impl FnOnce() -> T) -> T {
    with_test_hook(
        StateEditTestHook {
            random_failure: true,
            ..StateEditTestHook::default()
        },
        operation,
    )
}

#[cfg(test)]
pub(super) fn with_state_edit_random_names<T>(
    names: impl IntoIterator<Item = [u8; STAGING_RANDOM_BYTES]>,
    operation: impl FnOnce() -> T,
) -> T {
    with_test_hook(
        StateEditTestHook {
            random_names: names.into_iter().collect(),
            ..StateEditTestHook::default()
        },
        operation,
    )
}

#[cfg(test)]
pub(super) fn with_state_edit_hard_link_failure<T>(
    kind: io::ErrorKind,
    operation: impl FnOnce() -> T,
) -> T {
    with_test_hook(
        StateEditTestHook {
            hard_link_failure: Some(kind),
            ..StateEditTestHook::default()
        },
        operation,
    )
}

#[cfg(test)]
pub(super) fn with_state_edit_cleanup_failure<T>(
    kind: io::ErrorKind,
    operation: impl FnOnce() -> T,
) -> T {
    with_test_hook(
        StateEditTestHook {
            cleanup_failure: Some(kind),
            ..StateEditTestHook::default()
        },
        operation,
    )
}

#[cfg(test)]
pub(super) fn with_state_edit_failure_and_cleanup_failure<T>(
    stage: SnapshotStateEditStage,
    primary: io::ErrorKind,
    cleanup: io::ErrorKind,
    operation: impl FnOnce() -> T,
) -> T {
    with_test_hook(
        StateEditTestHook {
            failures: [(stage, primary)].into_iter().collect(),
            cleanup_failure: Some(cleanup),
            ..StateEditTestHook::default()
        },
        operation,
    )
}

#[cfg(test)]
pub(super) fn with_state_edit_action_and_cleanup_failure<T>(
    stage: SnapshotStateEditStage,
    action: impl FnOnce() + 'static,
    cleanup: io::ErrorKind,
    operation: impl FnOnce() -> T,
) -> T {
    let mut actions = VecDeque::new();
    actions.push_back((stage, Box::new(action) as StageAction));
    with_test_hook(
        StateEditTestHook {
            actions,
            cleanup_failure: Some(cleanup),
            ..StateEditTestHook::default()
        },
        operation,
    )
}

#[cfg(test)]
pub(super) fn with_state_edit_staging_mode<T>(
    mode: libc::c_int,
    operation: impl FnOnce() -> T,
) -> T {
    with_test_hook(
        StateEditTestHook {
            staging_mode: Some(mode),
            ..StateEditTestHook::default()
        },
        operation,
    )
}

#[cfg(test)]
fn with_test_hook<T>(hook: StateEditTestHook, operation: impl FnOnce() -> T) -> T {
    struct Reset;

    impl Drop for Reset {
        fn drop(&mut self) {
            STATE_EDIT_TEST_HOOK.with(|current| {
                *current.borrow_mut() = StateEditTestHook::default();
            });
        }
    }

    STATE_EDIT_TEST_HOOK.with(|current| {
        *current.borrow_mut() = hook;
    });
    let reset = Reset;
    let result = operation();
    drop(reset);
    result
}
