//! Bounded path-redacted CPU-template helper input.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use rustix::fd::OwnedFd;
use rustix::fs::{FileType, Mode, OFlags, fstat, open, openat};

use crate::CPU_TEMPLATE_DOCUMENT_MAX_BYTES;

/// Failure while reading one helper input document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputError {
    Open,
    Inspect,
    NotRegular,
    TooLarge,
    Read,
    InvalidUtf8,
}

impl fmt::Display for InputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Open => "helper input could not be opened safely",
            Self::Inspect => "helper input could not be inspected safely",
            Self::NotRegular => "helper input must be a regular file",
            Self::TooLarge => "helper input exceeds the size limit",
            Self::Read => "helper input could not be read",
            Self::InvalidUtf8 => "helper input is not UTF-8",
        })
    }
}

impl std::error::Error for InputError {}

/// Failure while preparing a batch strip input and destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripInputError {
    Input(InputError),
    TooFewInputs,
    InvalidPath,
    UnsafeSuffix,
    DuplicateInput,
    DuplicateOutput,
    InputOutputCollision,
    SharedInput,
}

impl fmt::Display for StripInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Input(source) => write!(formatter, "{source}"),
            Self::TooFewInputs => {
                formatter.write_str("CPU-template strip requires at least two inputs")
            }
            Self::InvalidPath => formatter.write_str("CPU-template strip path is invalid"),
            Self::UnsafeSuffix => formatter.write_str("CPU-template strip suffix is unsafe"),
            Self::DuplicateInput => formatter.write_str("CPU-template strip input is duplicated"),
            Self::DuplicateOutput => formatter.write_str("CPU-template strip output is duplicated"),
            Self::InputOutputCollision => {
                formatter.write_str("CPU-template strip input and output collide")
            }
            Self::SharedInput => {
                formatter.write_str("CPU-template strip replacement input has multiple links")
            }
        }
    }
}

impl std::error::Error for StripInputError {}

impl From<InputError> for StripInputError {
    fn from(source: InputError) -> Self {
        Self::Input(source)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct FileIdentity {
    device: u64,
    inode: u64,
}

impl fmt::Debug for FileIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("FileIdentity(<redacted>)")
    }
}

impl FileIdentity {
    pub(crate) fn from_metadata(metadata: &rustix::fs::Stat) -> Self {
        Self {
            #[cfg(target_os = "linux")]
            device: metadata.st_dev,
            #[cfg(target_os = "macos")]
            device: metadata.st_dev as u64,
            inode: metadata.st_ino,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StripOutputMode {
    Absent,
    ReplaceInput,
}

pub(crate) struct PreparedStripInput {
    directory: OwnedFd,
    input: File,
    directory_identity: FileIdentity,
    input_identity: FileIdentity,
    input_link_count: u64,
    input_name: OsString,
    output_name: OsString,
    mode: StripOutputMode,
}

impl fmt::Debug for PreparedStripInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedStripInput")
            .field("mode", &self.mode)
            .field("directory", &"<redacted>")
            .field("input", &"<redacted>")
            .field("output", &"<redacted>")
            .finish()
    }
}

impl PreparedStripInput {
    pub(crate) fn directory(&self) -> &OwnedFd {
        &self.directory
    }

    pub(crate) fn input(&self) -> &File {
        &self.input
    }

    pub(crate) const fn directory_identity(&self) -> FileIdentity {
        self.directory_identity
    }

    pub(crate) const fn input_identity(&self) -> FileIdentity {
        self.input_identity
    }

    pub(crate) const fn input_link_count(&self) -> u64 {
        self.input_link_count
    }

    pub(crate) fn input_name(&self) -> &OsStr {
        &self.input_name
    }

    pub(crate) fn output_name(&self) -> &OsStr {
        &self.output_name
    }

