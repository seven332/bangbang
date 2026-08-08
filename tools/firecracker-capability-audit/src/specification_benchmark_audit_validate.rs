use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::validate::{tracked_repository_files, validate_reference};
use crate::{
    FIRECRACKER_COMMIT, FIRECRACKER_TARGET, FIRECRACKER_VERSION, Reference,
    SpecificationBenchmarkAudit, SpecificationBenchmarkMeasurement, SpecificationBenchmarkNonclaim,
    SpecificationBenchmarkPolicy, SpecificationBenchmarkUpstreamSource, ValidationErrors,
};

/// Current specification-benchmark authority schema.
pub const SPECIFICATION_BENCHMARK_AUDIT_SCHEMA_VERSION: u32 = 1;
/// Repository-relative specification-benchmark authority path.
pub const SPECIFICATION_BENCHMARK_AUDIT_PATH: &str =
    "compat/firecracker/v1.16.0/specification-benchmark-audit.json";
/// Exact terminal capability scope owned by #1798.
pub const SPECIFICATION_BENCHMARK_CAPABILITY_IDS: [&str; 3] = [
    "corpus:network-performance",
    "corpus:specification",
    "semantic.specification:performance-resource-and-telemetry-outcomes",
];

const RUNNER_PATH: &str = "scripts/specification-benchmark.py";
const CONFIG_PATH: &str = "scripts/specification-benchmark-config.example.json";
const PUBLIC_DOC_PATH: &str = "docs/specification-benchmarks.md";
const CONTRACT_PATH: &str = "compat/firecracker/v1.16.0/specification-benchmark-contract.md";

const NONCLAIMS: [SpecificationBenchmarkNonclaim; 7] = [
    SpecificationBenchmarkNonclaim::FirecrackerOrAwsLinuxKvmParity,
    SpecificationBenchmarkNonclaim::PortableNumericThresholdOrRegressionVerdict,
    SpecificationBenchmarkNonclaim::GuestMapExcludingVmmOverhead,
    SpecificationBenchmarkNonclaim::CoremarkFioPingOrIperfEquivalence,
    SpecificationBenchmarkNonclaim::ControlledPageCacheOrBareMetalRatio,
    SpecificationBenchmarkNonclaim::NetworkAvailabilityCredentialsRecoveryOrCleanup,
    SpecificationBenchmarkNonclaim::TrackedHardwareReport,
];

struct MeasurementSpec {
    name: &'static str,
    method: &'static str,
    unit: &'static str,
    producer: &'static str,
    interpretation: &'static str,
}

