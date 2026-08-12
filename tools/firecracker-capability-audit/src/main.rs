use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode};

use bangbang_firecracker_capability_audit::{
    AuditError, AuditMode, CAPABILITY_INVENTORY_PATH, CPU_TEMPLATE_HELPER_AUDIT_PATH,
    FORMAL_VERIFICATION_AUDIT_PATH, GUEST_WORKFLOW_AUDIT_PATH, HOST_RESOURCE_AUTHORITY_AUDIT_PATH,
    JAILER_AGGREGATE_AUDIT_PATH, JAILER_SECCOMP_CONTAINMENT_AUDIT_PATH, LOGGER_PRODUCER_AUDIT_PATH,
    LOGGER_PRODUCER_MANIFEST_PATH, METRICS_DEVICE_PRODUCER_AUDIT_PATH,
    METRICS_LIFECYCLE_AUDIT_PATH, METRICS_PROCESS_PRODUCER_AUDIT_PATH,
    METRICS_SCHEMA_AUTHORITY_PATH, MULTIPROCESS_ISOLATION_AUDIT_PATH, PRODUCTION_HOST_AUDIT_PATH,
    SOURCE_MANIFEST_PATH, SPECIFICATION_BENCHMARK_AUDIT_PATH, TRACING_AUDIT_PATH,
    WAVE7_AGGREGATE_AUDIT_PATH, WAVE8_CERTIFICATION_AUDIT_PATH, derive_logger_producer_manifest,
    derive_metrics_schema_source, derive_source_manifest, logger_producer_manifest_json,
    metrics_schema_source_candidate_json, read_capability_inventory,
    read_cpu_template_helper_audit, read_formal_verification_audit, read_guest_workflow_audit,
    read_host_resource_authority_audit, read_jailer_aggregate_audit,
    read_jailer_seccomp_containment_audit, read_logger_producer_audit,
    read_logger_producer_manifest, read_metrics_device_producer_audit,
    read_metrics_lifecycle_audit, read_metrics_process_producer_audit,
    read_metrics_schema_authority, read_multiprocess_isolation_audit, read_production_host_audit,
    read_source_manifest, read_specification_benchmark_audit, read_tracing_audit,
    read_wave7_aggregate_audit, read_wave8_certification_audit, source_manifest_json, validate,
    validate_cpu_template_compatibility, validate_cpu_template_fingerprint_compare_compatibility,
    validate_cpu_template_fingerprint_dump_compatibility, validate_cpu_template_helper_audit,
    validate_cpu_template_helper_compatibility, validate_cpu_template_helper_transition,
    validate_cpu_template_strip_compatibility, validate_formal_verification_audit,
    validate_formal_verification_compatibility, validate_guest_workflow_audit,
    validate_guest_workflow_compatibility, validate_host_resource_authority_audit,
    validate_host_resource_authority_compatibility, validate_jailer_aggregate_audit,
    validate_jailer_aggregate_compatibility, validate_jailer_seccomp_containment_audit,
    validate_jailer_seccomp_containment_compatibility, validate_logger_compatibility,
    validate_logger_producers, validate_metrics_compatibility,
    validate_metrics_device_compatibility, validate_metrics_device_producers,
    validate_metrics_lifecycle, validate_metrics_process_compatibility,
    validate_metrics_process_producers, validate_metrics_schema,
    validate_metrics_schema_compatibility, validate_multiprocess_isolation_audit,
    validate_multiprocess_isolation_compatibility, validate_production_host_audit,
    validate_production_host_compatibility, validate_production_host_upstream_source,
    validate_specification_benchmark_audit, validate_specification_benchmark_compatibility,
    validate_tracing_audit, validate_tracing_compatibility, validate_wave7_aggregate_audit,
    validate_wave7_aggregate_compatibility, validate_wave8_certification_audit,
    validate_wave8_certification_compatibility,
};

