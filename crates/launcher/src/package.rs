use std::ffi::{OsStr, OsString};
use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;
use std::process::Output;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::layout::{APP_SANDBOX_ENTITLEMENT, HYPERVISOR_ENTITLEMENT};
use crate::{
    LAUNCHER_BUNDLE_IDENTIFIER, LAUNCHER_EXECUTABLE_NAME, OUTER_BUNDLE_NAME, PackageError,
    PackageOptions, WORKER_BUNDLE_IDENTIFIER, WORKER_BUNDLE_NAME, WORKER_EXECUTABLE_NAME,
};

const LAUNCHER_INFO_PLIST: &[u8] = include_bytes!("../../../packaging/macos/Bangbang-Info.plist");
const WORKER_INFO_PLIST: &[u8] =
    include_bytes!("../../../packaging/macos/BangbangWorker-Info.plist");
const WORKER_ENTITLEMENTS: &[u8] =
    include_bytes!("../../../packaging/macos/BangbangWorker.entitlements.plist");
const CODESIGN: &str = "/usr/bin/codesign";
const PLUTIL: &str = "/usr/bin/plutil";
const MAX_TEST_RESOURCE_ENTRIES: usize = 128;
const MAX_TEST_RESOURCE_DEPTH: usize = 8;
const MAX_TEST_RESOURCE_BYTES: u64 = 1024 * 1024 * 1024;
static NEXT_STAGE_ID: AtomicU64 = AtomicU64::new(0);

/// Builds, inspects, and exclusively publishes one production app bundle.
pub fn build_bundle(options: &PackageOptions) -> Result<PathBuf, PackageError> {
    #[cfg(target_os = "macos")]
    {
        build_bundle_with(options, &SystemTools, &SystemPublisher)
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = options;
        Err(PackageError::UnsupportedPlatform)
    }
}

trait ToolRunner {
    fn run(&self, program: &Path, args: &[OsString]) -> Result<Output, io::ErrorKind>;
}

#[derive(Debug)]
#[cfg(target_os = "macos")]
struct SystemTools;

#[cfg(target_os = "macos")]
impl ToolRunner for SystemTools {
    fn run(&self, program: &Path, args: &[OsString]) -> Result<Output, io::ErrorKind> {
        Command::new(program)
            .args(args)
            .output()
            .map_err(|err| err.kind())
    }
}

trait Publisher {
    fn publish(&self, source: &Path, destination: &Path) -> Result<(), io::ErrorKind>;
}

#[derive(Debug)]
#[cfg(target_os = "macos")]
struct SystemPublisher;

#[cfg(target_os = "macos")]
impl Publisher for SystemPublisher {
    fn publish(&self, source: &Path, destination: &Path) -> Result<(), io::ErrorKind> {
        crate::macos::publish::publish_exclusive(source, destination)
    }
}

fn build_bundle_with(
    options: &PackageOptions,
    tools: &dyn ToolRunner,
    publisher: &dyn Publisher,
) -> Result<PathBuf, PackageError> {
    let inputs = ValidatedInputs::new(options)?;
    let staging = StagingDirectory::create(&inputs.output_parent)?;
    let staged_bundle = staging.path().join(OUTER_BUNDLE_NAME);
    let staged_worker_bundle = staged_bundle
        .join("Contents/Helpers")
        .join(WORKER_BUNDLE_NAME);
    let staged_launcher = staged_bundle
        .join("Contents/MacOS")
        .join(LAUNCHER_EXECUTABLE_NAME);
    let staged_worker = staged_worker_bundle
        .join("Contents/MacOS")
        .join(WORKER_EXECUTABLE_NAME);
    let staged_outer_info = staged_bundle.join("Contents/Info.plist");
    let staged_worker_info = staged_worker_bundle.join("Contents/Info.plist");
    let entitlement_file = staging.path().join("BangbangWorker.entitlements.plist");

    create_dir(&staged_bundle.join("Contents/MacOS"))?;
    create_dir(&staged_worker_bundle.join("Contents/MacOS"))?;
    copy_executable(&inputs.launcher_binary, &staged_launcher)?;
    copy_executable(&inputs.worker_binary, &staged_worker)?;
    write_file(&staged_outer_info, LAUNCHER_INFO_PLIST)?;
    write_file(&staged_worker_info, WORKER_INFO_PLIST)?;
    write_file(&entitlement_file, WORKER_ENTITLEMENTS)?;

    if let Some(resources) = &inputs.test_worker_resources {
        let destination = staged_worker_bundle.join("Contents/Resources");
        create_dir(&destination)?;
        copy_test_resources(resources, &destination)?;
    }

    sign_worker(
        tools,
        &inputs.signing_identity,
        &entitlement_file,
        &staged_worker_bundle,
    )?;
    sign_outer(tools, &inputs.signing_identity, &staged_bundle)?;
    inspect_bundle(
        tools,
        &staged_bundle,
        &staged_worker_bundle,
        &staged_outer_info,
        &staged_worker_info,
    )?;

    match publisher.publish(&staged_bundle, &inputs.output_bundle) {
        Ok(()) => Ok(inputs.output_bundle),
        Err(io::ErrorKind::AlreadyExists) => Err(PackageError::OutputAlreadyExists),
        Err(kind) => Err(PackageError::Publication(kind)),
    }
}

