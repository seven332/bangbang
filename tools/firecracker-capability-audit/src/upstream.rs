use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::{
    AuditError, Baseline, Counts, FIRECRACKER_COMMIT, FIRECRACKER_TARGET, FIRECRACKER_VERSION,
    GENERATOR_VERSION, Input, SCHEMA_VERSION, SourceItem, SourceManifest,
};

const SWAGGER_PATH: &str = "src/firecracker/swagger/firecracker.yaml";
const FIRECRACKER_MAIN_PATH: &str = "src/firecracker/src/main.rs";
const PARSED_REQUEST_PATH: &str = "src/firecracker/src/api_server/parsed_request.rs";

#[derive(Clone, Copy)]
struct StaticItem {
    kind: &'static str,
    key: &'static str,
    path: &'static str,
    anchor: &'static str,
    family: &'static str,
}

const TOOL_OPERATIONS: &[StaticItem] = &[
    tool_operation(
        "firecracker/run",
        "src/firecracker/src/main.rs",
        "build_arg_parser",
        "process",
    ),
    tool_operation(
        "jailer/run",
        "src/jailer/src/main.rs",
        "build_arg_parser",
        "isolation",
    ),
    tool_operation(
        "snapshot-editor/edit-memory/rebase",
        "src/snapshot-editor/src/edit_memory.rs",
        "EditMemorySubCommand::Rebase",
        "snapshots",
    ),
    tool_operation(
        "snapshot-editor/edit-vmstate/remove-regs",
        "src/snapshot-editor/src/edit_vmstate.rs",
        "EditVmStateSubCommand::RemoveRegs",
        "snapshots",
    ),
    tool_operation(
        "snapshot-editor/info-vmstate/version",
        "src/snapshot-editor/src/info.rs",
        "InfoVmStateSubCommand::Version",
        "snapshots",
    ),
    tool_operation(
        "snapshot-editor/info-vmstate/vcpu-states",
        "src/snapshot-editor/src/info.rs",
        "InfoVmStateSubCommand::VcpuStates",
        "snapshots",
    ),
    tool_operation(
        "snapshot-editor/info-vmstate/vm-state",
        "src/snapshot-editor/src/info.rs",
        "InfoVmStateSubCommand::VmState",
        "snapshots",
    ),
    tool_operation(
        "rebase-snap/rebase",
        "src/rebase-snap/src/main.rs",
        "rebase",
        "snapshots",
    ),
    tool_operation(
        "cpu-template-helper/template/dump",
        "src/cpu-template-helper/src/main.rs",
        "TemplateOperation::Dump",
        "cpu-and-machine",
    ),
    tool_operation(
        "cpu-template-helper/template/strip",
        "src/cpu-template-helper/src/main.rs",
        "TemplateOperation::Strip",
        "cpu-and-machine",
    ),
    tool_operation(
        "cpu-template-helper/template/verify",
        "src/cpu-template-helper/src/main.rs",
        "TemplateOperation::Verify",
        "cpu-and-machine",
    ),
    tool_operation(
        "cpu-template-helper/fingerprint/dump",
        "src/cpu-template-helper/src/main.rs",
        "FingerprintOperation::Dump",
        "cpu-and-machine",
    ),
    tool_operation(
        "cpu-template-helper/fingerprint/compare",
        "src/cpu-template-helper/src/main.rs",
        "FingerprintOperation::Compare",
        "cpu-and-machine",
    ),
    tool_operation(
        "seccompiler/compile",
        "src/seccompiler/src/bin.rs",
        "compile_bpf",
        "isolation",
    ),
];