fn main() -> ExitCode {
    match run(env::args().skip(1).collect()) {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("firecracker capability audit failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> Result<String, AuditError> {
    let Some(command) = args.first().map(String::as_str) else {
        return Err(AuditError::new(usage()));
    };
    let command_args = args.get(1..).unwrap_or_default();
    match command {
        "validate" => run_validate(command_args),
        "compare" => run_compare(command_args),
        "regenerate" => run_regenerate(command_args),
        "regenerate-logger-producers" => run_regenerate_logger_producers(command_args),
        "regenerate-metrics-schema-source" => run_regenerate_metrics_schema_source(command_args),
        "help" | "--help" | "-h" => Ok(usage().to_string()),
        _ => Err(AuditError::new(format!(
            "unknown command: {command}\n{}",
            usage()
        ))),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValidateMode {
    Delivery,
    Final,
    LoggerFinal,
    TracingFinal,
    MetricsSchemaFinal,
    MetricsProcessFinal,
    MetricsDeviceFinal,
    MetricsFinal,
    CpuTemplateHelperFinal,
    CpuTemplateStripFinal,
    CpuTemplateFingerprintDumpFinal,
    CpuTemplateFingerprintCompareFinal,
    CpuTemplateFinal,
    GuestWorkflowFinal,
    JailerFinal,
    MultiprocessIsolationFinal,
    HostResourceAuthorityFinal,
    JailerSeccompContainmentFinal,
    ProductionHostFinal,
    FormalVerificationFinal,
    SpecificationBenchmarkFinal,
    Wave7Final,
    Wave8Final,
}

fn parse_validate_mode(args: &[String]) -> Result<ValidateMode, AuditError> {
    match args {
        [] => Ok(ValidateMode::Delivery),
        [flag] if flag == "--final" => Ok(ValidateMode::Final),
        [flag] if flag == "--logger-final" => Ok(ValidateMode::LoggerFinal),
        [flag] if flag == "--tracing-final" => Ok(ValidateMode::TracingFinal),
        [flag] if flag == "--metrics-schema-final" => Ok(ValidateMode::MetricsSchemaFinal),
        [flag] if flag == "--metrics-process-final" => Ok(ValidateMode::MetricsProcessFinal),
        [flag] if flag == "--metrics-device-final" => Ok(ValidateMode::MetricsDeviceFinal),
        [flag] if flag == "--metrics-final" => Ok(ValidateMode::MetricsFinal),
        [flag] if flag == "--cpu-template-helper-final" => Ok(ValidateMode::CpuTemplateHelperFinal),
        [flag] if flag == "--cpu-template-strip-final" => Ok(ValidateMode::CpuTemplateStripFinal),
        [flag] if flag == "--cpu-template-fingerprint-dump-final" => {
            Ok(ValidateMode::CpuTemplateFingerprintDumpFinal)
        }
        [flag] if flag == "--cpu-template-fingerprint-compare-final" => {
            Ok(ValidateMode::CpuTemplateFingerprintCompareFinal)
        }
        [flag] if flag == "--cpu-template-final" => Ok(ValidateMode::CpuTemplateFinal),
        [flag] if flag == "--guest-workflow-final" => Ok(ValidateMode::GuestWorkflowFinal),
        [flag] if flag == "--jailer-final" => Ok(ValidateMode::JailerFinal),
        [flag] if flag == "--multiprocess-isolation-final" => {
            Ok(ValidateMode::MultiprocessIsolationFinal)
        }
        [flag] if flag == "--host-resource-authority-final" => {
            Ok(ValidateMode::HostResourceAuthorityFinal)
        }
        [flag] if flag == "--jailer-seccomp-containment-final" => {
            Ok(ValidateMode::JailerSeccompContainmentFinal)
        }
        [flag] if flag == "--production-host-final" => Ok(ValidateMode::ProductionHostFinal),
        [flag] if flag == "--formal-verification-final" => {
            Ok(ValidateMode::FormalVerificationFinal)
        }
        [flag] if flag == "--specification-benchmark-final" => {
            Ok(ValidateMode::SpecificationBenchmarkFinal)
        }
        [flag] if flag == "--wave7-final" => Ok(ValidateMode::Wave7Final),
        [flag] if flag == "--wave8-final" => Ok(ValidateMode::Wave8Final),
        _ => Err(AuditError::new(
            "validate accepts only one optional --final, --logger-final, --tracing-final, --metrics-schema-final, --metrics-process-final, --metrics-device-final, --metrics-final, --cpu-template-helper-final, --cpu-template-strip-final, --cpu-template-fingerprint-dump-final, --cpu-template-fingerprint-compare-final, --cpu-template-final, --guest-workflow-final, --jailer-final, --multiprocess-isolation-final, --host-resource-authority-final, --jailer-seccomp-containment-final, --production-host-final, --formal-verification-final, --specification-benchmark-final, --wave7-final, or --wave8-final flag",
        )),
    }
}

fn run_validate(args: &[String]) -> Result<String, AuditError> {
    let mode = parse_validate_mode(args)?;
    let root = repository_root()?;
    let manifest = read_source_manifest(&root.join(SOURCE_MANIFEST_PATH))?;
    let inventory = read_capability_inventory(&root.join(CAPABILITY_INVENTORY_PATH))?;
    let logger_manifest = read_logger_producer_manifest(&root.join(LOGGER_PRODUCER_MANIFEST_PATH))?;
    let logger_audit = read_logger_producer_audit(&root.join(LOGGER_PRODUCER_AUDIT_PATH))?;
    let metrics_authority =
        read_metrics_schema_authority(&root.join(METRICS_SCHEMA_AUTHORITY_PATH))?;
    let metrics_process_audit =
        read_metrics_process_producer_audit(&root.join(METRICS_PROCESS_PRODUCER_AUDIT_PATH))?;
    let metrics_device_audit =
        read_metrics_device_producer_audit(&root.join(METRICS_DEVICE_PRODUCER_AUDIT_PATH))?;
    let metrics_lifecycle_audit =
        read_metrics_lifecycle_audit(&root.join(METRICS_LIFECYCLE_AUDIT_PATH))?;
    let tracing_audit = read_tracing_audit(&root.join(TRACING_AUDIT_PATH))?;
    let cpu_template_helper_audit =
        read_cpu_template_helper_audit(&root.join(CPU_TEMPLATE_HELPER_AUDIT_PATH))?;
    let guest_workflow_audit = read_guest_workflow_audit(&root.join(GUEST_WORKFLOW_AUDIT_PATH))?;
    let jailer_aggregate_audit =
        read_jailer_aggregate_audit(&root.join(JAILER_AGGREGATE_AUDIT_PATH))?;
    let multiprocess_isolation_audit =
        read_multiprocess_isolation_audit(&root.join(MULTIPROCESS_ISOLATION_AUDIT_PATH))?;
    let host_resource_authority_audit =
        read_host_resource_authority_audit(&root.join(HOST_RESOURCE_AUTHORITY_AUDIT_PATH))?;
    let jailer_seccomp_containment_audit =
        read_jailer_seccomp_containment_audit(&root.join(JAILER_SECCOMP_CONTAINMENT_AUDIT_PATH))?;
    let production_host_audit = read_production_host_audit(&root.join(PRODUCTION_HOST_AUDIT_PATH))?;
    let formal_verification_audit =
        read_formal_verification_audit(&root.join(FORMAL_VERIFICATION_AUDIT_PATH))?;
    let specification_benchmark_audit =
        read_specification_benchmark_audit(&root.join(SPECIFICATION_BENCHMARK_AUDIT_PATH))?;
    let wave7_aggregate_audit = read_wave7_aggregate_audit(&root.join(WAVE7_AGGREGATE_AUDIT_PATH))?;
    let wave8_certification_audit =
        read_wave8_certification_audit(&root.join(WAVE8_CERTIFICATION_AUDIT_PATH))?;
    validate_cpu_template_helper_transition(&inventory).map_err(|errors| {
        AuditError::new(format!(
            "CPU-template helper transition validation errors:\n{errors}"
        ))
    })?;
    validate_cpu_template_helper_audit(&cpu_template_helper_audit, &inventory, &root).map_err(
        |errors| {
            AuditError::new(format!(
                "CPU-template helper audit validation errors:\n{errors}"
            ))
        },
    )?;
    validate_guest_workflow_audit(&guest_workflow_audit, &inventory, &root).map_err(|errors| {
        AuditError::new(format!("guest workflow audit validation errors:\n{errors}"))
    })?;
    validate_jailer_aggregate_audit(&jailer_aggregate_audit, &manifest, &inventory, &root)
        .map_err(|errors| {
            AuditError::new(format!(
                "jailer aggregate audit validation errors:\n{errors}"
            ))
        })?;
    validate_multiprocess_isolation_audit(
        &multiprocess_isolation_audit,
        &manifest,
        &inventory,
        &root,
    )
    .map_err(|errors| {
        AuditError::new(format!(
            "multiprocess isolation audit validation errors:\n{errors}"
        ))
    })?;
    validate_host_resource_authority_audit(
        &host_resource_authority_audit,
        &manifest,
        &inventory,
        &root,
    )
    .map_err(|errors| {
        AuditError::new(format!(
            "host-resource authority audit validation errors:\n{errors}"
        ))
    })?;
    validate_jailer_seccomp_containment_audit(
        &jailer_seccomp_containment_audit,
        &manifest,
        &inventory,
        &root,
    )
    .map_err(|errors| {
        AuditError::new(format!(
            "jailer/seccomp containment audit validation errors:\n{errors}"
        ))
    })?;
    validate_production_host_audit(&production_host_audit, &manifest, &inventory, &root).map_err(
        |errors| {
            AuditError::new(format!(
                "production-host audit validation errors:\n{errors}"
            ))
        },
    )?;
    validate_formal_verification_audit(&formal_verification_audit, &root).map_err(|errors| {
        AuditError::new(format!(
            "formal verification audit validation errors:\n{errors}"
        ))
    })?;
    validate_specification_benchmark_audit(&specification_benchmark_audit, &root).map_err(
        |errors| {
            AuditError::new(format!(
                "specification benchmark audit validation errors:\n{errors}"
            ))
        },
    )?;
    validate_wave7_aggregate_audit(&wave7_aggregate_audit, &manifest, &inventory, &root).map_err(
        |errors| {
            AuditError::new(format!(
                "Wave 7 aggregate audit validation errors:\n{errors}"
            ))
        },
    )?;
    validate_wave8_certification_audit(&wave8_certification_audit, &manifest, &inventory, &root)
        .map_err(|errors| {
            AuditError::new(format!(
                "Wave 8 certification audit validation errors:\n{errors}"
            ))
        })?;
    let audit_mode = match mode {
        ValidateMode::Delivery => AuditMode::Delivery,
        ValidateMode::Final => AuditMode::Final,
        ValidateMode::LoggerFinal => {
            validate_logger_compatibility(
                &manifest,
                &inventory,
                &logger_manifest,
                &logger_audit,
                &root,
            )
            .map_err(|errors| {
                AuditError::new(format!("logger compatibility validation errors:\n{errors}"))
            })?;
            validate_metrics_schema(&metrics_authority, &manifest, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("metrics schema validation errors:\n{errors}"))
                })?;
            validate_metrics_process_producers(
                &metrics_process_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics process producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_device_producers(
                &metrics_device_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics device producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_lifecycle(
                &metrics_lifecycle_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!("metrics lifecycle validation errors:\n{errors}"))
            })?;
            validate_tracing_audit(&tracing_audit, &root, AuditMode::Delivery).map_err(
                |errors| AuditError::new(format!("tracing audit validation errors:\n{errors}")),
            )?;
            return Ok(
                "Firecracker capability inventory, logger producer audit, metrics schema authority, process producer audit, device producer audit, metrics lifecycle audit, and tracing audit are valid for the terminal logger compatibility scope"
                    .to_string(),
            );
        }
        ValidateMode::TracingFinal => {
            validate_tracing_compatibility(
                &manifest,
                &inventory,
                &logger_manifest,
                &logger_audit,
                &tracing_audit,
                &root,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "tracing compatibility validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_schema(&metrics_authority, &manifest, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("metrics schema validation errors:\n{errors}"))
                })?;
            validate_metrics_process_producers(
                &metrics_process_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics process producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_device_producers(
                &metrics_device_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics device producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_lifecycle(
                &metrics_lifecycle_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!("metrics lifecycle validation errors:\n{errors}"))
            })?;
            return Ok(
                "Firecracker capability inventory, logger producer audit, metrics schema authority, process producer audit, device producer audit, metrics lifecycle audit, and tracing audit are valid for the terminal tracing compatibility scope"
                    .to_string(),
            );
        }
        ValidateMode::MetricsSchemaFinal => {
            validate_metrics_schema_compatibility(&manifest, &inventory, &metrics_authority, &root)
                .map_err(|errors| {
                    AuditError::new(format!(
                        "metrics schema compatibility validation errors:\n{errors}"
                    ))
                })?;
            validate_logger_producers(&logger_manifest, &logger_audit, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("logger producer validation errors:\n{errors}"))
                })?;
            validate_metrics_process_producers(
                &metrics_process_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics process producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_device_producers(
                &metrics_device_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics device producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_lifecycle(
                &metrics_lifecycle_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!("metrics lifecycle validation errors:\n{errors}"))
            })?;
            validate_tracing_audit(&tracing_audit, &root, AuditMode::Delivery).map_err(
                |errors| AuditError::new(format!("tracing audit validation errors:\n{errors}")),
            )?;
            return Ok(
                "Firecracker capability inventory, logger producer audit, metrics schema authority, process producer audit, device producer audit, metrics lifecycle audit, and tracing audit are valid for the terminal metrics API/schema compatibility scope"
                    .to_string(),
            );
        }
        ValidateMode::MetricsProcessFinal => {
            validate_metrics_process_compatibility(
                &manifest,
                &inventory,
                &metrics_authority,
                &metrics_process_audit,
                &root,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics process compatibility validation errors:\n{errors}"
                ))
            })?;
            validate_logger_producers(&logger_manifest, &logger_audit, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("logger producer validation errors:\n{errors}"))
                })?;
            validate_metrics_device_producers(
                &metrics_device_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics device producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_lifecycle(
                &metrics_lifecycle_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!("metrics lifecycle validation errors:\n{errors}"))
            })?;
            validate_tracing_audit(&tracing_audit, &root, AuditMode::Delivery).map_err(
                |errors| AuditError::new(format!("tracing audit validation errors:\n{errors}")),
            )?;
            return Ok(
                "Firecracker capability inventory, logger producer audit, metrics schema authority, process producer audit, device producer audit, metrics lifecycle audit, and tracing audit are valid for the terminal process metrics compatibility scope"
                    .to_string(),
            );
        }
        ValidateMode::MetricsDeviceFinal => {
            validate_metrics_device_compatibility(
                &manifest,
                &inventory,
                &metrics_authority,
                &metrics_process_audit,
                &metrics_device_audit,
                &root,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics device compatibility validation errors:\n{errors}"
                ))
            })?;
            validate_logger_producers(&logger_manifest, &logger_audit, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("logger producer validation errors:\n{errors}"))
                })?;
            validate_metrics_lifecycle(
                &metrics_lifecycle_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!("metrics lifecycle validation errors:\n{errors}"))
            })?;
            validate_tracing_audit(&tracing_audit, &root, AuditMode::Delivery).map_err(
                |errors| AuditError::new(format!("tracing audit validation errors:\n{errors}")),
            )?;
            return Ok(
                "Firecracker capability inventory, logger producer audit, metrics schema authority, process producer audit, device producer audit, metrics lifecycle audit, and tracing audit are valid for the terminal device metrics compatibility scope"
                    .to_string(),
            );
        }
        ValidateMode::MetricsFinal => {
            validate_metrics_compatibility(
                &manifest,
                &inventory,
                &metrics_authority,
                &metrics_process_audit,
                &metrics_device_audit,
                &metrics_lifecycle_audit,
                &root,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics aggregate compatibility validation errors:\n{errors}"
                ))
            })?;
            validate_logger_producers(&logger_manifest, &logger_audit, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("logger producer validation errors:\n{errors}"))
                })?;
            validate_tracing_audit(&tracing_audit, &root, AuditMode::Delivery).map_err(
                |errors| AuditError::new(format!("tracing audit validation errors:\n{errors}")),
            )?;
            return Ok(
                "Firecracker capability inventory, logger producer audit, metrics schema authority, process producer audit, device producer audit, metrics lifecycle audit, and tracing audit are valid for the terminal aggregate metrics compatibility scope"
                    .to_string(),
            );
        }
        ValidateMode::CpuTemplateHelperFinal => {
            validate_cpu_template_helper_compatibility(&manifest, &inventory, &root).map_err(
                |errors| {
                    AuditError::new(format!(
                        "CPU-template helper compatibility validation errors:\n{errors}"
                    ))
                },
            )?;
            validate_logger_producers(&logger_manifest, &logger_audit, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("logger producer validation errors:\n{errors}"))
                })?;
            validate_metrics_schema(&metrics_authority, &manifest, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("metrics schema validation errors:\n{errors}"))
                })?;
            validate_metrics_process_producers(
                &metrics_process_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics process producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_device_producers(
                &metrics_device_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics device producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_lifecycle(
                &metrics_lifecycle_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!("metrics lifecycle validation errors:\n{errors}"))
            })?;
            validate_tracing_audit(&tracing_audit, &root, AuditMode::Delivery).map_err(
                |errors| AuditError::new(format!("tracing audit validation errors:\n{errors}")),
            )?;
            return Ok(
                "Firecracker capability inventory, CPU-template helper, logger producer audit, metrics schema authority, process producer audit, device producer audit, metrics lifecycle audit, and tracing audit are valid for the terminal CPU-template dump and verify compatibility scope"
                    .to_string(),
            );
        }
        ValidateMode::CpuTemplateStripFinal => {
            validate_cpu_template_strip_compatibility(&manifest, &inventory, &root).map_err(
                |errors| {
                    AuditError::new(format!(
                        "CPU-template strip compatibility validation errors:\n{errors}"
                    ))
                },
            )?;
            validate_logger_producers(&logger_manifest, &logger_audit, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("logger producer validation errors:\n{errors}"))
                })?;
            validate_metrics_schema(&metrics_authority, &manifest, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("metrics schema validation errors:\n{errors}"))
                })?;
            validate_metrics_process_producers(
                &metrics_process_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics process producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_device_producers(
                &metrics_device_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics device producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_lifecycle(
                &metrics_lifecycle_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!("metrics lifecycle validation errors:\n{errors}"))
            })?;
            validate_tracing_audit(&tracing_audit, &root, AuditMode::Delivery).map_err(
                |errors| AuditError::new(format!("tracing audit validation errors:\n{errors}")),
            )?;
            return Ok(
                "Firecracker capability inventory, CPU-template strip, logger producer audit, metrics schema authority, process producer audit, device producer audit, metrics lifecycle audit, and tracing audit are valid for the terminal portable CPU-template strip compatibility scope"
                    .to_string(),
            );
        }
        ValidateMode::CpuTemplateFingerprintDumpFinal => {
            validate_cpu_template_fingerprint_dump_compatibility(&manifest, &inventory, &root)
                .map_err(|errors| {
                    AuditError::new(format!(
                        "CPU-template fingerprint-dump compatibility validation errors:\n{errors}"
                    ))
                })?;
            validate_logger_producers(&logger_manifest, &logger_audit, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("logger producer validation errors:\n{errors}"))
                })?;
            validate_metrics_schema(&metrics_authority, &manifest, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("metrics schema validation errors:\n{errors}"))
                })?;
            validate_metrics_process_producers(
                &metrics_process_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics process producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_device_producers(
                &metrics_device_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics device producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_lifecycle(
                &metrics_lifecycle_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!("metrics lifecycle validation errors:\n{errors}"))
            })?;
            validate_tracing_audit(&tracing_audit, &root, AuditMode::Delivery).map_err(
                |errors| AuditError::new(format!("tracing audit validation errors:\n{errors}")),
            )?;
            return Ok(
                "Firecracker capability inventory, CPU-template fingerprint dump, logger producer audit, metrics schema authority, process producer audit, device producer audit, metrics lifecycle audit, and tracing audit are valid for the terminal platform-tagged CPU-fingerprint dump compatibility scope"
                    .to_string(),
            );
        }
        ValidateMode::CpuTemplateFingerprintCompareFinal => {
            validate_cpu_template_fingerprint_compare_compatibility(&manifest, &inventory, &root)
                .map_err(|errors| {
                AuditError::new(format!(
                    "CPU-template fingerprint-compare compatibility validation errors:\n{errors}"
                ))
            })?;
            validate_logger_producers(&logger_manifest, &logger_audit, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("logger producer validation errors:\n{errors}"))
                })?;
            validate_metrics_schema(&metrics_authority, &manifest, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("metrics schema validation errors:\n{errors}"))
                })?;
            validate_metrics_process_producers(
                &metrics_process_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics process producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_device_producers(
                &metrics_device_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics device producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_lifecycle(
                &metrics_lifecycle_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!("metrics lifecycle validation errors:\n{errors}"))
            })?;
            validate_tracing_audit(&tracing_audit, &root, AuditMode::Delivery).map_err(
                |errors| AuditError::new(format!("tracing audit validation errors:\n{errors}")),
            )?;
            return Ok(
                "Firecracker capability inventory, CPU-template fingerprint compare, logger producer audit, metrics schema authority, process producer audit, device producer audit, metrics lifecycle audit, and tracing audit are valid for the terminal deterministic CPU-fingerprint compare compatibility scope"
                    .to_string(),
            );
        }
        ValidateMode::CpuTemplateFinal => {
            validate_cpu_template_compatibility(
                &manifest,
                &inventory,
                &cpu_template_helper_audit,
                &root,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "CPU-template aggregate compatibility validation errors:\n{errors}"
                ))
            })?;
            validate_logger_producers(&logger_manifest, &logger_audit, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("logger producer validation errors:\n{errors}"))
                })?;
            validate_metrics_schema(&metrics_authority, &manifest, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("metrics schema validation errors:\n{errors}"))
                })?;
            validate_metrics_process_producers(
                &metrics_process_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics process producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_device_producers(
                &metrics_device_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics device producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_lifecycle(
                &metrics_lifecycle_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!("metrics lifecycle validation errors:\n{errors}"))
            })?;
            validate_tracing_audit(&tracing_audit, &root, AuditMode::Delivery).map_err(
                |errors| AuditError::new(format!("tracing audit validation errors:\n{errors}")),
            )?;
            return Ok(
                "Firecracker capability inventory, canonical CPU-template helper audit, logger producer audit, metrics schema authority, process producer audit, device producer audit, metrics lifecycle audit, and tracing audit are valid for the terminal aggregate CPU-template compatibility scope"
                    .to_string(),
            );
        }
        ValidateMode::GuestWorkflowFinal => {
            validate_guest_workflow_compatibility(
                &manifest,
                &inventory,
                &guest_workflow_audit,
                &root,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "guest workflow compatibility validation errors:\n{errors}"
                ))
            })?;
            validate_logger_producers(&logger_manifest, &logger_audit, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("logger producer validation errors:\n{errors}"))
                })?;
            validate_metrics_schema(&metrics_authority, &manifest, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("metrics schema validation errors:\n{errors}"))
                })?;
            validate_metrics_process_producers(
                &metrics_process_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics process producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_device_producers(
                &metrics_device_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics device producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_lifecycle(
                &metrics_lifecycle_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!("metrics lifecycle validation errors:\n{errors}"))
            })?;
            validate_tracing_audit(&tracing_audit, &root, AuditMode::Delivery).map_err(
                |errors| AuditError::new(format!("tracing audit validation errors:\n{errors}")),
            )?;
            return Ok(
                "Firecracker capability inventory, canonical guest workflow audit, logger producer audit, metrics schema authority, process producer audit, device producer audit, metrics lifecycle audit, and tracing audit are valid for the terminal macOS guest workflow compatibility scope"
                    .to_string(),
            );
        }
        ValidateMode::JailerFinal => {
            validate_jailer_aggregate_compatibility(
                &manifest,
                &inventory,
                &jailer_aggregate_audit,
                &root,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "jailer aggregate compatibility validation errors:\n{errors}"
                ))
            })?;
            validate_logger_producers(&logger_manifest, &logger_audit, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("logger producer validation errors:\n{errors}"))
                })?;
            validate_metrics_schema(&metrics_authority, &manifest, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("metrics schema validation errors:\n{errors}"))
                })?;
            validate_metrics_process_producers(
                &metrics_process_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics process producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_device_producers(
                &metrics_device_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics device producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_lifecycle(
                &metrics_lifecycle_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!("metrics lifecycle validation errors:\n{errors}"))
            })?;
            validate_tracing_audit(&tracing_audit, &root, AuditMode::Delivery).map_err(
                |errors| AuditError::new(format!("tracing audit validation errors:\n{errors}")),
            )?;
            return Ok(
                "Firecracker capability inventory and canonical aggregate jailer authority are valid for the terminal macOS jailer compatibility scope"
                    .to_string(),
            );
        }
        ValidateMode::MultiprocessIsolationFinal => {
            validate_multiprocess_isolation_compatibility(
                &manifest,
                &inventory,
                &multiprocess_isolation_audit,
                &root,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "multiprocess isolation compatibility validation errors:\n{errors}"
                ))
            })?;
            validate_logger_producers(&logger_manifest, &logger_audit, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("logger producer validation errors:\n{errors}"))
                })?;
            validate_metrics_schema(&metrics_authority, &manifest, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("metrics schema validation errors:\n{errors}"))
                })?;
            validate_metrics_process_producers(
                &metrics_process_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics process producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_device_producers(
                &metrics_device_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics device producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_lifecycle(
                &metrics_lifecycle_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!("metrics lifecycle validation errors:\n{errors}"))
            })?;
            validate_tracing_audit(&tracing_audit, &root, AuditMode::Delivery).map_err(
                |errors| AuditError::new(format!("tracing audit validation errors:\n{errors}")),
            )?;
            return Ok(
                "Firecracker capability inventory and canonical multiprocess isolation authority are valid for the terminal macOS multiprocess isolation scope"
                    .to_string(),
            );
        }
        ValidateMode::HostResourceAuthorityFinal => {
            validate_host_resource_authority_compatibility(
                &manifest,
                &inventory,
                &host_resource_authority_audit,
                &root,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "host-resource authority compatibility validation errors:\n{errors}"
                ))
            })?;
            validate_logger_producers(&logger_manifest, &logger_audit, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("logger producer validation errors:\n{errors}"))
                })?;
            validate_metrics_schema(&metrics_authority, &manifest, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("metrics schema validation errors:\n{errors}"))
                })?;
            validate_metrics_process_producers(
                &metrics_process_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics process producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_device_producers(
                &metrics_device_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics device producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_lifecycle(
                &metrics_lifecycle_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!("metrics lifecycle validation errors:\n{errors}"))
            })?;
            validate_tracing_audit(&tracing_audit, &root, AuditMode::Delivery).map_err(
                |errors| AuditError::new(format!("tracing audit validation errors:\n{errors}")),
            )?;
            return Ok(
                "Firecracker capability inventory and canonical host-resource authority are valid for the terminal macOS host-resource authority scope"
                    .to_string(),
            );
        }
        ValidateMode::JailerSeccompContainmentFinal => {
            validate_jailer_seccomp_containment_compatibility(
                &manifest,
                &inventory,
                &jailer_seccomp_containment_audit,
                &root,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "jailer/seccomp containment compatibility validation errors:\n{errors}"
                ))
            })?;
            validate_logger_producers(&logger_manifest, &logger_audit, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("logger producer validation errors:\n{errors}"))
                })?;
            validate_metrics_schema(&metrics_authority, &manifest, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("metrics schema validation errors:\n{errors}"))
                })?;
            validate_metrics_process_producers(
                &metrics_process_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics process producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_device_producers(
                &metrics_device_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics device producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_lifecycle(
                &metrics_lifecycle_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!("metrics lifecycle validation errors:\n{errors}"))
            })?;
            validate_tracing_audit(&tracing_audit, &root, AuditMode::Delivery).map_err(
                |errors| AuditError::new(format!("tracing audit validation errors:\n{errors}")),
            )?;
            return Ok(
                "Firecracker capability inventory and canonical jailer/seccomp containment authority are valid for the terminal fixed macOS containment scope"
                    .to_string(),
            );
        }
        ValidateMode::ProductionHostFinal => {
            validate_production_host_compatibility(
                &manifest,
                &inventory,
                &production_host_audit,
                &root,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "production-host compatibility validation errors:\n{errors}"
                ))
            })?;
            validate_logger_producers(&logger_manifest, &logger_audit, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("logger producer validation errors:\n{errors}"))
                })?;
            validate_metrics_schema(&metrics_authority, &manifest, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("metrics schema validation errors:\n{errors}"))
                })?;
            validate_metrics_process_producers(
                &metrics_process_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics process producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_device_producers(
                &metrics_device_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics device producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_lifecycle(
                &metrics_lifecycle_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!("metrics lifecycle validation errors:\n{errors}"))
            })?;
            validate_tracing_audit(&tracing_audit, &root, AuditMode::Delivery).map_err(
                |errors| AuditError::new(format!("tracing audit validation errors:\n{errors}")),
            )?;
            return Ok(
                "Firecracker capability inventory and canonical production-host authority are valid for the terminal production-host corpus scope"
                    .to_string(),
            );
        }
        ValidateMode::FormalVerificationFinal => {
            validate_formal_verification_compatibility(
                &manifest,
                &inventory,
                &formal_verification_audit,
                &root,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "formal verification compatibility validation errors:\n{errors}"
                ))
            })?;
            validate_logger_producers(&logger_manifest, &logger_audit, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("logger producer validation errors:\n{errors}"))
                })?;
            validate_metrics_schema(&metrics_authority, &manifest, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("metrics schema validation errors:\n{errors}"))
                })?;
            validate_metrics_process_producers(
                &metrics_process_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics process producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_device_producers(
                &metrics_device_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics device producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_lifecycle(
                &metrics_lifecycle_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!("metrics lifecycle validation errors:\n{errors}"))
            })?;
            validate_tracing_audit(&tracing_audit, &root, AuditMode::Delivery).map_err(
                |errors| AuditError::new(format!("tracing audit validation errors:\n{errors}")),
            )?;
            return Ok(
                "Firecracker capability inventory, canonical formal verification audit, logger producer audit, metrics schema authority, process producer audit, device producer audit, metrics lifecycle audit, and tracing audit are valid for the terminal targeted Kani compatibility scope"
                    .to_string(),
            );
        }
        ValidateMode::SpecificationBenchmarkFinal => {
            validate_specification_benchmark_compatibility(
                &manifest,
                &inventory,
                &specification_benchmark_audit,
                &root,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "specification benchmark compatibility validation errors:\n{errors}"
                ))
            })?;
            validate_logger_producers(&logger_manifest, &logger_audit, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("logger producer validation errors:\n{errors}"))
                })?;
            validate_metrics_schema(&metrics_authority, &manifest, &root, AuditMode::Delivery)
                .map_err(|errors| {
                    AuditError::new(format!("metrics schema validation errors:\n{errors}"))
                })?;
            validate_metrics_process_producers(
                &metrics_process_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics process producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_device_producers(
                &metrics_device_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics device producer validation errors:\n{errors}"
                ))
            })?;
            validate_metrics_lifecycle(
                &metrics_lifecycle_audit,
                &metrics_authority,
                &root,
                AuditMode::Delivery,
            )
            .map_err(|errors| {
                AuditError::new(format!("metrics lifecycle validation errors:\n{errors}"))
            })?;
            validate_tracing_audit(&tracing_audit, &root, AuditMode::Delivery).map_err(
                |errors| AuditError::new(format!("tracing audit validation errors:\n{errors}")),
            )?;
            return Ok(
                "Firecracker capability inventory, canonical specification benchmark audit, logger producer audit, metrics schema authority, process producer audit, device producer audit, metrics lifecycle audit, and tracing audit are valid for the terminal threshold-free specification benchmark scope"
                    .to_string(),
            );
        }
        ValidateMode::Wave7Final | ValidateMode::Wave8Final => {
            validate_logger_compatibility(
                &manifest,
                &inventory,
                &logger_manifest,
                &logger_audit,
                &root,
            )
            .map_err(|errors| {
                AuditError::new(format!("logger compatibility validation errors:\n{errors}"))
            })?;
            validate_metrics_compatibility(
                &manifest,
                &inventory,
                &metrics_authority,
                &metrics_process_audit,
                &metrics_device_audit,
                &metrics_lifecycle_audit,
                &root,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "metrics aggregate compatibility validation errors:\n{errors}"
                ))
            })?;
            validate_tracing_compatibility(
                &manifest,
                &inventory,
                &logger_manifest,
                &logger_audit,
                &tracing_audit,
                &root,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "tracing compatibility validation errors:\n{errors}"
                ))
            })?;
            validate_cpu_template_compatibility(
                &manifest,
                &inventory,
                &cpu_template_helper_audit,
                &root,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "CPU-template aggregate compatibility validation errors:\n{errors}"
                ))
            })?;
            validate_guest_workflow_compatibility(
                &manifest,
                &inventory,
                &guest_workflow_audit,
                &root,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "guest workflow compatibility validation errors:\n{errors}"
                ))
            })?;
            validate_formal_verification_compatibility(
                &manifest,
                &inventory,
                &formal_verification_audit,
                &root,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "formal verification compatibility validation errors:\n{errors}"
                ))
            })?;
            validate_specification_benchmark_compatibility(
                &manifest,
                &inventory,
                &specification_benchmark_audit,
                &root,
            )
            .map_err(|errors| {
                AuditError::new(format!(
                    "specification benchmark compatibility validation errors:\n{errors}"
                ))
            })?;
            validate_wave7_aggregate_compatibility(
                &manifest,
                &inventory,
                &wave7_aggregate_audit,
                &root,
            )
            .map_err(|errors| {
                AuditError::new(format!("Wave 7 aggregate compatibility errors:\n{errors}"))
            })?;
            if mode == ValidateMode::Wave8Final {
                validate_wave8_certification_compatibility(
                    &manifest,
                    &inventory,
                    &wave8_certification_audit,
                    &root,
                )
                .map_err(|errors| {
                    AuditError::new(format!(
                        "Wave 8 certification compatibility errors:\n{errors}"
                    ))
                })?;
                return Ok(
                    "Firecracker capability inventory and all component authorities are valid for the exact terminal Wave 8 platform-feasible scope"
                        .to_string(),
                );
            }
            return Ok(
                "Firecracker capability inventory and all Wave 7 component authorities are valid for the exact terminal Wave 7 aggregate scope"
                    .to_string(),
            );
        }
    };
    let mut failures = Vec::new();
    if let Err(errors) = validate(&manifest, &inventory, &root, audit_mode) {
        failures.push(format!("inventory validation errors:\n{errors}"));
    }
    if let Err(errors) =
        validate_logger_producers(&logger_manifest, &logger_audit, &root, audit_mode)
    {
        failures.push(format!("logger producer validation errors:\n{errors}"));
    }
    if let Err(errors) = validate_metrics_schema(&metrics_authority, &manifest, &root, audit_mode) {
        failures.push(format!("metrics schema validation errors:\n{errors}"));
    }
    if let Err(errors) = validate_metrics_process_producers(
        &metrics_process_audit,
        &metrics_authority,
        &root,
        audit_mode,
    ) {
        failures.push(format!(
            "metrics process producer validation errors:\n{errors}"
        ));
    }
    if let Err(errors) = validate_metrics_device_producers(
        &metrics_device_audit,
        &metrics_authority,
        &root,
        audit_mode,
    ) {
        failures.push(format!(
            "metrics device producer validation errors:\n{errors}"
        ));
    }
    if let Err(errors) = validate_metrics_lifecycle(
        &metrics_lifecycle_audit,
        &metrics_authority,
        &root,
        audit_mode,
    ) {
        failures.push(format!("metrics lifecycle validation errors:\n{errors}"));
    }
    if let Err(errors) = validate_tracing_audit(&tracing_audit, &root, audit_mode) {
        failures.push(format!("tracing audit validation errors:\n{errors}"));
    }
    if !failures.is_empty() {
        return Err(AuditError::new(failures.join("\n")));
    }
    let mode_name = match audit_mode {
        AuditMode::Delivery => "delivery",
        AuditMode::Final => "final",
    };
    Ok(format!(
        "Firecracker capability inventory, canonical CPU-template helper, guest-workflow, jailer aggregate, multiprocess isolation, host-resource authority, jailer/seccomp containment, production-host, formal-verification, specification-benchmark, Wave 7 aggregate, and Wave 8 certification audits, logger producer audit, metrics schema authority, process producer audit, device producer audit, metrics lifecycle audit, and tracing audit are valid in {mode_name} mode"
    ))
}