const MEASUREMENTS: [MeasurementSpec; 10] = [
    MeasurementSpec {
        name: "process_startup_wall_us",
        method: "bangbang-initial-metrics-v1",
        unit: "microseconds",
        producer: "api_server.process_startup_time_us",
        interpretation: "signed process startup wall clock before retained guest work",
    },
    MeasurementSpec {
        name: "process_startup_cpu_us",
        method: "bangbang-initial-metrics-v1",
        unit: "microseconds",
        producer: "api_server.process_startup_time_cpu_us",
        interpretation: "signed process startup CPU clock before retained guest work",
    },
    MeasurementSpec {
        name: "whole_process_rss_kib",
        method: "ps-rss-kib-v1",
        unit: "kibibytes",
        producer: "/bin/ps -o rss= -p PID at the guest ready barrier",
        interpretation: "whole Bangbang process including guest mappings",
    },
    MeasurementSpec {
        name: "guest_init_wall_us",
        method: "bangbang-boot-timer-v1",
        unit: "microseconds",
        producer: "production Guest-boot-time wall clock",
        interpretation: "InstanceStart to checked guest boot-timer write",
    },
    MeasurementSpec {
        name: "guest_init_cpu_us",
        method: "bangbang-boot-timer-v1",
        unit: "microseconds",
        producer: "production Guest-boot-time CPU clock",
        interpretation: "process CPU consumed through the checked guest boot-timer write",
    },
    MeasurementSpec {
        name: "guest_compute_duration_ns",
        method: "guest-clock-monotonic-fixed-loop-v1",
        unit: "nanoseconds",
        producer: "/bangbang-specification-benchmark",
        interpretation: "fixed data-dependent loop guest clock, not CoreMark or bare-metal ratio",
    },
    MeasurementSpec {
        name: "guest_storage_duration_ns",
        method: "guest-clock-monotonic-sequential-root-read-v1",
        unit: "nanoseconds",
        producer: "/bangbang-specification-benchmark",
        interpretation: "fixed sequential read guest clock, not fio or uncached throughput",
    },
    MeasurementSpec {
        name: "metrics_fifo_filled_bytes",
        method: "nonblocking-sentinel-until-eagain-v1",
        unit: "bytes",
        producer: "collector-owned real metrics FIFO filler",
        interpretation: "host pipe capacity observation, not loss evidence alone",
    },
    MeasurementSpec {
        name: "metrics_fifo_drained_bytes",
        method: "drain-after-failed-flush-v1",
        unit: "bytes",
        producer: "collector-owned real metrics FIFO reader",
        interpretation: "sentinel and possible partial failed-publication bytes",
    },
    MeasurementSpec {
        name: "metrics_missed_count",
        method: "failed-flush-replay-counter-v1",
        unit: "count",
        producer: "logger.missed_metrics_count after one typed failure and retry",
        interpretation: "exact production lost-output replay accounting",
    },
];