const TOOL_ARGUMENTS: &[StaticItem] = &[
    tool_argument("jailer/id", "src/jailer/src/main.rs", "--id", "isolation"),
    tool_argument(
        "jailer/exec-file",
        "src/jailer/src/main.rs",
        "--exec-file",
        "isolation",
    ),
    tool_argument("jailer/uid", "src/jailer/src/main.rs", "--uid", "isolation"),
    tool_argument("jailer/gid", "src/jailer/src/main.rs", "--gid", "isolation"),
    tool_argument(
        "jailer/chroot-base-dir",
        "src/jailer/src/main.rs",
        "--chroot-base-dir",
        "isolation",
    ),
    tool_argument(
        "jailer/netns",
        "src/jailer/src/main.rs",
        "--netns",
        "isolation",
    ),
    tool_argument(
        "jailer/daemonize",
        "src/jailer/src/main.rs",
        "--daemonize",
        "isolation",
    ),
    tool_argument(
        "jailer/new-pid-ns",
        "src/jailer/src/main.rs",
        "--new-pid-ns",
        "isolation",
    ),
    tool_argument(
        "jailer/cgroup",
        "src/jailer/src/main.rs",
        "--cgroup",
        "isolation",
    ),
    tool_argument(
        "jailer/resource-limit",
        "src/jailer/src/main.rs",
        "--resource-limit",
        "isolation",
    ),
    tool_argument(
        "jailer/cgroup-version",
        "src/jailer/src/main.rs",
        "--cgroup-version",
        "isolation",
    ),
    tool_argument(
        "jailer/parent-cgroup",
        "src/jailer/src/main.rs",
        "--parent-cgroup",
        "isolation",
    ),
    tool_argument(
        "jailer/version",
        "src/jailer/src/main.rs",
        "--version",
        "isolation",
    ),
    tool_argument(
        "snapshot-editor/edit-memory/rebase/memory-path",
        "src/snapshot-editor/src/edit_memory.rs",
        "--memory-path",
        "snapshots",
    ),
    tool_argument(
        "snapshot-editor/edit-memory/rebase/diff-path",
        "src/snapshot-editor/src/edit_memory.rs",
        "--diff-path",
        "snapshots",
    ),
    tool_argument(
        "snapshot-editor/edit-vmstate/remove-regs/regs",
        "src/snapshot-editor/src/edit_vmstate.rs",
        "regs",
        "snapshots",
    ),
    tool_argument(
        "snapshot-editor/edit-vmstate/remove-regs/vmstate-path",
        "src/snapshot-editor/src/edit_vmstate.rs",
        "--vmstate-path",
        "snapshots",
    ),
    tool_argument(
        "snapshot-editor/edit-vmstate/remove-regs/output-path",
        "src/snapshot-editor/src/edit_vmstate.rs",
        "--output-path",
        "snapshots",
    ),
    tool_argument(
        "snapshot-editor/info-vmstate/version/vmstate-path",
        "src/snapshot-editor/src/info.rs",
        "Version.vmstate_path",
        "snapshots",
    ),
    tool_argument(
        "snapshot-editor/info-vmstate/vcpu-states/vmstate-path",
        "src/snapshot-editor/src/info.rs",
        "VcpuStates.vmstate_path",
        "snapshots",
    ),
    tool_argument(
        "snapshot-editor/info-vmstate/vm-state/vmstate-path",
        "src/snapshot-editor/src/info.rs",
        "VmState.vmstate_path",
        "snapshots",
    ),
    tool_argument(
        "rebase-snap/base-file",
        "src/rebase-snap/src/main.rs",
        "--base-file",
        "snapshots",
    ),
    tool_argument(
        "rebase-snap/diff-file",
        "src/rebase-snap/src/main.rs",
        "--diff-file",
        "snapshots",
    ),
    tool_argument(
        "cpu-template-helper/template/dump/config",
        "src/cpu-template-helper/src/main.rs",
        "TemplateOperation::Dump.config",
        "cpu-and-machine",
    ),
    tool_argument(
        "cpu-template-helper/template/dump/template",
        "src/cpu-template-helper/src/main.rs",
        "TemplateOperation::Dump.template",
        "cpu-and-machine",
    ),
    tool_argument(
        "cpu-template-helper/template/dump/output",
        "src/cpu-template-helper/src/main.rs",
        "TemplateOperation::Dump.output",
        "cpu-and-machine",
    ),
    tool_argument(
        "cpu-template-helper/template/strip/paths",
        "src/cpu-template-helper/src/main.rs",
        "TemplateOperation::Strip.paths",
        "cpu-and-machine",
    ),
    tool_argument(
        "cpu-template-helper/template/strip/suffix",
        "src/cpu-template-helper/src/main.rs",
        "TemplateOperation::Strip.suffix",
        "cpu-and-machine",
    ),
    tool_argument(
        "cpu-template-helper/template/verify/config",
        "src/cpu-template-helper/src/main.rs",
        "TemplateOperation::Verify.config",
        "cpu-and-machine",
    ),
    tool_argument(
        "cpu-template-helper/template/verify/template",
        "src/cpu-template-helper/src/main.rs",
        "TemplateOperation::Verify.template",
        "cpu-and-machine",
    ),
    tool_argument(
        "cpu-template-helper/fingerprint/dump/config",
        "src/cpu-template-helper/src/main.rs",
        "FingerprintOperation::Dump.config",
        "cpu-and-machine",
    ),
    tool_argument(
        "cpu-template-helper/fingerprint/dump/template",
        "src/cpu-template-helper/src/main.rs",
        "FingerprintOperation::Dump.template",
        "cpu-and-machine",
    ),
    tool_argument(
        "cpu-template-helper/fingerprint/dump/output",
        "src/cpu-template-helper/src/main.rs",
        "FingerprintOperation::Dump.output",
        "cpu-and-machine",
    ),
    tool_argument(
        "cpu-template-helper/fingerprint/compare/prev",
        "src/cpu-template-helper/src/main.rs",
        "FingerprintOperation::Compare.prev",
        "cpu-and-machine",
    ),
    tool_argument(
        "cpu-template-helper/fingerprint/compare/curr",
        "src/cpu-template-helper/src/main.rs",
        "FingerprintOperation::Compare.curr",
        "cpu-and-machine",
    ),
    tool_argument(
        "cpu-template-helper/fingerprint/compare/filters",
        "src/cpu-template-helper/src/main.rs",
        "FingerprintOperation::Compare.filters",
        "cpu-and-machine",
    ),
    tool_argument(
        "seccompiler/target-arch",
        "src/seccompiler/src/bin.rs",
        "--target-arch",
        "isolation",
    ),
    tool_argument(
        "seccompiler/input-file",
        "src/seccompiler/src/bin.rs",
        "--input-file",
        "isolation",
    ),
    tool_argument(
        "seccompiler/output-file",
        "src/seccompiler/src/bin.rs",
        "--output-file",
        "isolation",
    ),
    tool_argument(
        "seccompiler/basic",
        "src/seccompiler/src/bin.rs",
        "--basic",
        "isolation",
    ),
    tool_argument(
        "seccompiler/split-output",
        "src/seccompiler/src/bin.rs",
        "--split-output",
        "isolation",
    ),
];

