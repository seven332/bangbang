use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode};

use bangbang_firecracker_capability_audit::{
    AuditError, AuditMode, CAPABILITY_INVENTORY_PATH, SOURCE_MANIFEST_PATH, derive_source_manifest,
    read_capability_inventory, read_source_manifest, source_manifest_json, validate,
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
        "help" | "--help" | "-h" => Ok(usage().to_string()),
        _ => Err(AuditError::new(format!(
            "unknown command: {command}\n{}",
            usage()
        ))),
    }
}

fn run_validate(args: &[String]) -> Result<String, AuditError> {
    let mode = match args {
        [] => AuditMode::Delivery,
        [flag] if flag == "--final" => AuditMode::Final,
        _ => {
            return Err(AuditError::new(
                "validate accepts only the optional --final flag",
            ));
        }
    };
    let root = repository_root()?;
    let manifest = read_source_manifest(&root.join(SOURCE_MANIFEST_PATH))?;
    let inventory = read_capability_inventory(&root.join(CAPABILITY_INVENTORY_PATH))?;
    validate(&manifest, &inventory, &root, mode)
        .map_err(|errors| AuditError::new(format!("inventory validation errors:\n{errors}")))?;
    let mode_name = match mode {
        AuditMode::Delivery => "delivery",
        AuditMode::Final => "final",
    };
    Ok(format!(
        "Firecracker capability inventory is valid in {mode_name} mode"
    ))
}

fn run_compare(args: &[String]) -> Result<String, AuditError> {
    let firecracker = required_option(args, "--firecracker")?;
    let root = repository_root()?;
    let checked_in = read_source_manifest(&root.join(SOURCE_MANIFEST_PATH))?;
    let derived = derive_source_manifest(Path::new(&firecracker))?;
    if checked_in != derived {
        let checked_json = String::from_utf8(source_manifest_json(&checked_in)?)
            .map_err(|_| AuditError::new("checked source manifest JSON is not valid UTF-8"))?;
        let derived_json = String::from_utf8(source_manifest_json(&derived)?)
            .map_err(|_| AuditError::new("derived source manifest JSON is not valid UTF-8"))?;
        return Err(AuditError::new(format!(
            "derived source manifest differs from {SOURCE_MANIFEST_PATH}; run regenerate to an explicit candidate path\n{}",
            canonical_line_diff(&checked_json, &derived_json)
        )));
    }
    Ok("checked-in source manifest matches the pinned Firecracker checkout".to_string())
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

fn candidate_output_path(root: &Path, output: &Path) -> Result<PathBuf, AuditError> {
    let output_path = absolute_from(root, output);
    let source_path = root.join(SOURCE_MANIFEST_PATH);
    let inventory_path = root.join(CAPABILITY_INVENTORY_PATH);
    let normalized_output = normalize_lexically(&output_path);
    if normalized_output == normalize_lexically(&source_path)
        || normalized_output == normalize_lexically(&inventory_path)
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
    for checked_path in [&source_path, &inventory_path] {
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
    "Usage:\n  bangbang-firecracker-capability-audit validate [--final]\n  bangbang-firecracker-capability-audit compare --firecracker PATH\n  bangbang-firecracker-capability-audit regenerate --firecracker PATH --output PATH"
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
    fn regenerate_refuses_both_checked_inventory_files() {
        let root = Path::new("/repository");
        for path in [SOURCE_MANIFEST_PATH, CAPABILITY_INVENTORY_PATH] {
            let error = candidate_output_path(root, Path::new(path))
                .expect_err("checked inventory path should be refused");
            assert!(error.to_string().contains("never overwrites"));
        }
    }

    #[test]
    fn regenerate_refuses_lexical_aliases_of_checked_inventory_files() {
        let root = Path::new("/repository");
        for path in [
            "compat/firecracker/../firecracker/v1.16.0/source-manifest.json",
            "compat/firecracker/v1.16.0/./capabilities.json",
        ] {
            let error = candidate_output_path(root, Path::new(path))
                .expect_err("checked inventory alias should be refused");
            assert!(error.to_string().contains("never overwrites"));
        }
    }
}