fn run_compare(args: &[String]) -> Result<String, AuditError> {
    let firecracker = required_option(args, "--firecracker")?;
    let root = repository_root()?;
    let checked_in = read_source_manifest(&root.join(SOURCE_MANIFEST_PATH))?;
    let derived = derive_source_manifest(Path::new(&firecracker))?;
    let checked_logger = read_logger_producer_manifest(&root.join(LOGGER_PRODUCER_MANIFEST_PATH))?;
    let derived_logger = derive_logger_producer_manifest(Path::new(&firecracker))?;
    let checked_metrics = read_metrics_schema_authority(&root.join(METRICS_SCHEMA_AUTHORITY_PATH))?
        .source_candidate();
    let derived_metrics = derive_metrics_schema_source(Path::new(&firecracker))?;
    let production_host_audit = read_production_host_audit(&root.join(PRODUCTION_HOST_AUDIT_PATH))?;
    let mut differences = Vec::new();
    if checked_in != derived {
        let checked_json = String::from_utf8(source_manifest_json(&checked_in)?)
            .map_err(|_| AuditError::new("checked source manifest JSON is not valid UTF-8"))?;
        let derived_json = String::from_utf8(source_manifest_json(&derived)?)
            .map_err(|_| AuditError::new("derived source manifest JSON is not valid UTF-8"))?;
        differences.push(format!(
            "derived source manifest differs from {SOURCE_MANIFEST_PATH}; run regenerate to an explicit candidate path\n{}",
            canonical_line_diff(&checked_json, &derived_json)
        ));
    }
    if checked_logger != derived_logger {
        let checked_json = String::from_utf8(logger_producer_manifest_json(&checked_logger)?)
            .map_err(|_| {
                AuditError::new("checked logger producer manifest JSON is not valid UTF-8")
            })?;
        let derived_json = String::from_utf8(logger_producer_manifest_json(&derived_logger)?)
            .map_err(|_| {
                AuditError::new("derived logger producer manifest JSON is not valid UTF-8")
            })?;
        differences.push(format!(
            "derived logger producer manifest differs from {LOGGER_PRODUCER_MANIFEST_PATH}; run regenerate-logger-producers to an explicit candidate path\n{}",
            canonical_line_diff(&checked_json, &derived_json)
        ));
    }
    if checked_metrics != derived_metrics {
        let checked_json = String::from_utf8(metrics_schema_source_candidate_json(
            &checked_metrics,
        )?)
        .map_err(|_| AuditError::new("checked metrics schema source JSON is not valid UTF-8"))?;
        let derived_json = String::from_utf8(metrics_schema_source_candidate_json(
            &derived_metrics,
        )?)
        .map_err(|_| AuditError::new("derived metrics schema source JSON is not valid UTF-8"))?;
        differences.push(format!(
            "derived metrics schema source differs from {METRICS_SCHEMA_AUTHORITY_PATH}; run regenerate-metrics-schema-source to an explicit candidate path\n{}",
            canonical_line_diff(&checked_json, &derived_json)
        ));
    }
    if let Err(errors) =
        validate_production_host_upstream_source(&production_host_audit, Path::new(&firecracker))
    {
        differences.push(format!(
            "production-host clause anchors differ from the pinned source:\n{errors}"
        ));
    }
    if differences.is_empty() {
        Ok(
            "checked-in source, production-host clause, logger producer, and metrics schema authorities match the pinned Firecracker checkout"
                .to_string(),
        )
    } else {
        Err(AuditError::new(differences.join("\n")))
    }
}