const CORPUS: &[StaticItem] = &[
    corpus(
        "specification",
        "SPECIFICATION.md",
        "entire-file",
        "specifications",
    ),
    corpus(
        "release-changelog",
        "CHANGELOG.md",
        "v1.16.0",
        "specifications",
    ),
    corpus("design", "docs/design.md", "entire-file", "process"),
    corpus(
        "getting-started",
        "docs/getting-started.md",
        "entire-file",
        "process",
    ),
    corpus(
        "actions-api",
        "docs/api_requests/actions.md",
        "entire-file",
        "api-contract",
    ),
    corpus(
        "block-caching",
        "docs/api_requests/block-caching.md",
        "entire-file",
        "storage",
    ),
    corpus(
        "block-io-engine",
        "docs/api_requests/block-io-engine.md",
        "entire-file",
        "storage",
    ),
    corpus(
        "block-vhost-user",
        "docs/api_requests/block-vhost-user.md",
        "entire-file",
        "storage",
    ),
    corpus(
        "patch-block",
        "docs/api_requests/patch-block.md",
        "entire-file",
        "storage",
    ),
    corpus(
        "patch-network-interface",
        "docs/api_requests/patch-network-interface.md",
        "entire-file",
        "network-and-mmds",
    ),
    corpus(
        "ballooning",
        "docs/ballooning.md",
        "entire-file",
        "memory-devices",
    ),
    corpus(
        "cpu-boot-protocol",
        "docs/cpu_templates/boot-protocol.md",
        "entire-file",
        "cpu-and-machine",
    ),
    corpus(
        "cpu-template-helper",
        "docs/cpu_templates/cpu-template-helper.md",
        "entire-file",
        "cpu-and-machine",
    ),
    corpus(
        "cpu-templates",
        "docs/cpu_templates/cpu-templates.md",
        "entire-file",
        "cpu-and-machine",
    ),
    corpus(
        "device-api",
        "docs/device-api.md",
        "entire-file",
        "device-transport",
    ),
    corpus(
        "device-hotplug",
        "docs/device-hotplug.md",
        "entire-file",
        "runtime-device-management",
    ),
    corpus(
        "entropy",
        "docs/entropy.md",
        "entire-file",
        "remaining-devices",
    ),
    corpus(
        "formal-verification",
        "docs/formal-verification.md",
        "entire-file",
        "specifications",
    ),
    corpus(
        "hugepages",
        "docs/hugepages.md",
        "entire-file",
        "machine-memory",
    ),
    corpus("jailer", "docs/jailer.md", "entire-file", "isolation"),
    corpus("logger", "docs/logger.md", "entire-file", "observability"),
    corpus(
        "memory-hotplug",
        "docs/memory-hotplug.md",
        "entire-file",
        "memory-devices",
    ),
    corpus("metrics", "docs/metrics.md", "entire-file", "observability"),
    corpus(
        "mmds-design",
        "docs/mmds/mmds-design.md",
        "entire-file",
        "network-and-mmds",
    ),
    corpus(
        "mmds-user-guide",
        "docs/mmds/mmds-user-guide.md",
        "entire-file",
        "network-and-mmds",
    ),
    corpus(
        "network-performance",
        "docs/network-performance.md",
        "entire-file",
        "specifications",
    ),
    corpus(
        "network-setup",
        "docs/network-setup.md",
        "entire-file",
        "network-and-mmds",
    ),
    corpus("pmem", "docs/pmem.md", "entire-file", "storage"),
    corpus(
        "production-host",
        "docs/prod-host-setup.md",
        "entire-file",
        "isolation",
    ),
    corpus(
        "rootfs-and-kernel",
        "docs/rootfs-and-kernel-setup.md",
        "entire-file",
        "boot-and-lifecycle",
    ),
    corpus("seccomp", "docs/seccomp.md", "entire-file", "isolation"),
    corpus(
        "seccompiler",
        "docs/seccompiler.md",
        "entire-file",
        "isolation",
    ),
    corpus(
        "snapshot-page-faults",
        "docs/snapshotting/handling-page-faults-on-snapshot-resume.md",
        "entire-file",
        "snapshots",
    ),
    corpus(
        "snapshot-network-clones",
        "docs/snapshotting/network-for-clones.md",
        "entire-file",
        "snapshots",
    ),
    corpus(
        "snapshot-random-clones",
        "docs/snapshotting/random-for-clones.md",
        "entire-file",
        "snapshots",
    ),
    corpus(
        "snapshot-editor",
        "docs/snapshotting/snapshot-editor.md",
        "entire-file",
        "snapshots",
    ),
    corpus(
        "snapshot-support",
        "docs/snapshotting/snapshot-support.md",
        "entire-file",
        "snapshots",
    ),
    corpus(
        "snapshot-versioning",
        "docs/snapshotting/versioning.md",
        "entire-file",
        "snapshots",
    ),
    corpus("tracing", "docs/tracing.md", "entire-file", "observability"),
    corpus("vsock", "docs/vsock.md", "entire-file", "vsock"),
];

