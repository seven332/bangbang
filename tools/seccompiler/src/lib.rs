//! Host-portable compiler for Firecracker v1.16 seccomp policies.
//!
//! This crate transforms the pinned JSON policy shape into classic-BPF
//! programs for Linux x86_64 or aarch64 targets. It is an offline artifact
//! compiler: it cannot install or enforce seccomp on macOS and deliberately
//! exposes no filter-installation API.

mod bpf;
mod compiler;
mod schema;
mod syscalls;

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

/// Maximum accepted in-memory JSON policy size.
pub const MAX_JSON_BYTES: usize = 1024 * 1024;

/// Maximum number of syscall rules accepted for one thread category.
pub const MAX_RULES_PER_THREAD: usize = 1024;

/// Maximum number of argument conditions accepted for one syscall rule.
pub const MAX_CONDITIONS_PER_RULE: usize = 6;

/// Maximum classic-BPF instructions accepted for one Linux seccomp program.
pub const MAX_BPF_INSTRUCTIONS: usize = 4096;

/// Ordered Firecracker thread-category programs in v1.16 `u64` form.
pub type CompiledFilters = BTreeMap<String, Vec<u64>>;

/// Linux architecture for which the seccomp artifact is compiled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetArch {
    /// Linux x86_64, excluding the x32 ABI.
    X86_64,
    /// Linux aarch64.
    Aarch64,
}

impl FromStr for TargetArch {
    type Err = CompileError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.eq_ignore_ascii_case("x86_64") {
            Ok(Self::X86_64)
        } else if value.eq_ignore_ascii_case("aarch64") {
            Ok(Self::Aarch64)
        } else {
            Err(CompileError::InvalidTargetArchitecture)
        }
    }
}

/// Options that affect the offline compilation transform.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompileOptions {
    basic: bool,
}

impl CompileOptions {
    /// Creates advanced-mode options that preserve argument conditions.
    #[must_use]
    pub const fn new() -> Self {
        Self { basic: false }
    }

    /// Selects Firecracker's deprecated basic mode, which drops conditions.
    #[must_use]
    pub const fn with_basic(mut self, basic: bool) -> Self {
        self.basic = basic;
        self
    }

    pub(crate) const fn is_basic(self) -> bool {
        self.basic
    }
}

/// Stable, caller-value-redacted compilation failure category.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompileError {
    /// The input exceeded [`MAX_JSON_BYTES`].
    InputTooLarge,
    /// The input was not syntactically valid JSON.
    InvalidJson,
    /// A JSON object contained a duplicate key.
    DuplicateObjectKey,
    /// The JSON did not match the pinned Firecracker v1.16 schema.
    InvalidSchema,
    /// The policy did not contain exactly `vmm`, `api`, and `vcpu`.
    InvalidThreadCategories,
    /// One thread category exceeded [`MAX_RULES_PER_THREAD`].
    TooManyRules,
    /// One rule exceeded [`MAX_CONDITIONS_PER_RULE`].
    TooManyConditions,
    /// A condition used an argument index outside zero through five.
    InvalidArgumentIndex,
    /// A nonempty filter used the same match and default action.
    IdenticalActions,
    /// A syscall name was not present in libseccomp v2.6.0's table.
    UnknownSyscall,
    /// The checked embedded syscall table violated an invariant.
    InvalidEmbeddedSyscallTable,
    /// A generated program exceeded [`MAX_BPF_INSTRUCTIONS`].
    ProgramTooLarge,
    /// A generated classic-BPF program violated a structural invariant.
    InvalidProgram,
    /// A target architecture string was unsupported.
    InvalidTargetArchitecture,
}

impl fmt::Display for CompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InputTooLarge => "seccomp policy exceeds the input limit",
            Self::InvalidJson => "seccomp policy is not valid JSON",
            Self::DuplicateObjectKey => "seccomp policy contains a duplicate object key",
            Self::InvalidSchema => "seccomp policy does not match the v1.16 schema",
            Self::InvalidThreadCategories => {
                "seccomp policy must contain the required thread categories"
            }
            Self::TooManyRules => "seccomp policy exceeds the rule limit",
            Self::TooManyConditions => "seccomp policy exceeds the condition limit",
            Self::InvalidArgumentIndex => "seccomp policy contains an invalid argument index",
            Self::IdenticalActions => "seccomp filter actions must differ when rules are present",
            Self::UnknownSyscall => "seccomp policy contains an unknown target syscall",
            Self::InvalidEmbeddedSyscallTable => "embedded seccomp syscall table is invalid",
            Self::ProgramTooLarge => "compiled seccomp program exceeds the instruction limit",
            Self::InvalidProgram => "compiled seccomp program is structurally invalid",
            Self::InvalidTargetArchitecture => "seccomp target architecture is unsupported",
        })
    }
}

impl std::error::Error for CompileError {}

/// Compiles one complete Firecracker v1.16 JSON policy for a Linux target.
///
/// The input must contain exactly the `vmm`, `api`, and `vcpu` categories. A
/// successful result is ordered by category and contains the numeric `u64`
/// representation expected by Firecracker v1.16's later serialization step.
/// This function performs no filesystem access and does not install a filter.
///
/// # Errors
///
/// Returns a value-redacted [`CompileError`] when parsing, validation, target
/// syscall resolution, lowering, or structural program validation fails.
pub fn compile_json(
    input: &str,
    target_arch: TargetArch,
    options: CompileOptions,
) -> Result<CompiledFilters, CompileError> {
    let policy = schema::parse(input)?;
    let syscall_table = syscalls::SyscallTable::new()?;
    compiler::compile(policy, target_arch, options, &syscall_table)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_target_names_without_disclosing_invalid_input() {
        assert_eq!("x86_64".parse(), Ok(TargetArch::X86_64));
        assert_eq!("X86_64".parse(), Ok(TargetArch::X86_64));
        assert_eq!("aarch64".parse(), Ok(TargetArch::Aarch64));
        assert_eq!("AaRcH64".parse(), Ok(TargetArch::Aarch64));

        let sensitive = "private-target-value";
        let error = sensitive.parse::<TargetArch>().unwrap_err();
        assert_eq!(error, CompileError::InvalidTargetArchitecture);
        assert!(!error.to_string().contains(sensitive));
        assert!(!format!("{error:?}").contains(sensitive));
    }

    #[test]
    fn options_default_to_advanced_mode() {
        assert!(!CompileOptions::default().is_basic());
        assert!(!CompileOptions::new().is_basic());
        assert!(CompileOptions::new().with_basic(true).is_basic());
    }

    #[test]
    fn every_public_error_category_has_static_redacted_output() {
        let sensitive = "private-policy-value";
        for error in [
            CompileError::InputTooLarge,
            CompileError::InvalidJson,
            CompileError::DuplicateObjectKey,
            CompileError::InvalidSchema,
            CompileError::InvalidThreadCategories,
            CompileError::TooManyRules,
            CompileError::TooManyConditions,
            CompileError::InvalidArgumentIndex,
            CompileError::IdenticalActions,
            CompileError::UnknownSyscall,
            CompileError::InvalidEmbeddedSyscallTable,
            CompileError::ProgramTooLarge,
            CompileError::InvalidProgram,
            CompileError::InvalidTargetArchitecture,
        ] {
            assert!(!error.to_string().contains(sensitive));
            assert!(!format!("{error:?}").contains(sensitive));
        }
    }
}
