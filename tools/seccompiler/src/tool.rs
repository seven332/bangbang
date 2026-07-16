use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

use bangbang_seccompiler::{
    CompileError, CompileOptions, CompiledFilters, MAX_JSON_BYTES, TargetArch, compile_json,
};
use rustix::fs::{FileType, Mode, OFlags, fstat, open};

use crate::artifact::{Artifact, PublicationError, publish};

const MAX_COMBINED_OUTPUT_BYTES: usize = 100_000;
const SPLIT_CATEGORIES: [&str; 3] = ["vmm", "api", "vcpu"];

#[derive(Debug)]
pub(super) struct RunOptions {
    pub(super) target_arch: String,
    pub(super) input_file: PathBuf,
    pub(super) output_file: PathBuf,
    pub(super) basic: bool,
    pub(super) split_output: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ToolError {
    UnsupportedTargetArchitecture,
    InputOpen,
    InputNotRegular,
    InputRead,
    InputTooLarge,
    InputNotUtf8,
    Compile(CompileError),
    Serialize,
    CombinedOutputTooLarge,
    InvalidOutputPath,
    Publish(PublicationError),
}

impl fmt::Display for ToolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedTargetArchitecture => "target architecture is unsupported",
            Self::InputOpen => "input file could not be opened safely",
            Self::InputNotRegular => "input path is not a regular file",
            Self::InputRead => "input file could not be read",
            Self::InputTooLarge => "input file exceeds the size limit",
            Self::InputNotUtf8 => "input file is not valid UTF-8",
            Self::Compile(error) => return write!(formatter, "{error}"),
            Self::Serialize => "compiled filters could not be serialized",
            Self::CombinedOutputTooLarge => "combined output exceeds the Firecracker size limit",
            Self::InvalidOutputPath => "output path must name a file",
            Self::Publish(error) => return write!(formatter, "{error}"),
        })
    }
}

impl From<CompileError> for ToolError {
    fn from(error: CompileError) -> Self {
        Self::Compile(error)
    }
}

impl From<PublicationError> for ToolError {
    fn from(error: PublicationError) -> Self {
        Self::Publish(error)
    }
}

pub(super) fn run(options: &RunOptions) -> Result<(), ToolError> {
    let target_arch = options
        .target_arch
        .parse::<TargetArch>()
        .map_err(|_| ToolError::UnsupportedTargetArchitecture)?;
    let input = read_input(options)?;
    let filters = compile_json(
        &input,
        target_arch,
        CompileOptions::new().with_basic(options.basic),
    )?;
    let (output_directory, artifacts) = build_artifacts(options, &filters)?;
    publish(&output_directory, &artifacts)?;
    Ok(())
}

fn read_input(options: &RunOptions) -> Result<String, ToolError> {
    let descriptor = open(
        &options.input_file,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|_| ToolError::InputOpen)?;
    let metadata = fstat(&descriptor).map_err(|_| ToolError::InputRead)?;
    if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile {
        return Err(ToolError::InputNotRegular);
    }

    let mut bytes = Vec::new();
    File::from(descriptor)
        .take((MAX_JSON_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| ToolError::InputRead)?;
    if bytes.len() > MAX_JSON_BYTES {
        return Err(ToolError::InputTooLarge);
    }
    String::from_utf8(bytes).map_err(|_| ToolError::InputNotUtf8)
}

fn build_artifacts(
    options: &RunOptions,
    filters: &CompiledFilters,
) -> Result<(PathBuf, Vec<Artifact>), ToolError> {
    let output_name = options
        .output_file
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or(ToolError::InvalidOutputPath)?;
    let parent = options
        .output_file
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf();

    if options.split_output {
        let mut artifacts = Vec::with_capacity(SPLIT_CATEGORIES.len());
        for category in SPLIT_CATEGORIES {
            let words = filters
                .get(category)
                .ok_or(ToolError::Compile(CompileError::InvalidThreadCategories))?;
            let mut bytes = Vec::with_capacity(words.len() * size_of::<u64>());
            for word in words {
                bytes.extend_from_slice(&word.to_le_bytes());
            }
            artifacts.push(Artifact {
                name: format!("{category}.bpf").into(),
                bytes,
            });
        }
        Ok((parent, artifacts))
    } else {
        let bytes = serialize_combined(filters)?;
        Ok((
            parent,
            vec![Artifact {
                name: output_name.to_os_string(),
                bytes,
            }],
        ))
    }
}

fn serialize_combined(filters: &BTreeMap<String, Vec<u64>>) -> Result<Vec<u8>, ToolError> {
    let bytes = bitcode::serialize(filters).map_err(|_| ToolError::Serialize)?;
    if bytes.len() > MAX_COMBINED_OUTPUT_BYTES {
        return Err(ToolError::CombinedOutputTooLarge);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn every_error_message_is_static_and_value_redacted() {
        let sensitive = "private-path-or-policy-value";
        let errors = [
            ToolError::UnsupportedTargetArchitecture,
            ToolError::InputOpen,
            ToolError::InputNotRegular,
            ToolError::InputRead,
            ToolError::InputTooLarge,
            ToolError::InputNotUtf8,
            ToolError::Compile(CompileError::InvalidJson),
            ToolError::Serialize,
            ToolError::CombinedOutputTooLarge,
            ToolError::InvalidOutputPath,
        ];
        for error in errors {
            assert!(!error.to_string().contains(sensitive));
            assert!(!format!("{error:?}").contains(sensitive));
        }
    }

    #[test]
    fn combined_encoding_deserializes_as_firecrackers_hash_map_shape() {
        let filters = BTreeMap::from([
            ("api".to_owned(), vec![1, 2]),
            ("vcpu".to_owned(), vec![3]),
            ("vmm".to_owned(), vec![4, 5, 6]),
        ]);
        let encoded = serialize_combined(&filters).expect("encoding should succeed");
        let decoded: std::collections::HashMap<String, Vec<u64>> =
            bitcode::deserialize(&encoded).expect("Firecracker shape should decode");
        assert_eq!(decoded.len(), 3);
        assert_eq!(decoded.get("api"), Some(&vec![1, 2]));
        assert_eq!(decoded.get("vcpu"), Some(&vec![3]));
        assert_eq!(decoded.get("vmm"), Some(&vec![4, 5, 6]));
    }

    #[test]
    fn combined_encoding_enforces_firecrackers_consumer_limit() {
        let mut state = 0x1234_5678_9abc_def0_u64;
        let words = std::iter::repeat_with(|| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        })
        .take(200_000)
        .collect();
        let filters = BTreeMap::from([("vmm".to_owned(), words)]);
        assert_eq!(
            serialize_combined(&filters),
            Err(ToolError::CombinedOutputTooLarge)
        );
    }
}