fn canonical_line_diff(checked: &str, derived: &str) -> String {
    let checked_lines: Vec<&str> = checked.lines().collect();
    let derived_lines: Vec<&str> = derived.lines().collect();
    let line_count = checked_lines.len().max(derived_lines.len());
    let mut differences = Vec::new();
    for index in 0..line_count {
        let checked_line = checked_lines.get(index).copied();
        let derived_line = derived_lines.get(index).copied();
        if checked_line != derived_line {
            differences.push(format!(
                "line {}: checked={checked_line:?}; derived={derived_line:?}",
                index + 1
            ));
        }
    }
    differences.join("\n")
}

fn run_regenerate(args: &[String]) -> Result<String, AuditError> {
    let options = required_options(args, &["--firecracker", "--output"])?;
    let firecracker = options
        .get("--firecracker")
        .ok_or_else(|| AuditError::new("--firecracker is required"))?;
    let output = options
        .get("--output")
        .ok_or_else(|| AuditError::new("--output is required"))?;
    let root = repository_root()?;
    let output_path = candidate_output_path(&root, Path::new(output))?;
    let derived = derive_source_manifest(Path::new(firecracker))?;
    let bytes = source_manifest_json(&derived)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output_path)
        .map_err(|error| AuditError::new(format!("failed to create candidate output: {error}")))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| AuditError::new(format!("failed to write candidate output: {error}")))?;
    Ok(format!(
        "generated source manifest candidate: {}",
        output_path.display()
    ))
}

