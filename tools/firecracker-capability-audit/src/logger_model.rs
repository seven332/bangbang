use serde::{Deserialize, Serialize};

use crate::{Baseline, Input, Reference};

/// Cardinalities for the pinned logger producer source population.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoggerProducerCounts {
    pub scanned_rust_files: usize,
    pub matching_input_files: usize,
    pub ordinary: usize,
    pub unrestricted: usize,
    pub error: usize,
    pub warn: usize,
    pub info: usize,
    pub debug: usize,
    pub trace: usize,
    pub error_unrestricted: usize,
    pub warn_unrestricted: usize,
    pub info_unrestricted: usize,
    pub production: usize,
    pub test: usize,
    pub example: usize,
    pub direct: usize,
    pub macro_template: usize,
}

/// Exact Firecracker logger macro invoked by a producer site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LoggerMacro {
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "warn")]
    Warn,
    #[serde(rename = "info")]
    Info,
    #[serde(rename = "debug")]
    Debug,
    #[serde(rename = "trace")]
    Trace,
    #[serde(rename = "error_unrestricted")]
    ErrorUnrestricted,
    #[serde(rename = "warn_unrestricted")]
    WarnUnrestricted,
    #[serde(rename = "info_unrestricted")]
    InfoUnrestricted,
}

impl LoggerMacro {
    pub(crate) const fn from_name(name: &str) -> Option<Self> {
        match name.as_bytes() {
            b"error" => Some(Self::Error),
            b"warn" => Some(Self::Warn),
            b"info" => Some(Self::Info),
            b"debug" => Some(Self::Debug),
            b"trace" => Some(Self::Trace),
            b"error_unrestricted" => Some(Self::ErrorUnrestricted),
            b"warn_unrestricted" => Some(Self::WarnUnrestricted),
            b"info_unrestricted" => Some(Self::InfoUnrestricted),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
            Self::ErrorUnrestricted => "error_unrestricted",
            Self::WarnUnrestricted => "warn_unrestricted",
            Self::InfoUnrestricted => "info_unrestricted",
        }
    }

    pub const fn is_unrestricted(self) -> bool {
        matches!(
            self,
            Self::ErrorUnrestricted | Self::WarnUnrestricted | Self::InfoUnrestricted
        )
    }
}

/// Compile context of a pinned logger invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoggerSourceContext {
    Production,
    Test,
    Example,
}

/// Rust syntax location in which a pinned invocation is expressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoggerInvocationSyntax {
    Direct,
    MacroTemplate,
}

/// One value-redacted invocation identity derived from pinned Rust syntax.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoggerInvocation {
    pub id: String,
    pub path: String,
    pub line: usize,
    pub column: usize,
    pub macro_name: LoggerMacro,
    pub syntax: LoggerInvocationSyntax,
    pub source_context: LoggerSourceContext,
    pub fingerprint: String,
}

/// Machine-owned logger producer manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoggerProducerManifest {
    pub schema_version: u32,
    pub baseline: Baseline,
    pub generator_version: u32,
    pub inputs: Vec<Input>,
    pub counts: LoggerProducerCounts,
    pub invocations: Vec<LoggerInvocation>,
}

/// Closed subsystem owning a semantic logger producer class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoggerSubsystem {
    Api,
    Process,
    VmLifecycle,
    Snapshot,
    CpuAndMachine,
    DeviceTransport,
    Storage,
    NetworkAndMmds,
    Vsock,
    MemoryDevices,
    RemainingDevices,
    Observability,
    DeveloperTooling,
}

/// Delivery contract for a semantic logger class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoggerDeliveryPolicy {
    UnrestrictedHost,
    BoundedHost,
    NonblockingAsync,
    NonblockingGuest,
    NotApplicable,
}

/// Fixed level for a semantic logger class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoggerLevelPolicy {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
    CategoryDerived,
    OutcomeDerived,
    NotApplicable,
}

/// Fixed module identity for a semantic logger class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LoggerModulePolicy {
    #[serde(rename = "bangbang::process")]
    Process,
    #[serde(rename = "bangbang::panic")]
    Panic,
    #[serde(rename = "bangbang::worker")]
    Worker,
    #[serde(rename = "bangbang_runtime::api_server")]
    ApiServer,
    #[serde(rename = "bangbang_runtime::vmm_action")]
    VmmAction,
    #[serde(rename = "bangbang_runtime::boot_timer")]
    BootTimer,
    #[serde(rename = "bangbang_runtime::lifecycle")]
    Lifecycle,
    #[serde(rename = "bangbang_runtime::snapshot")]
    Snapshot,
    #[serde(rename = "bangbang_runtime::device")]
    Device,
    #[serde(rename = "bangbang_hvf::backend")]
    Backend,
    #[serde(rename = "bangbang_hvf::vcpu")]
    Vcpu,
    #[serde(rename = "not-applicable")]
    NotApplicable,
}