#[derive(Debug)]
struct ValidatedInputs {
    launcher_binary: PathBuf,
    worker_binary: PathBuf,
    output_parent: PathBuf,
    output_bundle: PathBuf,
    signing_identity: OsString,
    test_worker_resources: Option<PathBuf>,
}

impl ValidatedInputs {
    fn new(options: &PackageOptions) -> Result<Self, PackageError> {
        require_plain_file(&options.launcher_binary)?;
        require_plain_file(&options.worker_binary)?;
        if options.signing_identity.is_empty() {
            return Err(PackageError::InvalidInput);
        }
        if options.output_bundle.file_name() != Some(OsStr::new(OUTER_BUNDLE_NAME)) {
            return Err(PackageError::InvalidInput);
        }
        match fs::symlink_metadata(&options.output_bundle) {
            Ok(_) => return Err(PackageError::OutputAlreadyExists),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(PackageError::Staging(err.kind())),
        }
        let output_parent = options
            .output_bundle
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let output_parent =
            fs::canonicalize(output_parent).map_err(|err| PackageError::Staging(err.kind()))?;
        require_plain_dir(&output_parent)?;
        let output_bundle = output_parent.join(OUTER_BUNDLE_NAME);
        match fs::symlink_metadata(&output_bundle) {
            Ok(_) => return Err(PackageError::OutputAlreadyExists),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(PackageError::Staging(err.kind())),
        }

        let test_worker_resources = match &options.test_worker_resources {
            Some(path) => {
                require_plain_resource_dir(path)?;
                Some(path.to_path_buf())
            }
            None => None,
        };

        Ok(Self {
            launcher_binary: options.launcher_binary.clone(),
            worker_binary: options.worker_binary.clone(),
            output_parent,
            output_bundle,
            signing_identity: options.signing_identity.clone(),
            test_worker_resources,
        })
    }
}

#[derive(Debug)]
struct StagingDirectory {
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl StagingDirectory {
    fn create(parent: &Path) -> Result<Self, PackageError> {
        for _ in 0..32 {
            let id = NEXT_STAGE_ID.fetch_add(1, Ordering::SeqCst);
            let path = parent.join(format!(
                ".bangbang-bundle-stage-{}-{id}",
                std::process::id()
            ));
            match DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => {
                    let metadata = match fs::symlink_metadata(&path) {
                        Ok(metadata) => metadata,
                        Err(err) => {
                            let _ = fs::remove_dir(&path);
                            return Err(PackageError::Staging(err.kind()));
                        }
                    };
                    if metadata.file_type().is_symlink() || !metadata.is_dir() {
                        return Err(PackageError::Staging(io::ErrorKind::InvalidData));
                    }
                    return Ok(Self {
                        path,
                        device: metadata.dev(),
                        inode: metadata.ino(),
                    });
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                Err(err) => return Err(PackageError::Staging(err.kind())),
            }
        }
        Err(PackageError::Staging(io::ErrorKind::AlreadyExists))
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        let Ok(metadata) = fs::symlink_metadata(&self.path) else {
            return;
        };
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.dev() != self.device
            || metadata.ino() != self.inode
        {
            return;
        }
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn require_plain_file(path: &Path) -> Result<(), PackageError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| PackageError::InvalidInputEntry)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(PackageError::InvalidInputEntry);
    }
    Ok(())
}

