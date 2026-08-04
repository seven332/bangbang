use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode};

use bangbang_firecracker_capability_audit::{
    AuditError, AuditMode, CAPABILITY_INVENTORY_PATH, LOGGER_PRODUCER_AUDIT_PATH,
    LOGGER_PRODUCER_MANIFEST_PATH, SOURCE_MANIFEST_PATH, derive_logger_producer_manifest,
    derive_source_manifest, logger_producer_manifest_json, read_capability_inventory,
    read_logger_producer_audit, read_logger_producer_manifest, read_source_manifest,
    source_manifest_json, validate, validate_logger_compatibility, validate_logger_producers,
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
}

fn parse_validate_mode(args: &[String]) -> Result<ValidateMode, AuditError> {
    match args {
        [] => Ok(ValidateMode::Delivery),
        [flag] if flag == "--final" => Ok(ValidateMode::Final),
        [flag] if flag == "--logger-final" => Ok(ValidateMode::LoggerFinal),
        _ => Err(AuditError::new(
            "validate accepts only one optional --final or --logger-final flag",
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
            return Ok(
                "Firecracker capability inventory and logger producer audit are valid for the terminal logger compatibility scope"
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
    if !failures.is_empty() {
        return Err(AuditError::new(failures.join("\n")));
    }
    let mode_name = match audit_mode {
        AuditMode::Delivery => "delivery",
        AuditMode::Final => "final",
    };
    Ok(format!(
        "Firecracker capability inventory and logger producer audit are valid in {mode_name} mode"
    ))
}

fn run_compare(args: &[String]) -> Result<String, AuditError> {
    let firecracker = required_option(args, "--firecracker")?;
    let root = repository_root()?;
    let checked_in = read_source_manifest(&root.join(SOURCE_MANIFEST_PATH))?;
    let derived = derive_source_manifest(Path::new(&firecracker))?;
    let checked_logger = read_logger_producer_manifest(&root.join(LOGGER_PRODUCER_MANIFEST_PATH))?;
    let derived_logger = derive_logger_producer_manifest(Path::new(&firecracker))?;
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
    if differences.is_empty() {
        Ok(
            "checked-in source and logger producer manifests match the pinned Firecracker checkout"
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

fn candidate_output_path(root: &Path, output: &Path) -> Result<PathBuf, AuditError> {
    let output_path = absolute_from(root, output);
    let source_path = root.join(SOURCE_MANIFEST_PATH);
    let inventory_path = root.join(CAPABILITY_INVENTORY_PATH);
    let logger_manifest_path = root.join(LOGGER_PRODUCER_MANIFEST_PATH);
    let logger_audit_path = root.join(LOGGER_PRODUCER_AUDIT_PATH);
    let normalized_output = normalize_lexically(&output_path);
    let checked_paths = [
        &source_path,
        &inventory_path,
        &logger_manifest_path,
        &logger_audit_path,
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
    "Usage:\n  bangbang-firecracker-capability-audit validate [--final | --logger-final]\n  bangbang-firecracker-capability-audit compare --firecracker PATH\n  bangbang-firecracker-capability-audit regenerate --firecracker PATH --output PATH\n  bangbang-firecracker-capability-audit regenerate-logger-producers --firecracker PATH --output PATH"
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

        for invalid in [
            vec!["--unknown".to_string()],
            vec!["--final".to_string(), "--logger-final".to_string()],
        ] {
            let error = parse_validate_mode(&invalid).expect_err("mode should be rejected");
            assert!(error.to_string().contains("accepts only one optional"));
        }
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