const fn tool_operation(
    key: &'static str,
    path: &'static str,
    anchor: &'static str,
    family: &'static str,
) -> StaticItem {
    StaticItem {
        kind: "tool-operation",
        key,
        path,
        anchor,
        family,
    }
}

const fn tool_argument(
    key: &'static str,
    path: &'static str,
    anchor: &'static str,
    family: &'static str,
) -> StaticItem {
    StaticItem {
        kind: "tool-argument",
        key,
        path,
        anchor,
        family,
    }
}

const fn corpus(
    key: &'static str,
    path: &'static str,
    anchor: &'static str,
    family: &'static str,
) -> StaticItem {
    StaticItem {
        kind: "corpus",
        key,
        path,
        anchor,
        family,
    }
}

#[derive(Debug, Deserialize)]
struct Swagger {
    info: SwaggerInfo,
    paths: BTreeMap<String, SwaggerPath>,
    definitions: BTreeMap<String, SwaggerDefinition>,
}

#[derive(Debug, Deserialize)]
struct SwaggerInfo {
    version: String,
}

#[derive(Debug, Default, Deserialize)]
struct SwaggerPath {
    #[serde(default)]
    get: Option<SwaggerOperation>,
    #[serde(default)]
    put: Option<SwaggerOperation>,
    #[serde(default)]
    post: Option<SwaggerOperation>,
    #[serde(default)]
    patch: Option<SwaggerOperation>,
    #[serde(default)]
    delete: Option<SwaggerOperation>,
}

#[derive(Debug, Deserialize)]
struct SwaggerOperation {
    #[serde(rename = "operationId")]
    operation_id: String,
}

#[derive(Debug, Default, Deserialize)]
struct SwaggerDefinition {
    #[serde(default)]
    properties: BTreeMap<String, yaml_serde::Value>,
}