fn run_regenerate_logger_producers(args: &[String]) -> Result<String, AuditError> {
    let options = required_options(args, &["--firecracker", "--output"])?;
    let firecracker = options
        .get("--firecracker")
        .ok_or_else(|| AuditError::new("--firecracker is required"))?;
    let output = options
        .get("--output")
        .ok_or_else(|| AuditError::new("--output is required"))?;
    let root = repository_root()?;
    let output_path = candidate_output_path(&root, Path::new(output))?;
    let derived = derive_logger_producer_manifest(Path::new(firecracker))?;
    let bytes = logger_producer_manifest_json(&derived)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output_path)
        .map_err(|error| AuditError::new(format!("failed to create candidate output: {error}")))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| AuditError::new(format!("failed to write candidate output: {error}")))?;
    Ok(format!(
        "generated logger producer manifest candidate: {}",
        output_path.display()
    ))
}

fn run_regenerate_metrics_schema_source(args: &[String]) -> Result<String, AuditError> {
    let options = required_options(args, &["--firecracker", "--output"])?;
    let firecracker = options
        .get("--firecracker")
        .ok_or_else(|| AuditError::new("--firecracker is required"))?;
    let output = options
        .get("--output")
        .ok_or_else(|| AuditError::new("--output is required"))?;
    let root = repository_root()?;
    let output_path = candidate_output_path(&root, Path::new(output))?;
    let derived = derive_metrics_schema_source(Path::new(firecracker))?;
    let bytes = metrics_schema_source_candidate_json(&derived)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output_path)
        .map_err(|error| AuditError::new(format!("failed to create candidate output: {error}")))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| AuditError::new(format!("failed to write candidate output: {error}")))?;
    Ok(format!(
        "generated metrics schema source candidate: {}",
        output_path.display()
    ))
}