/// Origin-prefix behavior for a semantic logger class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoggerOriginPolicy {
    Configurable,
    PreparedEmergency,
    NotApplicable,
}

/// Limiter behavior for a semantic logger class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoggerLimiterPolicy {
    Unrestricted,
    RateLimited,
    NotApplicable,
}

/// Closed safe field vocabulary admitted by a semantic logger class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoggerField {
    Action,
    Category,
    CpuTimeUs,
    DeviceKind,
    Method,
    Operation,
    Outcome,
    Route,
    SuppressedCount,
    WallTimeUs,
}

/// Delivery state of a semantic logger class in the current repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoggerClassDisposition {
    Implemented,
    Planned,
    NotApplicable,
}

/// Exact reason that an upstream invocation is not a Bangbang producer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoggerNonApplicableReason {
    TestOnly,
    ExampleOnly,
    DeveloperInstrumentation,
    DuplicatePropagation,
    TracingOwned,
    LinuxKvmOnly,
    X86Only,
    UpstreamInternalMechanism,
    SeparateToolOwner,
}

/// Existing compiled event shape synchronized by the audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoggerCompiledEvent {
    ApiControl,
    ApiWorker,
    ApiRequest,
    ApiResult,
    InstanceStart,
    FlushMetrics,
    BootTime,
    Lifecycle,
    Observability,
    RateLimitRecovery,
    ProcessStartup,
    ProcessPanic,
    ProcessExit,
    ProcessSignal,
    Snapshot,
}

/// Human-owned policy and evidence for one coherent semantic producer class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoggerProducerClass {
    pub id: String,
    pub subsystem: LoggerSubsystem,
    pub summary: String,
    pub guest_triggerable: bool,
    pub delivery: LoggerDeliveryPolicy,
    pub level: LoggerLevelPolicy,
    pub module: LoggerModulePolicy,
    pub origin: LoggerOriginPolicy,
    pub limiter: LoggerLimiterPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limiter_identity: Option<String>,
    pub allowed_fields: Vec<LoggerField>,
    pub disposition: LoggerClassDisposition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub non_applicable_reason: Option<LoggerNonApplicableReason>,
    pub rationale: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_issue: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compiled_events: Vec<LoggerCompiledEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub implementation: Vec<Reference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validation: Vec<Reference>,
}

/// Explicit source-to-class mapping for one pinned invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoggerProducerMapping {
    pub invocation_id: String,
    pub class_id: String,
}

/// Human-owned logger producer classification overlay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoggerProducerAudit {
    pub schema_version: u32,
    pub baseline: Baseline,
    pub classes: Vec<LoggerProducerClass>,
    pub mappings: Vec<LoggerProducerMapping>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logger_macro_names_are_exact() {
        assert_eq!(LoggerMacro::from_name("error"), Some(LoggerMacro::Error));
        assert_eq!(
            LoggerMacro::from_name("error_unrestricted"),
            Some(LoggerMacro::ErrorUnrestricted)
        );
        assert_eq!(LoggerMacro::from_name("compile_error"), None);
        assert_eq!(LoggerMacro::from_name("__log_error"), None);
    }

    #[test]
    fn rejects_unknown_logger_class_fields() {
        let json = r#"{
            "id":"logger.api-request",
            "subsystem":"api",
            "summary":"request",
            "guest_triggerable":false,
            "delivery":"unrestricted-host",
            "level":"info",
            "module":"bangbang_runtime::api_server",
            "origin":"configurable",
            "limiter":"unrestricted",
            "allowed_fields":[],
            "disposition":"planned",
            "rationale":"planned",
            "typo":true
        }"#;
        let error = serde_json::from_str::<LoggerProducerClass>(json)
            .expect_err("unknown class fields must fail");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn rejects_out_of_vocabulary_logger_policy() {
        let error = serde_json::from_str::<LoggerClassDisposition>(r#""other""#)
            .expect_err("out-of-vocabulary disposition must fail");
        assert!(error.to_string().contains("unknown variant"));
    }
}