/// Verify and return the canonical root of the pinned Firecracker checkout.
pub fn ensure_pinned_checkout(path: &Path) -> Result<PathBuf, AuditError> {
    ensure_checkout_at(path, FIRECRACKER_COMMIT)
}

/// Derive the complete machine-owned manifest from a pinned Firecracker checkout.
pub fn derive_source_manifest(path: &Path) -> Result<SourceManifest, AuditError> {
    let checkout = ensure_pinned_checkout(path)?;
    let mut items = extract_swagger(&checkout)?;
    items.extend(extract_firecracker_arguments(&checkout)?);
    items.extend(extract_non_swagger_routes(&checkout)?);
    items.extend(static_items(TOOL_OPERATIONS));
    items.extend(static_items(TOOL_ARGUMENTS));
    items.extend(static_items(CORPUS));
    items.sort_by(|left, right| left.id.cmp(&right.id));

    let mut input_extractors = BTreeMap::new();
    input_extractors.insert(SWAGGER_PATH.to_string(), "swagger-v1".to_string());
    input_extractors.insert(
        FIRECRACKER_MAIN_PATH.to_string(),
        "firecracker-cli-v1".to_string(),
    );
    input_extractors.insert(
        PARSED_REQUEST_PATH.to_string(),
        "parsed-delete-routes-v1".to_string(),
    );
    for spec in TOOL_OPERATIONS.iter().chain(TOOL_ARGUMENTS).chain(CORPUS) {
        input_extractors
            .entry(spec.path.to_string())
            .or_insert_with(|| "curated-source-v1".to_string());
    }

    let mut inputs = Vec::with_capacity(input_extractors.len());
    for (input_path, extractor) in input_extractors {
        ensure_regular_input(&checkout, &input_path)?;
        inputs.push(Input {
            git_blob: git_output(&checkout, &["rev-parse", &format!("HEAD:{input_path}")])?,
            path: input_path,
            extractor,
        });
    }

    let counts = counts(&items);
    Ok(SourceManifest {
        schema_version: SCHEMA_VERSION,
        baseline: Baseline {
            version: FIRECRACKER_VERSION.to_string(),
            commit: FIRECRACKER_COMMIT.to_string(),
            target: FIRECRACKER_TARGET.to_string(),
        },
        generator_version: GENERATOR_VERSION,
        inputs,
        counts,
        items,
    })
}

fn ensure_checkout_at(path: &Path, expected_commit: &str) -> Result<PathBuf, AuditError> {
    let canonical = path
        .canonicalize()
        .map_err(|_| AuditError::new("Firecracker checkout is not accessible"))?;
    if !canonical.is_dir() {
        return Err(AuditError::new("Firecracker checkout must be a directory"));
    }
    let root_text = git_output(&canonical, &["rev-parse", "--show-toplevel"])?;
    let root = PathBuf::from(root_text)
        .canonicalize()
        .map_err(|_| AuditError::new("Git worktree root is not accessible"))?;
    if root != canonical {
        return Err(AuditError::new(
            "Firecracker checkout path must name the Git worktree root",
        ));
    }
    let head = git_output(&canonical, &["rev-parse", "HEAD"])?;
    if head != expected_commit {
        return Err(AuditError::new(format!(
            "Firecracker checkout HEAD must be {expected_commit}, found {head}"
        )));
    }
    let status = git_output(&canonical, &["status", "--porcelain=v1"])?;
    if !status.is_empty() {
        return Err(AuditError::new("Firecracker checkout must be clean"));
    }
    Ok(canonical)
}

fn git_output(checkout: &Path, args: &[&str]) -> Result<String, AuditError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(args)
        .output()
        .map_err(|error| AuditError::new(format!("failed to execute Git: {error}")))?;
    if !output.status.success() {
        return Err(AuditError::new(
            "Git rejected the supplied Firecracker checkout",
        ));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_string())
        .map_err(|_| AuditError::new("Git returned non-UTF-8 output"))
}

fn ensure_regular_input(checkout: &Path, relative: &str) -> Result<PathBuf, AuditError> {
    let joined = checkout.join(relative);
    let metadata = std::fs::symlink_metadata(&joined)
        .map_err(|_| AuditError::new(format!("pinned input is missing: {relative}")))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(AuditError::new(format!(
            "pinned input must be a regular file: {relative}"
        )));
    }
    let canonical = joined
        .canonicalize()
        .map_err(|_| AuditError::new(format!("pinned input is not accessible: {relative}")))?;
    if !canonical.starts_with(checkout) {
        return Err(AuditError::new(format!(
            "pinned input escapes checkout: {relative}"
        )));
    }
    Ok(canonical)
}

