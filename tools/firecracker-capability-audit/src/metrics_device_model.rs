use serde::{Deserialize, Serialize};

use crate::{Baseline, PlatformExclusion, Reference};

/// Closed semantic producer boundary for a device-owned metrics field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MetricsDeviceProducerBoundary {
    Activation,
    ArchitectureRetained,
    Configuration,
    DataPath,
    DeviceState,
    InterruptLifecycle,
    Latency,
    MmdsDataPath,
    QueueEvent,
    RateLimiter,
    VcpuExit,
    VcpuFailure,
}

impl MetricsDeviceProducerBoundary {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Activation => "activation",
            Self::ArchitectureRetained => "architecture-retained",
            Self::Configuration => "configuration",
            Self::DataPath => "data-path",
            Self::DeviceState => "device-state",
            Self::InterruptLifecycle => "interrupt-lifecycle",
            Self::Latency => "latency",
            Self::MmdsDataPath => "mmds-data-path",
            Self::QueueEvent => "queue-event",
            Self::RateLimiter => "rate-limiter",
            Self::VcpuExit => "vcpu-exit",
            Self::VcpuFailure => "vcpu-failure",
        }
    }
}

/// Terminal or unresolved state of one exact device producer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MetricsDeviceProducerDisposition {
    Planned,
    ProvisionalPlatformZero,
    Implemented,
    SourceNeutral,
    PlatformZero,
}

impl MetricsDeviceProducerDisposition {
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Implemented | Self::SourceNeutral | Self::PlatformZero
        )
    }
}

/// Reviewed producer authority for one canonical device metrics field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsDeviceProducerRecord {
    pub field_id: String,
    pub boundary: MetricsDeviceProducerBoundary,
    pub disposition: MetricsDeviceProducerDisposition,
    pub delivery_issue: String,
    pub rationale: String,
    #[serde(default)]
    pub implementation: Vec<Reference>,
    #[serde(default)]
    pub validation: Vec<Reference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_exclusion: Option<PlatformExclusion>,
}

/// Human-reviewed exact-field producer overlay for device-owned metrics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsDeviceProducerAudit {
    pub schema_version: u32,
    pub baseline: Baseline,
    pub records: Vec<MetricsDeviceProducerRecord>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_device_audit_fields() {
        let error = serde_json::from_str::<MetricsDeviceProducerAudit>(
            r#"{
                "schema_version":1,
                "baseline":{"version":"1.16.0","commit":"c","target":"t"},
                "records":[],
                "typo":true
            }"#,
        )
        .expect_err("unknown audit fields must fail");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn rejects_open_ended_device_boundaries() {
        let error = serde_json::from_str::<MetricsDeviceProducerRecord>(
            r##"{
                "field_id":"static:test.field",
                "boundary":"somewhere",
                "disposition":"planned",
                "delivery_issue":"#1",
                "rationale":"test"
            }"##,
        )
        .expect_err("unknown boundaries must fail");
        assert!(error.to_string().contains("unknown variant"));
    }

    #[test]
    fn rejects_unknown_device_dispositions() {
        let error = serde_json::from_str::<MetricsDeviceProducerRecord>(
            r##"{
                "field_id":"static:test.field",
                "boundary":"data-path",
                "disposition":"deferred",
                "delivery_issue":"#1",
                "rationale":"test"
            }"##,
        )
        .expect_err("unknown dispositions must fail");
        assert!(error.to_string().contains("unknown variant"));
    }

    #[test]
    fn provisional_platform_zero_is_not_terminal() {
        assert!(!MetricsDeviceProducerDisposition::Planned.is_terminal());
        assert!(!MetricsDeviceProducerDisposition::ProvisionalPlatformZero.is_terminal());
        assert!(MetricsDeviceProducerDisposition::Implemented.is_terminal());
        assert!(MetricsDeviceProducerDisposition::SourceNeutral.is_terminal());
        assert!(MetricsDeviceProducerDisposition::PlatformZero.is_terminal());
    }
}