fn candidate_output_path(root: &Path, output: &Path) -> Result<PathBuf, AuditError> {
    let output_path = absolute_from(root, output);
    let source_path = root.join(SOURCE_MANIFEST_PATH);
    let inventory_path = root.join(CAPABILITY_INVENTORY_PATH);
    let logger_manifest_path = root.join(LOGGER_PRODUCER_MANIFEST_PATH);
    let logger_audit_path = root.join(LOGGER_PRODUCER_AUDIT_PATH);
    let metrics_schema_path = root.join(METRICS_SCHEMA_AUTHORITY_PATH);
    let metrics_process_audit_path = root.join(METRICS_PROCESS_PRODUCER_AUDIT_PATH);
    let metrics_device_audit_path = root.join(METRICS_DEVICE_PRODUCER_AUDIT_PATH);
    let metrics_lifecycle_audit_path = root.join(METRICS_LIFECYCLE_AUDIT_PATH);
    let tracing_audit_path = root.join(TRACING_AUDIT_PATH);
    let cpu_template_helper_audit_path = root.join(CPU_TEMPLATE_HELPER_AUDIT_PATH);
    let guest_workflow_audit_path = root.join(GUEST_WORKFLOW_AUDIT_PATH);
    let jailer_aggregate_audit_path = root.join(JAILER_AGGREGATE_AUDIT_PATH);
    let multiprocess_isolation_audit_path = root.join(MULTIPROCESS_ISOLATION_AUDIT_PATH);
    let host_resource_authority_audit_path = root.join(HOST_RESOURCE_AUTHORITY_AUDIT_PATH);
    let jailer_seccomp_containment_audit_path = root.join(JAILER_SECCOMP_CONTAINMENT_AUDIT_PATH);
    let production_host_audit_path = root.join(PRODUCTION_HOST_AUDIT_PATH);
    let formal_verification_audit_path = root.join(FORMAL_VERIFICATION_AUDIT_PATH);
    let specification_benchmark_audit_path = root.join(SPECIFICATION_BENCHMARK_AUDIT_PATH);
    let wave7_aggregate_audit_path = root.join(WAVE7_AGGREGATE_AUDIT_PATH);
    let wave8_certification_audit_path = root.join(WAVE8_CERTIFICATION_AUDIT_PATH);
    let normalized_output = normalize_lexically(&output_path);
    let checked_paths = [
        &source_path,
        &inventory_path,
        &logger_manifest_path,
        &logger_audit_path,
        &metrics_schema_path,
        &metrics_process_audit_path,
        &metrics_device_audit_path,
        &metrics_lifecycle_audit_path,
        &tracing_audit_path,
        &cpu_template_helper_audit_path,
        &guest_workflow_audit_path,
        &jailer_aggregate_audit_path,
        &multiprocess_isolation_audit_path,
        &host_resource_authority_audit_path,
        &jailer_seccomp_containment_audit_path,
        &production_host_audit_path,
        &formal_verification_audit_path,
        &specification_benchmark_audit_path,
        &wave7_aggregate_audit_path,
        &wave8_certification_audit_path,
    ];
    if checked_paths
        .iter()
        .any(|path| normalized_output == normalize_lexically(path))
    {
        return Err(AuditError::new(
            "regenerate requires a separate candidate output and never overwrites checked-in inventory files",
        ));
    }
    if std::fs::symlink_metadata(&output_path).is_ok() {
        return Err(AuditError::new("candidate output already exists"));
    }
    let parent = output_path
        .parent()
        .ok_or_else(|| AuditError::new("candidate output must have a parent directory"))?;
    if !parent.is_dir() {
        return Err(AuditError::new(
            "candidate output parent directory does not exist",
        ));
    }
    let file_name = output_path
        .file_name()
        .ok_or_else(|| AuditError::new("candidate output must name a file"))?;
    let canonical_parent = parent
        .canonicalize()
        .map_err(|_| AuditError::new("candidate output parent directory is not accessible"))?;
    let effective_output = canonical_parent.join(file_name);
    for checked_path in checked_paths {
        if checked_path
            .canonicalize()
            .is_ok_and(|canonical| canonical == effective_output)
        {
            return Err(AuditError::new(
                "regenerate requires a separate candidate output and never overwrites checked-in inventory files",
            ));
        }
    }
    Ok(output_path)
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

fn required_option(args: &[String], name: &str) -> Result<String, AuditError> {
    let values = required_options(args, &[name])?;
    values
        .get(name)
        .cloned()
        .ok_or_else(|| AuditError::new(format!("{name} is required")))
}

fn required_options<'a>(
    args: &[String],
    names: &[&'a str],
) -> Result<std::collections::BTreeMap<&'a str, String>, AuditError> {
    let mut values = std::collections::BTreeMap::new();
    let mut index = 0;
    while index < args.len() {
        let argument = args
            .get(index)
            .ok_or_else(|| AuditError::new("invalid argument index"))?;
        let Some(name) = names.iter().copied().find(|name| argument == name) else {
            return Err(AuditError::new(format!("unknown argument: {argument}")));
        };
        if values.contains_key(name) {
            return Err(AuditError::new(format!("duplicate argument: {name}")));
        }
        let value = args
            .get(index + 1)
            .filter(|value| !value.starts_with("--"))
            .ok_or_else(|| AuditError::new(format!("{name} requires a value")))?;
        values.insert(name, value.clone());
        index += 2;
    }
    for name in names {
        if !values.contains_key(name) {
            return Err(AuditError::new(format!("{name} is required")));
        }
    }
    Ok(values)
}