fn read_input(checkout: &Path, relative: &str) -> Result<String, AuditError> {
    let path = ensure_regular_input(checkout, relative)?;
    std::fs::read_to_string(path)
        .map_err(|error| AuditError::new(format!("failed to read {relative}: {error}")))
}

fn extract_swagger(checkout: &Path) -> Result<Vec<SourceItem>, AuditError> {
    let source = read_input(checkout, SWAGGER_PATH)?;
    let swagger: Swagger = yaml_serde::from_str(&source)
        .map_err(|error| AuditError::new(format!("failed to parse pinned Swagger: {error}")))?;
    if swagger.info.version != FIRECRACKER_VERSION {
        return Err(AuditError::new(format!(
            "Swagger version must be {FIRECRACKER_VERSION}, found {}",
            swagger.info.version
        )));
    }

    let mut items = Vec::new();
    for (path, methods) in swagger.paths {
        items.push(source_item(
            "api-path",
            &path,
            SWAGGER_PATH,
            &format!("#/paths/{path}"),
            "api-contract",
        ));
        let operations = [
            ("GET", methods.get),
            ("PUT", methods.put),
            ("POST", methods.post),
            ("PATCH", methods.patch),
            ("DELETE", methods.delete),
        ];
        for (method, operation) in operations {
            if let Some(operation) = operation {
                let key = format!("{method} {path}");
                items.push(source_item(
                    "api-operation",
                    &key,
                    SWAGGER_PATH,
                    &format!(
                        "#/paths/{path}/{method};operationId={}",
                        operation.operation_id
                    ),
                    "api-contract",
                ));
            }
        }
    }
    for (name, definition) in swagger.definitions {
        items.push(source_item(
            "api-schema",
            &name,
            SWAGGER_PATH,
            &format!("#/definitions/{name}"),
            "api-contract",
        ));
        for property in definition.properties.keys() {
            let key = format!("{name}.{property}");
            items.push(source_item(
                "api-property",
                &key,
                SWAGGER_PATH,
                &format!("#/definitions/{name}/properties/{property}"),
                "api-contract",
            ));
        }
    }
    Ok(items)
}

fn extract_firecracker_arguments(checkout: &Path) -> Result<Vec<SourceItem>, AuditError> {
    let source = read_input(checkout, FIRECRACKER_MAIN_PATH)?;
    let metadata = extract_string_constant(&source, "MMDS_CONTENT_ARG")?;
    let constants = BTreeMap::from([("MMDS_CONTENT_ARG", metadata.as_str())]);
    let names = extract_argument_names(&source, &constants)?;
    if names.len() != 23 {
        return Err(AuditError::new(format!(
            "expected 23 configured Firecracker arguments, found {}",
            names.len()
        )));
    }
    Ok(names
        .into_iter()
        .map(|name| {
            source_item(
                "firecracker-argument",
                &name,
                FIRECRACKER_MAIN_PATH,
                &format!("Argument::new({name})"),
                "process",
            )
        })
        .collect())
}

fn extract_string_constant(source: &str, name: &str) -> Result<String, AuditError> {
    let marker = format!("const {name}: &str = \"");
    let start = source
        .find(&marker)
        .ok_or_else(|| AuditError::new(format!("missing string constant {name}")))?
        + marker.len();
    let rest = source
        .get(start..)
        .ok_or_else(|| AuditError::new(format!("invalid string constant {name}")))?;
    let end = rest
        .find('"')
        .ok_or_else(|| AuditError::new(format!("unterminated string constant {name}")))?;
    rest.get(..end)
        .map(ToString::to_string)
        .ok_or_else(|| AuditError::new(format!("invalid string constant {name}")))
}