/// Validate the exact checked #1798 authority and its local evidence.
pub fn validate_specification_benchmark_audit(
    audit: &SpecificationBenchmarkAudit,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    validate_baseline(audit, &mut errors);
    validate_upstream_sources(&audit.upstream_sources, &mut errors);
    validate_measurements(&audit.measurements, &mut errors);
    validate_policy(&audit.policy, &mut errors);
    validate_terminal_scope(audit, &mut errors);

    let tracked = tracked_repository_files(repository_root, &mut errors);
    validate_evidence(audit, repository_root, &tracked, &mut errors);
    validate_source_contracts(repository_root, &mut errors);

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

fn validate_baseline(audit: &SpecificationBenchmarkAudit, errors: &mut Vec<String>) {
    if audit.schema_version != SPECIFICATION_BENCHMARK_AUDIT_SCHEMA_VERSION {
        errors.push(format!(
            "specification benchmark audit schema_version must be {SPECIFICATION_BENCHMARK_AUDIT_SCHEMA_VERSION}"
        ));
    }
    if audit.baseline.version != FIRECRACKER_VERSION
        || audit.baseline.commit != FIRECRACKER_COMMIT
        || audit.baseline.target != FIRECRACKER_TARGET
    {
        errors.push("specification benchmark audit baseline is not pinned".to_string());
    }
    if audit.parent_issue != "#1798" || audit.delivery_issue != "#1877" {
        errors.push("specification benchmark audit ownership must be #1798/#1877".to_string());
    }
}

fn validate_upstream_sources(
    sources: &[SpecificationBenchmarkUpstreamSource],
    errors: &mut Vec<String>,
) {
    let expected = vec![
        SpecificationBenchmarkUpstreamSource {
            id: "specification".to_string(),
            path: "SPECIFICATION.md".to_string(),
            git_blob: "67ede9964f8a2d314b9cad69fe8d5b773e01b1d8".to_string(),
            environment: vec![
                "AWS M5d.metal and M6g.metal bare-metal hosts".to_string(),
                "Linux/KVM with sufficient CPU and memory".to_string(),
                "minimal guest and source-specific workload controls".to_string(),
            ],
            claims: vec![
                "API startup CPU time <= 8 ms".to_string(),
                "API startup wall time 6-60 ms, typically 12 ms".to_string(),
                "VMM overhead <= 5 MiB for 1 vCPU and 128 MiB after excluding guest mappings"
                    .to_string(),
                "guest boot <= 125 ms from InstanceStart to /sbin/init".to_string(),
                "full nonblocking metrics FIFO may lose output and increments missed_metrics_count"
                    .to_string(),
            ],
            pending: vec![
                "guest compute > 95% of bare metal".to_string(),
                "network throughput and latency integration coverage".to_string(),
                "storage throughput integration coverage".to_string(),
            ],
        },
        SpecificationBenchmarkUpstreamSource {
            id: "network-performance".to_string(),
            path: "docs/network-performance.md".to_string(),
            git_blob: "0be0d8cdd8dec6041286f36d3b33b7d7f8f4f437".to_string(),
            environment: vec![
                "AWS M5d.metal host in a VPC".to_string(),
                "Amazon Linux 2 host and guest with Linux kernel 4.14".to_string(),
                "ping, 10 Mbps background traffic, and multiple iperf3 clients".to_string(),
            ],
            claims: vec![
                "TCP throughput 14.5 Gbps at <= 80% CPU".to_string(),
                "TCP throughput 25 Gbps at 100% CPU".to_string(),
                "round-trip latency 0.06 ms".to_string(),
            ],
            pending: vec![
                "SPECIFICATION.md retains network performance integration coverage as pending"
                    .to_string(),
            ],
        },
    ];
    if sources != expected {
        errors.push(
            "specification benchmark audit requires the exact pinned upstream references"
                .to_string(),
        );
    }
}

fn validate_measurements(
    measurements: &[SpecificationBenchmarkMeasurement],
    errors: &mut Vec<String>,
) {
    if measurements.len() != MEASUREMENTS.len() {
        errors.push("specification benchmark audit requires exactly ten measurements".to_string());
        return;
    }
    for (measurement, expected) in measurements.iter().zip(MEASUREMENTS.iter()) {
        if measurement.name != expected.name
            || measurement.method != expected.method
            || measurement.unit != expected.unit
            || measurement.producer != expected.producer
            || measurement.interpretation != expected.interpretation
        {
            errors.push(format!(
                "specification benchmark measurement identity drifted: {}",
                measurement.name
            ));
        }
    }
}

fn validate_policy(policy: &SpecificationBenchmarkPolicy, errors: &mut Vec<String>) {
    let expected = SpecificationBenchmarkPolicy {
        runner: RUNNER_PATH.to_string(),
        config_example: CONFIG_PATH.to_string(),
        public_commands: vec![
            vec![
                RUNNER_PATH.to_string(),
                "collect".to_string(),
                "--config".to_string(),
                CONFIG_PATH.to_string(),
                "--output".to_string(),
                ".tmp/bangbang-specification-report.json".to_string(),
            ],
            vec![
                RUNNER_PATH.to_string(),
                "validate".to_string(),
                "--report".to_string(),
                ".tmp/bangbang-specification-report.json".to_string(),
            ],
            vec![
                RUNNER_PATH.to_string(),
                "compare".to_string(),
                "--previous".to_string(),
                "/path/to/previous.json".to_string(),
                "--current".to_string(),
                "/path/to/current.json".to_string(),
            ],
        ],
        build_command: vec![
            "cargo".to_string(),
            "build".to_string(),
            "--package".to_string(),
            "bangbang".to_string(),
            "--release".to_string(),
            "--locked".to_string(),
            "--no-default-features".to_string(),
            "--target".to_string(),
            "aarch64-apple-darwin".to_string(),
        ],
        platform: "Apple Silicon macOS with executable Hypervisor.framework; no unsupported bypass"
            .to_string(),
        sessions: vec![
            "independent signed workload session per warmup and iteration".to_string(),
            "independent signed real metrics FIFO session per warmup and iteration".to_string(),
        ],
        summary_fields: vec![
            "count".to_string(),
            "min".to_string(),
            "median".to_string(),
            "max".to_string(),
        ],
        comparison_key: "SHA-256 of environment, policy, ordered metric definitions, and optional fixture identity; raw values and summaries excluded".to_string(),
        publication: "canonical mode-0600 absent-only report after complete root-session cleanup"
            .to_string(),
        network_default: "network member absent".to_string(),
        network_fixture: "explicit credential-free no-shell bounded fixture with digest-pinned executable, matching labels, integer output, cleanup=complete, and no argv/environment in report".to_string(),
        ci: "portable schema, parser, fake transaction, real portable FIFO, fixture, cleanup, publication, and static audit only; no hardware threshold".to_string(),
        merged_main_gate: "fresh ignored networkless report from clean synchronized merged main with matching commit/tree/binary identity".to_string(),
    };
    if policy != &expected {
        errors.push("specification benchmark collection/report policy drifted".to_string());
    }
}

fn validate_terminal_scope(audit: &SpecificationBenchmarkAudit, errors: &mut Vec<String>) {
    if audit
        .capability_ids
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        != SPECIFICATION_BENCHMARK_CAPABILITY_IDS
    {
        errors.push(
            "specification benchmark audit requires the exact three capabilities".to_string(),
        );
    }
    if audit.nonclaims != NONCLAIMS {
        errors
            .push("specification benchmark audit requires the exact ordered nonclaims".to_string());
    }
}

fn validate_evidence(
    audit: &SpecificationBenchmarkAudit,
    repository_root: &Path,
    tracked: &BTreeSet<PathBuf>,
    errors: &mut Vec<String>,
) {
    for (kind, references) in [
        ("implementation", audit.evidence.implementation.as_slice()),
        ("validation", audit.evidence.validation.as_slice()),
        ("documentation", audit.evidence.documentation.as_slice()),
    ] {
        if references.is_empty() {
            errors.push(format!(
                "specification benchmark audit requires {kind} evidence"
            ));
        }
        if references
            .windows(2)
            .any(|pair| matches!(pair, [previous, current] if previous >= current))
        {
            errors.push(format!(
                "specification benchmark {kind} evidence must be unique and sorted"
            ));
        }
        for (index, reference) in references.iter().enumerate() {
            let label = format!("specification benchmark {kind}[{index}]");
            validate_reference(reference, repository_root, tracked, &label, errors);
            let Reference::Local {
                path,
                anchor: Some(anchor),
            } = reference
            else {
                errors.push(format!(
                    "specification benchmark evidence must be anchored local: {label}"
                ));
                continue;
            };
            let Ok(contents) = std::fs::read_to_string(repository_root.join(path)) else {
                continue;
            };
            if !contents.contains(anchor.as_str()) {
                errors.push(format!(
                    "specification benchmark evidence anchor is absent: {label}"
                ));
            }
        }
    }
}

fn validate_source_contracts(repository_root: &Path, errors: &mut Vec<String>) {
    let sources = [
        (
            RUNNER_PATH,
            &[
                "def collect_report(",
                "def validate_report_document(",
                "def comparison_document(",
                "EXPECTED_WOULD_BLOCK_FAULT",
                "--no-default-features",
            ][..],
        ),
        (
            CONFIG_PATH,
            &[
                "\"iterations\": 3",
                "\"tracing\": \"disabled\"",
                "\"warmups\": 1",
            ][..],
        ),
        (
            PUBLIC_DOC_PATH,
            &[
                "## Firecracker reference figures",
                "whole-process RSS",
                "There is no unsupported-host success mode",
                "does not certify #1378",
            ][..],
        ),
        (
            CONTRACT_PATH,
            &[
                "## Exact terminal capability set",
                "371 implemented-and-verified",
                "#1378",
            ][..],
        ),
    ];
    for (path, tokens) in sources {
        match std::fs::read_to_string(repository_root.join(path)) {
            Ok(contents) => {
                for token in tokens {
                    if !contents.contains(token) {
                        errors.push(format!(
                            "specification benchmark source {path} omits required token: {token}"
                        ));
                    }
                }
            }
            Err(_) => errors.push(format!(
                "specification benchmark source is unreadable: {path}"
            )),
        }
    }
}