    pub(crate) const fn mode(&self) -> StripOutputMode {
        self.mode
    }
}

/// Read at most one MiB from a no-follow regular file and validate UTF-8.
pub fn read_regular_utf8(path: &Path) -> Result<String, InputError> {
    read_regular_utf8_with_post_inspection(path, || {})
}

/// Open, bind, and read one strip input relative to its retained parent.
pub(crate) fn prepare_strip_input(
    path: &Path,
    suffix: &str,
) -> Result<(PreparedStripInput, String), StripInputError> {
    validate_strip_suffix(suffix)?;
    let input_name = path
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or(StripInputError::InvalidPath)?;
    let output_name = derive_output_name(path, suffix)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let directory = open(
        parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|_| InputError::Open)?;
    let directory_metadata = fstat(&directory).map_err(|_| InputError::Inspect)?;
    let descriptor = openat(
        &directory,
        input_name,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|_| InputError::Open)?;
    let metadata = fstat(&descriptor).map_err(|_| InputError::Inspect)?;
    let size = inspect_regular_size(&metadata)?;
    let mut input = File::from(descriptor);
    let contents = read_bounded_utf8(&mut input, size)?;
    let input_link_count = metadata.st_nlink.into();
    let mode = if suffix.is_empty() {
        StripOutputMode::ReplaceInput
    } else {
        StripOutputMode::Absent
    };

    Ok((
        PreparedStripInput {
            directory,
            input,
            directory_identity: FileIdentity::from_metadata(&directory_metadata),
            input_identity: FileIdentity::from_metadata(&metadata),
            input_link_count,
            input_name: input_name.to_os_string(),
            output_name,
            mode,
        },
        contents,
    ))
}

pub(crate) fn validate_prepared_strip_inputs(
    inputs: &[PreparedStripInput],
) -> Result<(), StripInputError> {
    if inputs.len() < 2 {
        return Err(StripInputError::TooFewInputs);
    }
    let mode = inputs
        .first()
        .map(PreparedStripInput::mode)
        .ok_or(StripInputError::TooFewInputs)?;
    let mut identities = BTreeSet::new();
    let mut input_entries = BTreeSet::new();
    let mut output_entries = BTreeSet::new();

    for input in inputs {
        if input.mode() != mode {
            return Err(StripInputError::UnsafeSuffix);
        }
        if !identities.insert(input.input_identity()) {
            return Err(StripInputError::DuplicateInput);
        }
        if !input_entries.insert((
            input.directory_identity(),
            input.input_name().to_os_string(),
        )) {
            return Err(StripInputError::DuplicateInput);
        }
        if !output_entries.insert((
            input.directory_identity(),
            input.output_name().to_os_string(),
        )) {
            return Err(StripInputError::DuplicateOutput);
        }
        if mode == StripOutputMode::ReplaceInput
            && (input.input_name() != input.output_name() || input.input_link_count() != 1)
        {
            return Err(StripInputError::SharedInput);
        }
    }
    if mode == StripOutputMode::Absent
        && output_entries
            .iter()
            .any(|output| input_entries.contains(output))
    {
        return Err(StripInputError::InputOutputCollision);
    }
    Ok(())
}

fn validate_strip_suffix(suffix: &str) -> Result<(), StripInputError> {
    if suffix.chars().any(std::path::is_separator) {
        Err(StripInputError::UnsafeSuffix)
    } else {
        Ok(())
    }
}

fn derive_output_name(path: &Path, suffix: &str) -> Result<OsString, StripInputError> {
    let stem = path
        .file_stem()
        .filter(|stem| !stem.is_empty())
        .ok_or(StripInputError::InvalidPath)?;
    let mut output = OsString::from(stem);
    output.push(suffix);
    if let Some(extension) = path.extension() {
        output.push(".");
        output.push(extension);
    }
    let output_path = Path::new(&output);
    if output.is_empty()
        || output_path.file_name() != Some(output.as_os_str())
        || output_path
            .parent()
            .is_some_and(|parent| !parent.as_os_str().is_empty())
    {
        return Err(StripInputError::InvalidPath);
    }
    Ok(output)
}

fn inspect_regular_size(metadata: &rustix::fs::Stat) -> Result<usize, InputError> {
    if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile {
        return Err(InputError::NotRegular);
    }
    let size = usize::try_from(metadata.st_size).map_err(|_| InputError::Inspect)?;
    if size > CPU_TEMPLATE_DOCUMENT_MAX_BYTES {
        return Err(InputError::TooLarge);
    }
    Ok(size)
}

fn read_bounded_utf8(file: &mut File, size: usize) -> Result<String, InputError> {
    let mut contents = Vec::with_capacity(size);
    file.take(CPU_TEMPLATE_DOCUMENT_MAX_BYTES as u64 + 1)
        .read_to_end(&mut contents)
        .map_err(|_| InputError::Read)?;
    if contents.len() > CPU_TEMPLATE_DOCUMENT_MAX_BYTES {
        return Err(InputError::TooLarge);
    }
    String::from_utf8(contents).map_err(|_| InputError::InvalidUtf8)
}

fn read_regular_utf8_with_post_inspection(
    path: &Path,
    post_inspection: impl FnOnce(),
) -> Result<String, InputError> {
    let descriptor = open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|_| InputError::Open)?;
    let metadata = fstat(&descriptor).map_err(|_| InputError::Inspect)?;
    let size = inspect_regular_size(&metadata)?;
    post_inspection();