fn extract_argument_names(
    source: &str,
    constants: &BTreeMap<&str, &str>,
) -> Result<Vec<String>, AuditError> {
    let marker = "Argument::new(";
    let mut remainder = source;
    let mut names = BTreeSet::new();
    while let Some(offset) = remainder.find(marker) {
        let after_marker = remainder
            .get(offset + marker.len()..)
            .ok_or_else(|| AuditError::new("invalid Argument::new expression"))?;
        let close = after_marker
            .find(')')
            .ok_or_else(|| AuditError::new("unterminated Argument::new expression"))?;
        let token = after_marker
            .get(..close)
            .ok_or_else(|| AuditError::new("invalid Argument::new token"))?
            .trim();
        let name = if let Some(quoted) = token
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
        {
            quoted
        } else {
            constants.get(token).copied().ok_or_else(|| {
                AuditError::new(format!("unresolved Argument::new token: {token}"))
            })?
        };
        if !names.insert(name.to_string()) {
            return Err(AuditError::new(format!(
                "duplicate configured Firecracker argument: {name}"
            )));
        }
        remainder = after_marker
            .get(close + 1..)
            .ok_or_else(|| AuditError::new("invalid Argument::new remainder"))?;
    }
    Ok(names.into_iter().collect())
}

fn extract_non_swagger_routes(checkout: &Path) -> Result<Vec<SourceItem>, AuditError> {
    let source = read_input(checkout, PARSED_REQUEST_PATH)?;
    let routes = [
        (
            "DELETE /drives/{drive_id}",
            "(Method::Delete, \"drives\", None)",
            "storage",
        ),
        (
            "DELETE /network-interfaces/{iface_id}",
            "(Method::Delete, \"network-interfaces\", None)",
            "network-and-mmds",
        ),
        (
            "DELETE /pmem/{id}",
            "(Method::Delete, \"pmem\", None)",
            "storage",
        ),
    ];
    for (_, pattern, _) in routes {
        if !source.contains(pattern) {
            return Err(AuditError::new(format!(
                "missing pinned non-Swagger route pattern: {pattern}"
            )));
        }
    }
    Ok(routes
        .into_iter()
        .map(|(key, anchor, family)| {
            source_item(
                "non-swagger-route",
                key,
                PARSED_REQUEST_PATH,
                anchor,
                family,
            )
        })
        .collect())
}

fn static_items(specs: &[StaticItem]) -> Vec<SourceItem> {
    specs
        .iter()
        .map(|spec| source_item(spec.kind, spec.key, spec.path, spec.anchor, spec.family))
        .collect()
}

fn source_item(kind: &str, key: &str, path: &str, anchor: &str, family: &str) -> SourceItem {
    SourceItem {
        id: format!("{kind}:{key}"),
        kind: kind.to_string(),
        key: key.to_string(),
        path: path.to_string(),
        anchor: anchor.to_string(),
        family: family.to_string(),
    }
}

fn counts(items: &[SourceItem]) -> Counts {
    Counts {
        swagger_paths: kind_count(items, "api-path"),
        swagger_operations: kind_count(items, "api-operation"),
        swagger_definitions: kind_count(items, "api-schema"),
        swagger_properties: kind_count(items, "api-property"),
        firecracker_arguments: kind_count(items, "firecracker-argument"),
        non_swagger_routes: kind_count(items, "non-swagger-route"),
        public_tool_operations: kind_count(items, "tool-operation"),
        public_tool_arguments: kind_count(items, "tool-argument"),
        corpus_items: kind_count(items, "corpus"),
    }
}

