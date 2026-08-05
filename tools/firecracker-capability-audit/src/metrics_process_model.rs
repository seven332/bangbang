use serde::{Deserialize, Serialize};

use crate::{Baseline, Reference};

/// Closed semantic producer boundary for a process-owned metrics field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MetricsProcessProducerBoundary {
    RequestParserEntry,
    RequestParserFailure,
    AcceptedDeprecatedApiValue,
    ProcessStartup,
    SuccessfulOuterApiOperation,
    SuccessfulInnerVmmOperation,
    LoggerLifecycle,
    SignalLifecycle,
    PanicLifecycle,
    SeccompFault,
}

/// Terminal or unresolved state of one exact process producer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MetricsProcessProducerDisposition {
    Planned,
    Implemented,
    SourceNeutral,
    PlatformZero,
}

impl MetricsProcessProducerDisposition {
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Planned)
    }
}

/// Reviewed producer authority for one canonical metrics field identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsProcessProducerRecord {
    pub field_id: String,
    pub boundary: MetricsProcessProducerBoundary,
    pub disposition: MetricsProcessProducerDisposition,
    pub delivery_issue: String,
    pub rationale: String,
    #[serde(default)]
    pub implementation: Vec<Reference>,
    #[serde(default)]
    pub validation: Vec<Reference>,
}

/// Human-reviewed exact-field producer overlay for process-owned metrics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsProcessProducerAudit {
    pub schema_version: u32,
    pub baseline: Baseline,
    pub records: Vec<MetricsProcessProducerRecord>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_process_audit_fields() {
        let error = serde_json::from_str::<MetricsProcessProducerAudit>(
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
    fn rejects_open_ended_process_boundaries() {
        let error = serde_json::from_str::<MetricsProcessProducerRecord>(
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
}