fn require_plain_dir(path: &Path) -> Result<(), PackageError> {
    let metadata = fs::symlink_metadata(path).map_err(|err| PackageError::Staging(err.kind()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(PackageError::InvalidInputEntry);
    }
    Ok(())
}

fn require_plain_resource_dir(path: &Path) -> Result<(), PackageError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| PackageError::InvalidTestResources)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(PackageError::InvalidTestResources);
    }
    Ok(())
}

fn create_dir(path: &Path) -> Result<(), PackageError> {
    fs::create_dir_all(path).map_err(|err| PackageError::Staging(err.kind()))
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), PackageError> {
    fs::write(path, bytes).map_err(|err| PackageError::Staging(err.kind()))
}

fn copy_executable(source: &Path, destination: &Path) -> Result<(), PackageError> {
    fs::copy(source, destination).map_err(|err| PackageError::Staging(err.kind()))?;
    let mut permissions = fs::metadata(destination)
        .map_err(|err| PackageError::Staging(err.kind()))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(destination, permissions).map_err(|err| PackageError::Staging(err.kind()))
}

fn copy_test_resources(source: &Path, destination: &Path) -> Result<(), PackageError> {
    let mut budget = ResourceBudget::default();
    copy_resource_directory(source, destination, 0, &mut budget)
}

#[derive(Debug, Default)]
struct ResourceBudget {
    entries: usize,
    bytes: u64,
}

fn copy_resource_directory(
    source: &Path,
    destination: &Path,
    depth: usize,
    budget: &mut ResourceBudget,
) -> Result<(), PackageError> {
    if depth > MAX_TEST_RESOURCE_DEPTH {
        return Err(PackageError::InvalidTestResources);
    }
    let remaining_entries = MAX_TEST_RESOURCE_ENTRIES
        .checked_sub(budget.entries)
        .ok_or(PackageError::InvalidTestResources)?;
    let entries = fs::read_dir(source).map_err(|_| PackageError::InvalidTestResources)?;
    let mut entries = entries
        .take(remaining_entries + 1)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| PackageError::InvalidTestResources)?;
    if entries.len() > remaining_entries {
        return Err(PackageError::InvalidTestResources);
    }
    entries.sort_by_key(fs::DirEntry::file_name);

    for entry in entries {
        budget.entries = budget
            .entries
            .checked_add(1)
            .ok_or(PackageError::InvalidTestResources)?;
        if budget.entries > MAX_TEST_RESOURCE_ENTRIES {
            return Err(PackageError::InvalidTestResources);
        }
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata =
            fs::symlink_metadata(&source_path).map_err(|_| PackageError::InvalidTestResources)?;
        if metadata.file_type().is_symlink() {
            return Err(PackageError::InvalidTestResources);
        }
        if metadata.is_dir() {
            fs::create_dir(&destination_path).map_err(|err| PackageError::Staging(err.kind()))?;
            copy_resource_directory(&source_path, &destination_path, depth + 1, budget)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(PackageError::InvalidTestResources);
        }
        budget.bytes = budget
            .bytes
            .checked_add(metadata.len())
            .ok_or(PackageError::InvalidTestResources)?;
        if budget.bytes > MAX_TEST_RESOURCE_BYTES {
            return Err(PackageError::InvalidTestResources);
        }
        fs::copy(&source_path, &destination_path)
            .map_err(|err| PackageError::Staging(err.kind()))?;
    }
    Ok(())
}

fn sign_worker(
    tools: &dyn ToolRunner,
    identity: &OsStr,
    entitlements: &Path,
    worker_bundle: &Path,
) -> Result<(), PackageError> {
    let mut args = signing_args(identity);
    args.extend([
        OsString::from("--entitlements"),
        entitlements.as_os_str().to_os_string(),
        worker_bundle.as_os_str().to_os_string(),
    ]);
    run_success(tools, Path::new(CODESIGN), &args, "worker signing")?;
    Ok(())
}