    let mut file = File::from(descriptor);
    read_bounded_utf8(&mut file, size)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::symlink;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    #[derive(Debug)]
    struct TestDirectory(std::path::PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should follow epoch")
                .as_nanos();
            let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "bangbang-cpu-template-input-{}-{nonce}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("test directory should be created");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reads_boundaries_and_rejects_oversize_or_invalid_utf8() {
        let directory = TestDirectory::new();
        let empty = directory.0.join("empty");
        fs::write(&empty, []).expect("empty fixture should be written");
        assert_eq!(read_regular_utf8(&empty).as_deref(), Ok(""));

        let exact = directory.0.join("exact");
        fs::write(&exact, vec![b'a'; CPU_TEMPLATE_DOCUMENT_MAX_BYTES])
            .expect("fixture should be written");
        assert_eq!(
            read_regular_utf8(&exact)
                .expect("exact limit should read")
                .len(),
            CPU_TEMPLATE_DOCUMENT_MAX_BYTES
        );

        let oversized = directory.0.join("oversized");
        fs::write(&oversized, vec![b'a'; CPU_TEMPLATE_DOCUMENT_MAX_BYTES + 1])
            .expect("fixture should be written");
        assert_eq!(read_regular_utf8(&oversized), Err(InputError::TooLarge));

        let invalid = directory.0.join("invalid");
        fs::write(&invalid, [0xff]).expect("fixture should be written");
        assert_eq!(read_regular_utf8(&invalid), Err(InputError::InvalidUtf8));

        let growing = directory.0.join("growing");
        fs::write(&growing, vec![b'a'; CPU_TEMPLATE_DOCUMENT_MAX_BYTES])
            .expect("growing fixture should be written");
        assert_eq!(
            read_regular_utf8_with_post_inspection(&growing, || {
                let mut file = fs::OpenOptions::new()
                    .append(true)
                    .open(&growing)
                    .expect("growing fixture should reopen");
                file.write_all(b"x").expect("growing fixture should extend");
            }),
            Err(InputError::TooLarge)
        );
    }