fn repository_root() -> Result<PathBuf, AuditError> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|error| AuditError::new(format!("failed to locate repository root: {error}")))?;
    if !output.status.success() {
        return Err(AuditError::new(
            "current directory is not in a Git worktree",
        ));
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|_| AuditError::new("repository root is not valid UTF-8"))?;
    PathBuf::from(text.trim())
        .canonicalize()
        .map_err(|_| AuditError::new("repository root is not accessible"))
}

fn absolute_from(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn usage() -> &'static str {
    "Usage:\n  bangbang-firecracker-capability-audit validate [--final | --logger-final | --tracing-final | --metrics-schema-final | --metrics-process-final | --metrics-device-final | --metrics-final | --cpu-template-helper-final | --cpu-template-strip-final | --cpu-template-fingerprint-dump-final | --cpu-template-fingerprint-compare-final | --cpu-template-final | --guest-workflow-final | --jailer-final | --multiprocess-isolation-final | --host-resource-authority-final | --jailer-seccomp-containment-final | --production-host-final | --formal-verification-final | --specification-benchmark-final | --wave7-final | --wave8-final]\n  bangbang-firecracker-capability-audit compare --firecracker PATH\n  bangbang-firecracker-capability-audit regenerate --firecracker PATH --output PATH\n  bangbang-firecracker-capability-audit regenerate-logger-producers --firecracker PATH --output PATH\n  bangbang-firecracker-capability-audit regenerate-metrics-schema-source --firecracker PATH --output PATH"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_help() {
        assert!(
            run(vec!["--help".to_string()])
                .expect("help should work")
                .contains("Usage:")
        );
    }

    #[test]
    fn parses_exact_validate_modes() {
        assert_eq!(parse_validate_mode(&[]).unwrap(), ValidateMode::Delivery);
        assert_eq!(
            parse_validate_mode(&["--final".to_string()]).unwrap(),
            ValidateMode::Final
        );
        assert_eq!(
            parse_validate_mode(&["--logger-final".to_string()]).unwrap(),
            ValidateMode::LoggerFinal
        );
        assert_eq!(
            parse_validate_mode(&["--tracing-final".to_string()]).unwrap(),
            ValidateMode::TracingFinal
        );
        assert_eq!(
            parse_validate_mode(&["--metrics-schema-final".to_string()]).unwrap(),
            ValidateMode::MetricsSchemaFinal
        );
        assert_eq!(
            parse_validate_mode(&["--metrics-process-final".to_string()]).unwrap(),
            ValidateMode::MetricsProcessFinal
        );
        assert_eq!(
            parse_validate_mode(&["--metrics-device-final".to_string()]).unwrap(),
            ValidateMode::MetricsDeviceFinal
        );
        assert_eq!(
            parse_validate_mode(&["--metrics-final".to_string()]).unwrap(),
            ValidateMode::MetricsFinal
        );
        assert_eq!(
            parse_validate_mode(&["--cpu-template-helper-final".to_string()]).unwrap(),
            ValidateMode::CpuTemplateHelperFinal
        );
        assert_eq!(
            parse_validate_mode(&["--cpu-template-strip-final".to_string()]).unwrap(),
            ValidateMode::CpuTemplateStripFinal
        );
        assert_eq!(
            parse_validate_mode(&["--cpu-template-fingerprint-dump-final".to_string()]).unwrap(),
            ValidateMode::CpuTemplateFingerprintDumpFinal
        );
        assert_eq!(
            parse_validate_mode(&["--cpu-template-fingerprint-compare-final".to_string()]).unwrap(),
            ValidateMode::CpuTemplateFingerprintCompareFinal
        );
        assert_eq!(
            parse_validate_mode(&["--cpu-template-final".to_string()]).unwrap(),
            ValidateMode::CpuTemplateFinal
        );
        assert_eq!(
            parse_validate_mode(&["--guest-workflow-final".to_string()]).unwrap(),
            ValidateMode::GuestWorkflowFinal
        );
        assert_eq!(
            parse_validate_mode(&["--jailer-final".to_string()]).unwrap(),
            ValidateMode::JailerFinal
        );
        assert_eq!(
            parse_validate_mode(&["--multiprocess-isolation-final".to_string()]).unwrap(),
            ValidateMode::MultiprocessIsolationFinal
        );
        assert_eq!(
            parse_validate_mode(&["--host-resource-authority-final".to_string()]).unwrap(),
            ValidateMode::HostResourceAuthorityFinal
        );
        assert_eq!(
            parse_validate_mode(&["--jailer-seccomp-containment-final".to_string()]).unwrap(),
            ValidateMode::JailerSeccompContainmentFinal
        );
        assert_eq!(
            parse_validate_mode(&["--production-host-final".to_string()]).unwrap(),
            ValidateMode::ProductionHostFinal
        );
        assert_eq!(
            parse_validate_mode(&["--formal-verification-final".to_string()]).unwrap(),
            ValidateMode::FormalVerificationFinal
        );
        assert_eq!(
            parse_validate_mode(&["--specification-benchmark-final".to_string()]).unwrap(),
            ValidateMode::SpecificationBenchmarkFinal
        );
        assert_eq!(
            parse_validate_mode(&["--wave7-final".to_string()]).unwrap(),
            ValidateMode::Wave7Final
        );
        assert_eq!(
            parse_validate_mode(&["--wave8-final".to_string()]).unwrap(),
            ValidateMode::Wave8Final
        );

        for invalid in [
            vec!["--unknown".to_string()],
            vec!["--final".to_string(), "--logger-final".to_string()],
            vec!["--logger-final".to_string(), "--tracing-final".to_string()],
            vec![
                "--logger-final".to_string(),
                "--metrics-schema-final".to_string(),
            ],
            vec![
                "--metrics-schema-final".to_string(),
                "--metrics-process-final".to_string(),
            ],
            vec![
                "--metrics-process-final".to_string(),
                "--metrics-device-final".to_string(),
            ],
            vec![
                "--metrics-device-final".to_string(),
                "--metrics-final".to_string(),
            ],
            vec![
                "--metrics-final".to_string(),
                "--cpu-template-helper-final".to_string(),
            ],
            vec![
                "--cpu-template-helper-final".to_string(),
                "--cpu-template-strip-final".to_string(),
            ],
            vec![
                "--cpu-template-strip-final".to_string(),
                "--cpu-template-fingerprint-dump-final".to_string(),
            ],
            vec![
                "--cpu-template-fingerprint-dump-final".to_string(),
                "--cpu-template-fingerprint-compare-final".to_string(),
            ],
            vec![
                "--cpu-template-fingerprint-compare-final".to_string(),
                "--cpu-template-final".to_string(),
            ],
            vec![
                "--guest-workflow-final".to_string(),
                "--formal-verification-final".to_string(),
            ],
            vec![
                "--formal-verification-final".to_string(),
                "--specification-benchmark-final".to_string(),
            ],
            vec![
                "--specification-benchmark-final".to_string(),
                "--wave7-final".to_string(),
            ],
            vec!["--wave7-final".to_string(), "--wave8-final".to_string()],
        ] {
            let error = parse_validate_mode(&invalid).expect_err("mode should be rejected");
            assert!(error.to_string().contains("accepts only one optional"));
        }
    }

    #[test]
    fn completed_scoped_modes_consume_device_audit_in_delivery_mode() {
        for flag in [
            "--logger-final",
            "--tracing-final",
            "--metrics-schema-final",
            "--metrics-process-final",
        ] {
            let message = run_validate(&[flag.to_string()])
                .expect("completed scoped validation must remain available");
            assert!(message.contains("device producer audit"), "{flag}");
        }
    }

    #[test]
    fn device_final_mode_certifies_the_terminal_device_scope() {
        let message = run_validate(&["--metrics-device-final".to_string()])
            .expect("terminal device validation must pass");
        assert!(message.contains("terminal device metrics compatibility scope"));
    }

    #[test]
    fn tracing_final_mode_certifies_the_terminal_tracing_scope() {
        let message = run_validate(&["--tracing-final".to_string()])
            .expect("terminal tracing validation must pass");
        assert!(message.contains("terminal tracing compatibility scope"));
    }

    #[test]
    fn metrics_final_mode_certifies_the_terminal_aggregate_scope() {
        let message = run_validate(&["--metrics-final".to_string()])
            .expect("terminal aggregate metrics validation must pass");
        assert!(message.contains("terminal aggregate metrics compatibility scope"));
    }

    #[test]
    fn cpu_template_helper_final_mode_certifies_the_terminal_scope() {
        let message = run_validate(&["--cpu-template-helper-final".to_string()])
            .expect("terminal CPU-template helper validation must pass");
        assert!(message.contains("terminal CPU-template dump and verify compatibility scope"));
    }

    #[test]
    fn cpu_template_strip_final_mode_certifies_the_terminal_scope() {
        let message = run_validate(&["--cpu-template-strip-final".to_string()])
            .expect("terminal CPU-template strip validation must pass");
        assert!(message.contains("terminal portable CPU-template strip compatibility scope"));
    }

    #[test]
    fn cpu_template_fingerprint_dump_final_mode_certifies_the_terminal_scope() {
        let message = run_validate(&["--cpu-template-fingerprint-dump-final".to_string()])
            .expect("terminal CPU-template fingerprint dump validation must pass");
        assert!(message.contains("terminal platform-tagged CPU-fingerprint dump compatibility"));
    }

    #[test]
    fn cpu_template_fingerprint_compare_final_mode_certifies_the_terminal_scope() {
        let message = run_validate(&["--cpu-template-fingerprint-compare-final".to_string()])
            .expect("terminal CPU-template fingerprint compare validation must pass");
        assert!(message.contains("terminal deterministic CPU-fingerprint compare compatibility"));
    }

    #[test]
    fn cpu_template_final_mode_certifies_the_terminal_aggregate_scope() {
        let message = run_validate(&["--cpu-template-final".to_string()])
            .expect("terminal aggregate CPU-template validation must pass");
        assert!(message.contains("terminal aggregate CPU-template compatibility scope"));
    }

    #[test]
    fn specification_benchmark_final_mode_certifies_the_terminal_scope() {
        let message = run_validate(&["--specification-benchmark-final".to_string()])
            .expect("terminal specification benchmark validation must pass");
        assert!(message.contains("terminal threshold-free specification benchmark scope"));
    }

    #[test]
    fn jailer_final_mode_certifies_the_terminal_aggregate_scope() {
        let message = run_validate(&["--jailer-final".to_string()])
            .expect("terminal aggregate jailer validation must pass");
        assert!(message.contains("terminal macOS jailer compatibility scope"));
    }

    #[test]
    fn multiprocess_isolation_final_mode_certifies_the_terminal_scope() {
        let message = run_validate(&["--multiprocess-isolation-final".to_string()])
            .expect("terminal multiprocess isolation validation must pass");
        assert!(message.contains("terminal macOS multiprocess isolation scope"));
    }

    #[test]
    fn host_resource_authority_final_mode_certifies_the_terminal_scope() {
        let message = run_validate(&["--host-resource-authority-final".to_string()])
            .expect("terminal host-resource authority validation must pass");
        assert!(message.contains("terminal macOS host-resource authority scope"));
    }

    #[test]
    fn jailer_seccomp_containment_final_mode_certifies_the_terminal_scope() {
        let message = run_validate(&["--jailer-seccomp-containment-final".to_string()])
            .expect("terminal jailer/seccomp containment validation must pass");
        assert!(message.contains("terminal fixed macOS containment scope"));
    }

    #[test]
    fn production_host_final_mode_certifies_the_terminal_scope() {
        let message = run_validate(&["--production-host-final".to_string()])
            .expect("terminal production-host validation must pass");
        assert!(message.contains("terminal production-host corpus scope"));
    }

    #[test]
    fn wave7_final_mode_certifies_the_terminal_aggregate_scope() {
        let message = run_validate(&["--wave7-final".to_string()])
            .expect("terminal Wave 7 aggregate validation must pass");
        assert!(message.contains("exact terminal Wave 7 aggregate scope"));
    }

    #[test]
    fn wave8_final_mode_certifies_the_platform_feasible_scope() {
        let message = run_validate(&["--wave8-final".to_string()])
            .expect("terminal Wave 8 validation must pass");
        assert!(message.contains("terminal Wave 8 platform-feasible scope"));
    }

    #[test]
    fn rejects_duplicate_options() {
        let error = required_options(
            &[
                "--firecracker".to_string(),
                "one".to_string(),
                "--firecracker".to_string(),
                "two".to_string(),
            ],
            &["--firecracker"],
        )
        .expect_err("duplicate should fail");
        assert!(error.to_string().contains("duplicate argument"));
    }

    #[test]
    fn rejects_missing_option_value() {
        let error = required_option(&["--firecracker".to_string()], "--firecracker")
            .expect_err("missing value should fail");
        assert!(error.to_string().contains("requires a value"));
    }

    #[test]
    fn canonical_diff_reports_changed_and_missing_lines() {
        let diff = canonical_line_diff("one\ntwo\n", "one\nchanged\nthree\n");
        assert!(diff.contains("line 2: checked=Some(\"two\"); derived=Some(\"changed\")"));
        assert!(diff.contains("line 3: checked=None; derived=Some(\"three\")"));
    }

    #[test]
    fn regeneration_refuses_all_checked_inventory_files() {
        let root = Path::new("/repository");
        for path in [
            SOURCE_MANIFEST_PATH,
            CAPABILITY_INVENTORY_PATH,
            LOGGER_PRODUCER_MANIFEST_PATH,
            LOGGER_PRODUCER_AUDIT_PATH,
            METRICS_SCHEMA_AUTHORITY_PATH,
            METRICS_PROCESS_PRODUCER_AUDIT_PATH,
            METRICS_DEVICE_PRODUCER_AUDIT_PATH,
            METRICS_LIFECYCLE_AUDIT_PATH,
            TRACING_AUDIT_PATH,
            CPU_TEMPLATE_HELPER_AUDIT_PATH,
            GUEST_WORKFLOW_AUDIT_PATH,
            JAILER_AGGREGATE_AUDIT_PATH,
            MULTIPROCESS_ISOLATION_AUDIT_PATH,
            HOST_RESOURCE_AUTHORITY_AUDIT_PATH,
            JAILER_SECCOMP_CONTAINMENT_AUDIT_PATH,
            FORMAL_VERIFICATION_AUDIT_PATH,
            SPECIFICATION_BENCHMARK_AUDIT_PATH,
            WAVE7_AGGREGATE_AUDIT_PATH,
            WAVE8_CERTIFICATION_AUDIT_PATH,
        ] {
            let error = candidate_output_path(root, Path::new(path))
                .expect_err("checked inventory path should be refused");
            assert!(error.to_string().contains("never overwrites"));
        }
    }

    #[test]
    fn regeneration_refuses_lexical_aliases_of_checked_inventory_files() {
        let root = Path::new("/repository");
        for path in [
            "compat/firecracker/../firecracker/v1.16.0/source-manifest.json",
            "compat/firecracker/v1.16.0/./capabilities.json",
            "compat/firecracker/v1.16.0/./logger-producer-manifest.json",
            "compat/firecracker/v1.16.0/../v1.16.0/logger-producer-audit.json",
            "compat/firecracker/v1.16.0/./metrics-schema.json",
            "compat/firecracker/v1.16.0/./metrics-process-producer-audit.json",
            "compat/firecracker/v1.16.0/./metrics-device-producer-audit.json",
            "compat/firecracker/v1.16.0/./metrics-lifecycle-audit.json",
            "compat/firecracker/v1.16.0/./tracing-audit.json",
            "compat/firecracker/v1.16.0/./cpu-template-helper-audit.json",
            "compat/firecracker/v1.16.0/./guest-workflow-audit.json",
            "compat/firecracker/v1.16.0/./jailer-seccomp-containment-audit.json",
            "compat/firecracker/v1.16.0/./formal-verification-audit.json",
            "compat/firecracker/v1.16.0/./specification-benchmark-audit.json",
            "compat/firecracker/v1.16.0/./wave7-aggregate-audit.json",
            "compat/firecracker/v1.16.0/./wave8-certification-audit.json",
        ] {
            let error = candidate_output_path(root, Path::new(path))
                .expect_err("checked inventory alias should be refused");
            assert!(error.to_string().contains("never overwrites"));
        }
    }

    #[test]
    fn regeneration_refuses_an_existing_destination() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let error = candidate_output_path(root, Path::new("Cargo.toml"))
            .expect_err("an existing candidate destination must be refused");
        assert!(error.to_string().contains("already exists"));
    }
}