fn sign_outer(
    tools: &dyn ToolRunner,
    identity: &OsStr,
    outer_bundle: &Path,
) -> Result<(), PackageError> {
    let mut args = signing_args(identity);
    args.push(outer_bundle.as_os_str().to_os_string());
    run_success(tools, Path::new(CODESIGN), &args, "launcher signing")?;
    Ok(())
}

fn signing_args(identity: &OsStr) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("--force"),
        OsString::from("--sign"),
        identity.to_os_string(),
        OsString::from("--options"),
        OsString::from("runtime"),
    ];
    if identity != OsStr::new("-") {
        args.push(OsString::from("--timestamp"));
    }
    args
}

fn inspect_bundle(
    tools: &dyn ToolRunner,
    outer_bundle: &Path,
    worker_bundle: &Path,
    outer_info: &Path,
    worker_info: &Path,
) -> Result<(), PackageError> {
    inspect_plist_value(
        tools,
        outer_info,
        "CFBundleIdentifier",
        LAUNCHER_BUNDLE_IDENTIFIER,
    )?;
    inspect_plist_value(
        tools,
        outer_info,
        "CFBundleExecutable",
        LAUNCHER_EXECUTABLE_NAME,
    )?;
    inspect_plist_value(
        tools,
        worker_info,
        "CFBundleIdentifier",
        WORKER_BUNDLE_IDENTIFIER,
    )?;
    inspect_plist_value(
        tools,
        worker_info,
        "CFBundleExecutable",
        WORKER_EXECUTABLE_NAME,
    )?;

    let worker_requirement = format!(
        "identifier \"{WORKER_BUNDLE_IDENTIFIER}\" and entitlement[\"{APP_SANDBOX_ENTITLEMENT}\"] exists and entitlement[\"{HYPERVISOR_ENTITLEMENT}\"] exists"
    );
    verify_requirement(tools, worker_bundle, &worker_requirement)?;
    verify_requirement(
        tools,
        outer_bundle,
        &format!(
            "identifier \"{LAUNCHER_BUNDLE_IDENTIFIER}\" and entitlement[\"{APP_SANDBOX_ENTITLEMENT}\"] absent and entitlement[\"{HYPERVISOR_ENTITLEMENT}\"] absent"
        ),
    )?;
    run_success(
        tools,
        Path::new(CODESIGN),
        &[
            OsString::from("--verify"),
            OsString::from("--deep"),
            OsString::from("--strict"),
            OsString::from("--verbose=4"),
            outer_bundle.as_os_str().to_os_string(),
        ],
        "recursive verification",
    )?;
    inspect_runtime_flag(tools, outer_bundle)?;
    inspect_runtime_flag(tools, worker_bundle)?;
    inspect_worker_entitlements(tools, worker_bundle)?;
    inspect_outer_entitlements(tools, outer_bundle)
}

fn inspect_plist_value(
    tools: &dyn ToolRunner,
    plist: &Path,
    key: &str,
    expected: &str,
) -> Result<(), PackageError> {
    let output = run_success(
        tools,
        Path::new(PLUTIL),
        &[
            OsString::from("-extract"),
            OsString::from(key),
            OsString::from("raw"),
            OsString::from("-o"),
            OsString::from("-"),
            plist.as_os_str().to_os_string(),
        ],
        "metadata inspection",
    )?;
    if String::from_utf8_lossy(&output.stdout).trim() == expected {
        Ok(())
    } else {
        Err(PackageError::InspectionFailure)
    }
}

fn verify_requirement(
    tools: &dyn ToolRunner,
    bundle: &Path,
    requirement: &str,
) -> Result<(), PackageError> {
    run_success(
        tools,
        Path::new(CODESIGN),
        &[
            OsString::from("--verify"),
            OsString::from("--strict"),
            OsString::from(format!("-R={requirement}")),
            bundle.as_os_str().to_os_string(),
        ],
        "requirement inspection",
    )?;
    Ok(())
}