    #[test]
    fn rejects_symlink_and_non_regular_paths_without_echoing_paths() {
        let directory = TestDirectory::new();
        let target = directory.0.join("target");
        fs::write(&target, b"{}").expect("fixture should be written");
        let link = directory.0.join("private-link-name");
        symlink(&target, &link).expect("symlink fixture should be created");
        let error = read_regular_utf8(&link).expect_err("symlink should fail closed");
        assert_eq!(error, InputError::Open);
        assert!(!error.to_string().contains("private-link-name"));

        assert_eq!(read_regular_utf8(&directory.0), Err(InputError::NotRegular));

        let fifo = directory.0.join("private-fifo-name");
        let status = Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo should be available on supported hosts");
        assert!(status.success(), "FIFO fixture should be created");
        let error = read_regular_utf8(&fifo).expect_err("FIFO should fail without blocking");
        assert_eq!(error, InputError::NotRegular);
        assert!(!error.to_string().contains("private-fifo-name"));
    }

    #[test]
    fn strip_batch_rejects_aliases_and_collisions_but_allows_unlisted_input_links() {
        let directory = TestDirectory::new();
        let first = directory.0.join("first.json");
        let second = directory.0.join("second.json");
        fs::write(&first, b"{}").expect("first fixture should be written");
        fs::write(&second, b"{}").expect("second fixture should be written");

        let duplicate = [
            prepare_strip_input(&first, "_stripped")
                .expect("first input should prepare")
                .0,
            prepare_strip_input(&first, "_stripped")
                .expect("duplicate input should prepare")
                .0,
        ];
        assert_eq!(
            validate_prepared_strip_inputs(&duplicate),
            Err(StripInputError::DuplicateInput)
        );

        let alias = directory.0.join("first-alias.json");
        fs::hard_link(&first, &alias).expect("hard-link alias should be created");
        let aliased = [
            prepare_strip_input(&first, "_stripped")
                .expect("first input should prepare")
                .0,
            prepare_strip_input(&alias, "_stripped")
                .expect("alias input should prepare")
                .0,
        ];
        assert_eq!(
            validate_prepared_strip_inputs(&aliased),
            Err(StripInputError::DuplicateInput)
        );

        let ordinary = [
            prepare_strip_input(&first, "_stripped")
                .expect("linked input should prepare")
                .0,
            prepare_strip_input(&second, "_stripped")
                .expect("second input should prepare")
                .0,
        ];
        validate_prepared_strip_inputs(&ordinary)
            .expect("an unlisted link must be safe in absent-only mode");

        let collision = directory.0.join("first_stripped.json");
        fs::write(&collision, b"{}").expect("collision input should be written");
        let colliding = [
            prepare_strip_input(&first, "_stripped")
                .expect("first input should prepare")
                .0,
            prepare_strip_input(&collision, "_stripped")
                .expect("collision input should prepare")
                .0,
        ];
        assert_eq!(
            validate_prepared_strip_inputs(&colliding),
            Err(StripInputError::InputOutputCollision)
        );

        let replacing = [
            prepare_strip_input(&first, "")
                .expect("linked replacement input should prepare")
                .0,
            prepare_strip_input(&second, "")
                .expect("second replacement input should prepare")
                .0,
        ];
        assert_eq!(
            validate_prepared_strip_inputs(&replacing),
            Err(StripInputError::SharedInput)
        );
    }

    #[test]
    fn strip_suffix_and_debug_fail_before_exposing_paths_or_identities() {
        let private_missing = Path::new("private-missing-template.json");
        let error = prepare_strip_input(private_missing, "../escape")
            .expect_err("unsafe suffix must fail before path access");
        assert_eq!(error, StripInputError::UnsafeSuffix);
        assert!(!error.to_string().contains("private-missing"));

        let directory = TestDirectory::new();
        let private = directory.0.join("private-template-name.json");
        fs::write(&private, b"{}").expect("private fixture should be written");
        let prepared = prepare_strip_input(&private, "_stripped")
            .expect("private fixture should prepare")
            .0;
        let debug = format!("{prepared:?}");
        assert!(!debug.contains("private-template-name"));
        assert!(!debug.contains(&prepared.input_identity().inode.to_string()));
        assert!(debug.contains("<redacted>"));
    }
}