fn kind_count(items: &[SourceItem], kind: &str) -> usize {
    items.iter().filter(|item| item.kind == kind).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    #[derive(Debug)]
    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "bangbang-capability-audit-{}-{serial}",
                std::process::id()
            ));
            std::fs::create_dir(&path).expect("temporary directory should be created");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn git_for_test(path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .expect("Git should execute");
        assert!(
            output.status.success(),
            "Git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("Git output should be UTF-8")
            .trim()
            .to_string()
    }

    fn initialized_repository() -> (TempDirectory, String) {
        let directory = TempDirectory::new();
        git_for_test(directory.path(), &["init", "--quiet"]);
        std::fs::write(directory.path().join("tracked"), "initial\n")
            .expect("tracked fixture should be written");
        git_for_test(directory.path(), &["add", "tracked"]);
        git_for_test(
            directory.path(),
            &[
                "-c",
                "user.name=Capability Audit",
                "-c",
                "user.email=audit@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ],
        );
        let head = git_for_test(directory.path(), &["rev-parse", "HEAD"]);
        (directory, head)
    }

    #[test]
    fn extracts_literal_and_constant_arguments() {
        let source = r#"
            const NAME: &str = "metadata";
            ArgParser::new()
                .arg(Argument::new("api-sock"))
                .arg(Argument::new(NAME));
        "#;
        let constants = BTreeMap::from([("NAME", "metadata")]);
        assert_eq!(
            extract_argument_names(source, &constants).expect("arguments should parse"),
            vec!["api-sock".to_string(), "metadata".to_string()]
        );
    }

    #[test]
    fn rejects_unresolved_argument_tokens() {
        let error = extract_argument_names("Argument::new(UNKNOWN)", &BTreeMap::new())
            .expect_err("unknown constant should fail");
        assert!(error.to_string().contains("unresolved Argument::new token"));
    }

    #[test]
    fn parses_minimal_swagger_shape() {
        let source = r#"
info:
  version: 1.16.0
paths:
  /:
    get:
      operationId: describeInstance
definitions:
  InstanceInfo:
    properties:
      id:
        type: string
"#;
        let swagger: Swagger = yaml_serde::from_str(source).expect("Swagger should parse");
        assert_eq!(swagger.paths.len(), 1);
        assert_eq!(swagger.definitions.len(), 1);
        assert_eq!(
            swagger
                .definitions
                .get("InstanceInfo")
                .expect("definition should exist")
                .properties
                .len(),
            1
        );
    }

    #[test]
    fn static_tool_inventory_has_expected_cardinality() {
        assert_eq!(TOOL_OPERATIONS.len(), 14);
        assert_eq!(TOOL_ARGUMENTS.len(), 41);
        assert!(CORPUS.len() >= 40);
    }

    #[test]
    fn extracts_expected_bodyless_delete_routes() {
        let directory = TempDirectory::new();
        let source_path = directory.path().join(PARSED_REQUEST_PATH);
        std::fs::create_dir_all(
            source_path
                .parent()
                .expect("parsed request fixture should have a parent"),
        )
        .expect("parsed request fixture directory should be created");
        std::fs::write(
            source_path,
            r#"
                (Method::Delete, "drives", None)
                (Method::Delete, "network-interfaces", None)
                (Method::Delete, "pmem", None)
            "#,
        )
        .expect("parsed request fixture should be written");

        let checkout = directory
            .path()
            .canonicalize()
            .expect("fixture checkout should canonicalize");
        let items = extract_non_swagger_routes(&checkout)
            .expect("the three bodyless DELETE routes should be extracted");
        let ids: Vec<&str> = items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "non-swagger-route:DELETE /drives/{drive_id}",
                "non-swagger-route:DELETE /network-interfaces/{iface_id}",
                "non-swagger-route:DELETE /pmem/{id}",
            ]
        );
    }

    #[test]
    fn accepts_clean_checkout_at_expected_head() {
        let (directory, head) = initialized_repository();
        assert_eq!(
            ensure_checkout_at(directory.path(), &head).expect("clean checkout should validate"),
            directory
                .path()
                .canonicalize()
                .expect("fixture path should canonicalize")
        );
    }

    #[test]
    fn rejects_wrong_checkout_head() {
        let (directory, _) = initialized_repository();
        let error =
            ensure_checkout_at(directory.path(), "0000000000000000000000000000000000000000")
                .expect_err("wrong head should fail");
        assert!(error.to_string().contains("HEAD must be"));
    }

    #[test]
    fn rejects_dirty_checkout() {
        let (directory, head) = initialized_repository();
        std::fs::write(directory.path().join("tracked"), "changed\n")
            .expect("fixture should be changed");
        let error =
            ensure_checkout_at(directory.path(), &head).expect_err("dirty checkout should fail");
        assert!(error.to_string().contains("must be clean"));
    }

    #[test]
    fn rejects_non_git_directory() {
        let directory = TempDirectory::new();
        let error = ensure_checkout_at(directory.path(), FIRECRACKER_COMMIT)
            .expect_err("non-Git directory should fail");
        assert!(error.to_string().contains("Git rejected"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_pinned_input() {
        use std::os::unix::fs::symlink;

        let directory = TempDirectory::new();
        std::fs::write(directory.path().join("target"), "data").expect("target should be written");
        symlink("target", directory.path().join("link")).expect("symlink should be created");
        let error = ensure_regular_input(directory.path(), "link")
            .expect_err("symlinked input should fail");
        assert!(error.to_string().contains("must be a regular file"));
    }

    #[test]
    fn rejects_malformed_swagger() {
        let error =
            yaml_serde::from_str::<Swagger>("paths: [").expect_err("malformed Swagger should fail");
        assert!(!error.to_string().is_empty());
    }
}