fn inspect_runtime_flag(tools: &dyn ToolRunner, bundle: &Path) -> Result<(), PackageError> {
    let output = run_success(
        tools,
        Path::new(CODESIGN),
        &[
            OsString::from("--display"),
            OsString::from("--verbose=4"),
            bundle.as_os_str().to_os_string(),
        ],
        "runtime inspection",
    )?;
    if display_has_runtime_flag(&output) {
        Ok(())
    } else {
        Err(PackageError::InspectionFailure)
    }
}

fn inspect_worker_entitlements(
    tools: &dyn ToolRunner,
    worker_bundle: &Path,
) -> Result<(), PackageError> {
    let output = display_entitlements(tools, worker_bundle)?;
    let xml = String::from_utf8_lossy(&output.stdout);
    let key_count = xml.matches("<key>").count();
    if key_count == 2
        && plist_boolean_is_true(&xml, APP_SANDBOX_ENTITLEMENT)
        && plist_boolean_is_true(&xml, HYPERVISOR_ENTITLEMENT)
    {
        Ok(())
    } else {
        Err(PackageError::InspectionFailure)
    }
}

fn inspect_outer_entitlements(
    tools: &dyn ToolRunner,
    outer_bundle: &Path,
) -> Result<(), PackageError> {
    let output = display_entitlements(tools, outer_bundle)?;
    let xml = String::from_utf8_lossy(&output.stdout);
    if xml.matches("<key>").count() == 0 {
        Ok(())
    } else {
        Err(PackageError::InspectionFailure)
    }
}

fn display_entitlements(tools: &dyn ToolRunner, bundle: &Path) -> Result<Output, PackageError> {
    run_success(
        tools,
        Path::new(CODESIGN),
        &[
            OsString::from("--display"),
            OsString::from("--entitlements"),
            OsString::from("-"),
            OsString::from("--xml"),
            bundle.as_os_str().to_os_string(),
        ],
        "entitlement inspection",
    )
}

fn display_has_runtime_flag(output: &Output) -> bool {
    let combined = [output.stdout.as_slice(), output.stderr.as_slice()].concat();
    String::from_utf8_lossy(&combined).lines().any(|line| {
        let Some(flags) = line.trim().strip_prefix("CodeDirectory ").and_then(|line| {
            line.split_ascii_whitespace()
                .find(|field| field.starts_with("flags="))
        }) else {
            return false;
        };
        let Some((_, flags)) = flags.split_once('(') else {
            return false;
        };
        let Some((flags, _)) = flags.split_once(')') else {
            return false;
        };
        flags.split(',').any(|flag| flag.trim() == "runtime")
    })
}

fn plist_boolean_is_true(xml: &str, key: &str) -> bool {
    let compact = xml
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect::<String>();
    compact.contains(&format!("<key>{key}</key><true/>"))
}

fn run_success(
    tools: &dyn ToolRunner,
    program: &Path,
    args: &[OsString],
    stage: &'static str,
) -> Result<Output, PackageError> {
    let output = tools
        .run(program, args)
        .map_err(|_| PackageError::ToolFailure(stage))?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(PackageError::ToolFailure(stage))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs::{self, File};
    use std::os::unix::fs::symlink;
    use std::os::unix::process::ExitStatusExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    #[derive(Debug)]
    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir().join(format!(
                "bangbang-bundle-package-{}-{id}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("test directory should be created");
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Debug, Default)]
    struct RecordingTools {
        calls: RefCell<Vec<Vec<OsString>>>,
        fail_stage: RefCell<Option<usize>>,
    }

    impl ToolRunner for RecordingTools {
        fn run(&self, program: &Path, args: &[OsString]) -> Result<Output, io::ErrorKind> {
            let mut call = vec![program.as_os_str().to_os_string()];
            call.extend_from_slice(args);
            let index = self.calls.borrow().len();
            self.calls.borrow_mut().push(call);
            if *self.fail_stage.borrow() == Some(index) {
                return Ok(output(1, b"", b"private tool detail"));
            }

            if program == Path::new(PLUTIL) {
                let key = args
                    .get(1)
                    .and_then(|value| value.to_str())
                    .expect("plutil key should exist");
                let plist = args
                    .last()
                    .and_then(|value| value.to_str())
                    .expect("plist path should exist");
                let value = match (plist.contains("BangbangWorker.app"), key) {
                    (true, "CFBundleIdentifier") => WORKER_BUNDLE_IDENTIFIER,
                    (true, "CFBundleExecutable") => WORKER_EXECUTABLE_NAME,
                    (false, "CFBundleIdentifier") => LAUNCHER_BUNDLE_IDENTIFIER,
                    (false, "CFBundleExecutable") => LAUNCHER_EXECUTABLE_NAME,
                    _ => "unexpected",
                };
                return Ok(output(0, format!("{value}\n").as_bytes(), b""));
            }
            if args.iter().any(|argument| argument == "--entitlements")
                && args.iter().any(|argument| argument == "--display")
            {
                let is_worker = args
                    .last()
                    .is_some_and(|path| path.to_string_lossy().contains(WORKER_BUNDLE_NAME));
                if !is_worker {
                    return Ok(output(0, b"", b""));
                }
                return Ok(output(
                    0,
                    b"<plist><dict><key>com.apple.security.app-sandbox</key><true/><key>com.apple.security.hypervisor</key><true/></dict></plist>",
                    b"",
                ));
            }
            if args.iter().any(|argument| argument == "--display") {
                return Ok(output(
                    0,
                    b"",
                    b"CodeDirectory v=20500 flags=0x10002(adhoc,runtime) hashes=1+1\n",
                ));
            }
            Ok(output(0, b"", b""))
        }
    }

    fn output(code: i32, stdout: &[u8], stderr: &[u8]) -> Output {
        Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        }
    }

    #[derive(Debug)]
    struct RenamePublisher;

    impl Publisher for RenamePublisher {
        fn publish(&self, source: &Path, destination: &Path) -> Result<(), io::ErrorKind> {
            fs::rename(source, destination).map_err(|err| err.kind())
        }
    }

    #[derive(Debug)]
    struct CollidingPublisher;

    impl Publisher for CollidingPublisher {
        fn publish(&self, _source: &Path, destination: &Path) -> Result<(), io::ErrorKind> {
            fs::create_dir(destination).expect("competing output should be created");
            fs::write(destination.join("sentinel"), b"competing")
                .expect("competing output should be populated");
            Err(io::ErrorKind::AlreadyExists)
        }
    }

    fn options(test_dir: &TestDir) -> PackageOptions {
        let launcher = test_dir.0.join("launcher");
        let worker = test_dir.0.join("worker");
        fs::write(&launcher, b"launcher").expect("launcher should be written");
        fs::write(&worker, b"worker").expect("worker should be written");
        PackageOptions {
            launcher_binary: launcher,
            worker_binary: worker,
            output_bundle: test_dir.0.join(OUTER_BUNDLE_NAME),
            signing_identity: OsString::from("-"),
            test_worker_resources: None,
        }
    }

    #[test]
    fn assembles_signs_inspects_and_publishes_fixed_layout() {
        let test_dir = TestDir::new();
        let options = options(&test_dir);
        let tools = RecordingTools::default();
        let published =
            build_bundle_with(&options, &tools, &RenamePublisher).expect("bundle should publish");
        assert_eq!(
            published,
            fs::canonicalize(&test_dir.0)
                .expect("test parent should canonicalize")
                .join(OUTER_BUNDLE_NAME)
        );
        assert!(
            published
                .join("Contents/MacOS")
                .join(LAUNCHER_EXECUTABLE_NAME)
                .is_file()
        );
        assert!(
            published
                .join("Contents/Helpers")
                .join(WORKER_BUNDLE_NAME)
                .join("Contents/MacOS")
                .join(WORKER_EXECUTABLE_NAME)
                .is_file()
        );

        let calls = tools.calls.borrow();
        let signing_calls = calls
            .iter()
            .filter(|call| call.iter().any(|argument| argument == "--sign"))
            .collect::<Vec<_>>();
        assert_eq!(signing_calls.len(), 2);
        assert!(
            signing_calls[0]
                .last()
                .expect("worker sign target should exist")
                .to_string_lossy()
                .contains(WORKER_BUNDLE_NAME)
        );
        assert!(
            !signing_calls[1]
                .last()
                .expect("outer sign target should exist")
                .to_string_lossy()
                .contains(WORKER_BUNDLE_NAME)
        );
        assert!(
            signing_calls
                .iter()
                .all(|call| call.iter().any(|argument| argument == "runtime"))
        );
    }

    #[test]
    fn refuses_existing_output_without_touching_it() {
        let test_dir = TestDir::new();
        let options = options(&test_dir);
        fs::create_dir(&options.output_bundle).expect("existing output should be created");
        let sentinel = options.output_bundle.join("sentinel");
        fs::write(&sentinel, b"owned").expect("sentinel should be written");
        assert_eq!(
            build_bundle_with(&options, &RecordingTools::default(), &RenamePublisher),
            Err(PackageError::OutputAlreadyExists)
        );
        assert_eq!(
            fs::read(sentinel).expect("sentinel should remain"),
            b"owned"
        );
    }

    #[test]
    fn signing_failure_leaves_no_final_or_staging_directory() {
        let test_dir = TestDir::new();
        let options = options(&test_dir);
        let tools = RecordingTools::default();
        *tools.fail_stage.borrow_mut() = Some(0);
        assert_eq!(
            build_bundle_with(&options, &tools, &RenamePublisher),
            Err(PackageError::ToolFailure("worker signing"))
        );
        assert!(!options.output_bundle.exists());
        let residue = fs::read_dir(&test_dir.0)
            .expect("test directory should be readable")
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".bangbang-bundle-stage-")
            });
        assert!(!residue, "private staging should be removed");
    }

    #[test]
    fn every_platform_tool_failure_leaves_no_output_or_stage() {
        let baseline_dir = TestDir::new();
        let baseline_options = options(&baseline_dir);
        let baseline_tools = RecordingTools::default();
        build_bundle_with(&baseline_options, &baseline_tools, &RenamePublisher)
            .expect("baseline bundle should publish");
        let call_count = baseline_tools.calls.borrow().len();
        assert!(call_count > 10, "all inspection stages should be recorded");

        for fail_stage in 0..call_count {
            let test_dir = TestDir::new();
            let options = options(&test_dir);
            let tools = RecordingTools::default();
            *tools.fail_stage.borrow_mut() = Some(fail_stage);
            assert!(
                matches!(
                    build_bundle_with(&options, &tools, &RenamePublisher),
                    Err(PackageError::ToolFailure(_))
                ),
                "tool stage {fail_stage} should fail closed"
            );
            assert!(!options.output_bundle.exists());
            assert!(!has_stage_residue(&test_dir));
        }
    }

    #[test]
    fn exclusive_publication_collision_preserves_competing_output() {
        let test_dir = TestDir::new();
        let options = options(&test_dir);
        assert_eq!(
            build_bundle_with(&options, &RecordingTools::default(), &CollidingPublisher),
            Err(PackageError::OutputAlreadyExists)
        );
        assert_eq!(
            fs::read(options.output_bundle.join("sentinel"))
                .expect("competing output should remain"),
            b"competing"
        );
        assert!(!has_stage_residue(&test_dir));
    }

    #[test]
    fn staging_cleanup_does_not_remove_replacement_directory() {
        let test_dir = TestDir::new();
        let staging = StagingDirectory::create(&test_dir.0).expect("staging should be created");
        assert_eq!(
            fs::symlink_metadata(staging.path())
                .expect("staging metadata should exist")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let original = staging.path().to_path_buf();
        let moved = test_dir.0.join("moved-owned-stage");
        fs::rename(&original, &moved).expect("owned stage should move");
        fs::create_dir(&original).expect("replacement stage path should be created");
        let sentinel = original.join("sentinel");
        fs::write(&sentinel, b"replacement").expect("replacement sentinel should be written");

        drop(staging);

        assert_eq!(
            fs::read(sentinel).expect("replacement should not be removed"),
            b"replacement"
        );
        assert!(
            moved.is_dir(),
            "original owned directory should remain moved"
        );
    }

    #[test]
    fn rejects_symlinked_test_resource_without_final_output() {
        let test_dir = TestDir::new();
        let mut options = options(&test_dir);
        let resources = test_dir.0.join("resources");
        fs::create_dir(&resources).expect("resources should be created");
        let source = test_dir.0.join("source");
        fs::write(&source, b"guest").expect("resource source should be written");
        symlink(&source, resources.join("guest-kernel"))
            .expect("resource symlink should be created");
        options.test_worker_resources = Some(resources);
        assert_eq!(
            build_bundle_with(&options, &RecordingTools::default(), &RenamePublisher),
            Err(PackageError::InvalidTestResources)
        );
        assert!(!options.output_bundle.exists());
    }

    #[test]
    fn rejects_test_resources_past_entry_limit() {
        let test_dir = TestDir::new();
        let mut options = options(&test_dir);
        let resources = test_dir.0.join("resources");
        fs::create_dir(&resources).expect("resources should be created");
        for index in 0..=MAX_TEST_RESOURCE_ENTRIES {
            fs::write(resources.join(format!("resource-{index}")), b"")
                .expect("resource should be written");
        }
        options.test_worker_resources = Some(resources);
        assert_eq!(
            build_bundle_with(&options, &RecordingTools::default(), &RenamePublisher),
            Err(PackageError::InvalidTestResources)
        );
        assert!(!has_stage_residue(&test_dir));
    }

    #[test]
    fn rejects_test_resources_past_depth_limit() {
        let test_dir = TestDir::new();
        let mut options = options(&test_dir);
        let resources = test_dir.0.join("resources");
        fs::create_dir(&resources).expect("resources should be created");
        let mut nested = resources.clone();
        for index in 0..=MAX_TEST_RESOURCE_DEPTH {
            nested = nested.join(format!("level-{index}"));
            fs::create_dir(&nested).expect("nested resource should be created");
        }
        options.test_worker_resources = Some(resources);
        assert_eq!(
            build_bundle_with(&options, &RecordingTools::default(), &RenamePublisher),
            Err(PackageError::InvalidTestResources)
        );
        assert!(!has_stage_residue(&test_dir));
    }

    #[test]
    fn rejects_test_resources_past_byte_limit() {
        let test_dir = TestDir::new();
        let mut options = options(&test_dir);
        let resources = test_dir.0.join("resources");
        fs::create_dir(&resources).expect("resources should be created");
        File::create(resources.join("oversized"))
            .expect("sparse resource should be created")
            .set_len(MAX_TEST_RESOURCE_BYTES + 1)
            .expect("sparse resource should be sized");
        options.test_worker_resources = Some(resources);
        assert_eq!(
            build_bundle_with(&options, &RecordingTools::default(), &RenamePublisher),
            Err(PackageError::InvalidTestResources)
        );
        assert!(!has_stage_residue(&test_dir));
    }

    #[test]
    fn inspection_helpers_require_exact_runtime_and_true_entitlements() {
        assert!(display_has_runtime_flag(&output(
            0,
            b"",
            b"CodeDirectory v=20500 flags=0x10002(adhoc,runtime) hashes=1+1\n"
        )));
        assert!(!display_has_runtime_flag(&output(
            0,
            b"",
            b"Executable=/private/runtime/Bangbang.app/Contents/MacOS/bangbang\nCodeDirectory v=20500 flags=0x2(adhoc) hashes=1+1\n"
        )));
        let true_xml = "<key>com.apple.security.app-sandbox</key>\n<true />";
        let false_xml = "<key>com.apple.security.app-sandbox</key><false/>";
        assert!(plist_boolean_is_true(true_xml, APP_SANDBOX_ENTITLEMENT));
        assert!(!plist_boolean_is_true(false_xml, APP_SANDBOX_ENTITLEMENT));
    }

    #[test]
    fn tool_failure_error_does_not_expose_tool_output_or_paths() {
        let display = PackageError::ToolFailure("worker signing").to_string();
        assert_eq!(display, "production bundle worker signing failed");
        assert!(!display.contains("private tool detail"));
        assert!(!display.contains("/private/secret"));
    }

    fn has_stage_residue(test_dir: &TestDir) -> bool {
        fs::read_dir(&test_dir.0)
            .expect("test directory should be readable")
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".bangbang-bundle-stage-")
            })
    }
}
