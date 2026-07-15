use std::ffi::{OsStr, OsString};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Cursor, Read};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Output;
#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(target_os = "macos")]
use std::thread;
use std::time::SystemTime;
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

use plist::Value;

#[cfg(target_os = "macos")]
use crate::VMNET_AUTHORIZATION_PROBE_ARG;
use crate::layout::{
    APP_SANDBOX_ENTITLEMENT, APPLICATION_IDENTIFIER_ENTITLEMENT, HYPERVISOR_ENTITLEMENT,
    TEAM_IDENTIFIER_ENTITLEMENT, VMNET_ENTITLEMENT,
};
use crate::provisioning_profile::{ApprovedProvisioningProfile, MAX_PROVISIONING_PROFILE_BYTES};
use crate::{
    LAUNCHER_BUNDLE_IDENTIFIER, LAUNCHER_EXECUTABLE_NAME, OUTER_BUNDLE_NAME, PackageError,
    PackageOptions, PackageProfile, WORKER_BUNDLE_IDENTIFIER, WORKER_BUNDLE_NAME,
    WORKER_EXECUTABLE_NAME,
};

const LAUNCHER_INFO_PLIST: &[u8] = include_bytes!("../../../packaging/macos/Bangbang-Info.plist");
const WORKER_INFO_PLIST: &[u8] =
    include_bytes!("../../../packaging/macos/BangbangWorker-Info.plist");
const WORKER_ENTITLEMENTS: &[u8] =
    include_bytes!("../../../packaging/macos/BangbangWorker.entitlements.plist");
const CODESIGN: &str = "/usr/bin/codesign";
const PLUTIL: &str = "/usr/bin/plutil";
const SECURITY: &str = "/usr/bin/security";
const AUTHORIZATION_PROBE_BUNDLE_NAME: &str = "VmnetAuthorizationProbe.app";
#[cfg(target_os = "macos")]
const AUTHORIZATION_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_SIGNING_CERTIFICATE_BYTES: usize = 64 * 1024;
const MAX_TEST_RESOURCE_ENTRIES: usize = 128;
const MAX_TEST_RESOURCE_DEPTH: usize = 8;
const MAX_TEST_RESOURCE_BYTES: u64 = 1024 * 1024 * 1024;
static NEXT_STAGE_ID: AtomicU64 = AtomicU64::new(0);

/// Builds, inspects, and exclusively publishes one production app bundle.
pub fn build_bundle(options: &PackageOptions) -> Result<PathBuf, PackageError> {
    #[cfg(target_os = "macos")]
    {
        build_bundle_with(
            options,
            &SystemTools,
            &SystemAuthorization::production(),
            &SystemPublisher,
        )
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = options;
        Err(PackageError::UnsupportedPlatform)
    }
}

/// Assembles and authorizes a vmnet bundle transaction without publishing it.
pub fn preflight_bundle(options: &PackageOptions) -> Result<(), PackageError> {
    #[cfg(target_os = "macos")]
    {
        preflight_bundle_with(options, &SystemTools, &SystemAuthorization::production())
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

trait AuthorizationRunner {
    fn authorize(&self, executable: &Path) -> Result<(), io::ErrorKind>;
}

#[derive(Debug)]
#[cfg(target_os = "macos")]
struct SystemAuthorization {
    timeout: Duration,
}

#[cfg(target_os = "macos")]
impl SystemAuthorization {
    const fn production() -> Self {
        Self {
            timeout: AUTHORIZATION_TIMEOUT,
        }
    }
}

#[cfg(target_os = "macos")]
impl AuthorizationRunner for SystemAuthorization {
    fn authorize(&self, executable: &Path) -> Result<(), io::ErrorKind> {
        let mut child = Command::new(executable)
            .arg(VMNET_AUTHORIZATION_PROBE_ARG)
            .env_clear()
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| err.kind())?;
        let deadline = Instant::now() + self.timeout;
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(()),
                Ok(Some(_)) => return Err(io::ErrorKind::PermissionDenied),
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(io::ErrorKind::TimedOut);
                }
                Err(err) => {
                    let kind = err.kind();
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(kind);
                }
            }
        }
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
    authorization: &dyn AuthorizationRunner,
    publisher: &dyn Publisher,
) -> Result<PathBuf, PackageError> {
    let assembled = assemble_bundle_with(options, tools)?;
    authorize_vmnet_bundle(tools, authorization, &assembled)?;

    match publisher.publish(&assembled.staged_bundle, &assembled.output_bundle) {
        Ok(()) => Ok(assembled.output_bundle.clone()),
        Err(io::ErrorKind::AlreadyExists) => Err(PackageError::OutputAlreadyExists),
        Err(kind) => Err(PackageError::Publication(kind)),
    }
}

fn preflight_bundle_with(
    options: &PackageOptions,
    tools: &dyn ToolRunner,
    authorization: &dyn AuthorizationRunner,
) -> Result<(), PackageError> {
    if options.profile != PackageProfile::Vmnet {
        return Err(PackageError::InvalidInput);
    }
    let assembled = assemble_bundle_with(options, tools)?;
    authorize_vmnet_bundle(tools, authorization, &assembled)
}

fn assemble_bundle_with(
    options: &PackageOptions,
    tools: &dyn ToolRunner,
) -> Result<AssembledBundle, PackageError> {
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

    let profile = match inputs.provisioning_profile {
        Some(bytes) => {
            let embedded = staged_worker_bundle.join("Contents/embedded.provisionprofile");
            write_file(&embedded, &bytes)?;
            let approved = decode_provisioning_profile(tools, &embedded, staging.path())?;
            write_file(&entitlement_file, &approved.entitlement_plist())?;
            ResolvedProfile::Vmnet { approved, bytes }
        }
        None => {
            write_file(&entitlement_file, WORKER_ENTITLEMENTS)?;
            ResolvedProfile::Networkless
        }
    };

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
        &profile,
        staging.path(),
    )?;

    Ok(AssembledBundle {
        _staging: staging,
        output_bundle: inputs.output_bundle,
        staged_bundle,
        profile,
        signing_identity: inputs.signing_identity,
        entitlement_file,
    })
}

struct AssembledBundle {
    _staging: StagingDirectory,
    output_bundle: PathBuf,
    staged_bundle: PathBuf,
    profile: ResolvedProfile,
    signing_identity: OsString,
    entitlement_file: PathBuf,
}

enum ResolvedProfile {
    Networkless,
    Vmnet {
        approved: ApprovedProvisioningProfile,
        bytes: Vec<u8>,
    },
}

impl ResolvedProfile {
    fn worker_requirement(&self) -> String {
        // Security requirement syntax reliably tests entitlement presence, but
        // not typed Boolean equality on every supported macOS release. The
        // immediately following plist inspection enforces true values and the
        // exact closed entitlement set.
        match self {
            Self::Networkless => format!(
                "identifier \"{WORKER_BUNDLE_IDENTIFIER}\" and entitlement[\"{APP_SANDBOX_ENTITLEMENT}\"] exists and entitlement[\"{HYPERVISOR_ENTITLEMENT}\"] exists"
            ),
            Self::Vmnet { approved, .. } => format!(
                "identifier \"{WORKER_BUNDLE_IDENTIFIER}\" and certificate leaf[subject.OU] = \"{}\" and entitlement[\"{APP_SANDBOX_ENTITLEMENT}\"] exists and entitlement[\"{HYPERVISOR_ENTITLEMENT}\"] exists and entitlement[\"{VMNET_ENTITLEMENT}\"] exists and entitlement[\"{APPLICATION_IDENTIFIER_ENTITLEMENT}\"] = \"{}\" and entitlement[\"{TEAM_IDENTIFIER_ENTITLEMENT}\"] = \"{}\"",
                approved.team_identifier(),
                approved.application_identifier(),
                approved.team_identifier(),
            ),
        }
    }
}

struct ValidatedInputs {
    launcher_binary: PathBuf,
    worker_binary: PathBuf,
    output_parent: PathBuf,
    output_bundle: PathBuf,
    signing_identity: OsString,
    provisioning_profile: Option<Vec<u8>>,
    test_worker_resources: Option<PathBuf>,
}

impl ValidatedInputs {
    fn new(options: &PackageOptions) -> Result<Self, PackageError> {
        require_plain_file(&options.launcher_binary)?;
        require_plain_file(&options.worker_binary)?;
        if options.signing_identity.is_empty() {
            return Err(PackageError::InvalidInput);
        }
        let provisioning_profile = match options.profile {
            PackageProfile::Networkless if options.provisioning_profile.is_none() => None,
            PackageProfile::Vmnet
                if options.signing_identity != OsStr::new("-")
                    && options.provisioning_profile.is_some()
                    && options.test_worker_resources.is_none() =>
            {
                Some(read_provisioning_profile(
                    options
                        .provisioning_profile
                        .as_deref()
                        .ok_or(PackageError::InvalidProvisioningProfile)?,
                )?)
            }
            PackageProfile::Networkless | PackageProfile::Vmnet => {
                return Err(PackageError::InvalidInput);
            }
        };
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
            provisioning_profile,
            test_worker_resources,
        })
    }
}

fn read_provisioning_profile(path: &Path) -> Result<Vec<u8>, PackageError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| PackageError::InvalidProvisioningProfile)?;
    let metadata = file
        .metadata()
        .map_err(|_| PackageError::InvalidProvisioningProfile)?;
    if !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > MAX_PROVISIONING_PROFILE_BYTES as u64
    {
        return Err(PackageError::InvalidProvisioningProfile);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take(MAX_PROVISIONING_PROFILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| PackageError::InvalidProvisioningProfile)?;
    if bytes.is_empty() || bytes.len() > MAX_PROVISIONING_PROFILE_BYTES {
        return Err(PackageError::InvalidProvisioningProfile);
    }
    Ok(bytes)
}

fn decode_provisioning_profile(
    tools: &dyn ToolRunner,
    embedded: &Path,
    scratch: &Path,
) -> Result<ApprovedProvisioningProfile, PackageError> {
    let decoded = scratch.join("decoded-provisioning-profile.plist");
    let output = tools
        .run(
            Path::new(SECURITY),
            &[
                OsString::from("cms"),
                OsString::from("-D"),
                OsString::from("-i"),
                embedded.as_os_str().to_os_string(),
                OsString::from("-o"),
                decoded.as_os_str().to_os_string(),
            ],
        )
        .map_err(|_| PackageError::InvalidProvisioningProfile)?;
    if !output.status.success() {
        return Err(PackageError::InvalidProvisioningProfile);
    }
    let bytes = read_bounded_plain_file(&decoded, MAX_PROVISIONING_PROFILE_BYTES)
        .map_err(|_| PackageError::InvalidProvisioningProfile)?;
    ApprovedProvisioningProfile::parse(&bytes, SystemTime::now())
}

fn read_bounded_plain_file(path: &Path, limit: usize) -> Result<Vec<u8>, io::ErrorKind> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|err| err.kind())?;
    let metadata = file.metadata().map_err(|err| err.kind())?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > limit as u64 {
        return Err(io::ErrorKind::InvalidData);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| err.kind())?;
    if bytes.is_empty() || bytes.len() > limit {
        return Err(io::ErrorKind::InvalidData);
    }
    Ok(bytes)
}

fn authorize_vmnet_bundle(
    tools: &dyn ToolRunner,
    authorization: &dyn AuthorizationRunner,
    assembled: &AssembledBundle,
) -> Result<(), PackageError> {
    let ResolvedProfile::Vmnet { bytes, .. } = &assembled.profile else {
        return Ok(());
    };
    let probe_bundle = assembled
        ._staging
        .path()
        .join(AUTHORIZATION_PROBE_BUNDLE_NAME);
    let probe_macos = probe_bundle.join("Contents/MacOS");
    let probe_executable = probe_macos.join(WORKER_EXECUTABLE_NAME);
    let probe_info = probe_bundle.join("Contents/Info.plist");
    let probe_profile = probe_bundle.join("Contents/embedded.provisionprofile");
    create_dir(&probe_macos)?;
    let current_executable =
        std::env::current_exe().map_err(|_| PackageError::AuthorizationBlocked)?;
    require_plain_file(&current_executable).map_err(|_| PackageError::AuthorizationBlocked)?;
    copy_executable(&current_executable, &probe_executable)?;
    write_file(&probe_info, WORKER_INFO_PLIST)?;
    write_file(&probe_profile, bytes)?;
    sign_worker(
        tools,
        &assembled.signing_identity,
        &assembled.entitlement_file,
        &probe_bundle,
    )?;
    inspect_worker_bundle(
        tools,
        &probe_bundle,
        &probe_info,
        &assembled.profile,
        &assembled._staging.path().join("probe-signing-certificate"),
    )?;
    authorization
        .authorize(&probe_executable)
        .map_err(|_| PackageError::AuthorizationBlocked)
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
    profile: &ResolvedProfile,
    scratch: &Path,
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
    inspect_worker_bundle(
        tools,
        worker_bundle,
        worker_info,
        profile,
        &scratch.join("worker-signing-certificate"),
    )?;
    verify_requirement(
        tools,
        outer_bundle,
        &format!(
            "identifier \"{LAUNCHER_BUNDLE_IDENTIFIER}\" and entitlement[\"{APP_SANDBOX_ENTITLEMENT}\"] absent and entitlement[\"{HYPERVISOR_ENTITLEMENT}\"] absent and entitlement[\"{VMNET_ENTITLEMENT}\"] absent and entitlement[\"{APPLICATION_IDENTIFIER_ENTITLEMENT}\"] absent and entitlement[\"{TEAM_IDENTIFIER_ENTITLEMENT}\"] absent"
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
    inspect_outer_entitlements(tools, outer_bundle)
}

fn inspect_worker_bundle(
    tools: &dyn ToolRunner,
    worker_bundle: &Path,
    worker_info: &Path,
    profile: &ResolvedProfile,
    certificate_prefix: &Path,
) -> Result<(), PackageError> {
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
    verify_requirement(tools, worker_bundle, &profile.worker_requirement())?;
    inspect_runtime_flag(tools, worker_bundle)?;
    inspect_worker_entitlements(tools, worker_bundle, profile)?;
    inspect_embedded_profile(worker_bundle, profile)?;
    if let ResolvedProfile::Vmnet { approved, .. } = profile {
        let leaf = extract_leaf_certificate(tools, worker_bundle, certificate_prefix)?;
        if !approved.permits_certificate(&leaf) {
            return Err(PackageError::InspectionFailure);
        }
    }
    Ok(())
}

fn inspect_embedded_profile(
    worker_bundle: &Path,
    profile: &ResolvedProfile,
) -> Result<(), PackageError> {
    let embedded = worker_bundle.join("Contents/embedded.provisionprofile");
    match profile {
        ResolvedProfile::Networkless => match fs::symlink_metadata(embedded) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Ok(_) | Err(_) => Err(PackageError::InspectionFailure),
        },
        ResolvedProfile::Vmnet { bytes, .. } => {
            let embedded = read_bounded_plain_file(&embedded, MAX_PROVISIONING_PROFILE_BYTES)
                .map_err(|_| PackageError::InspectionFailure)?;
            if &embedded == bytes {
                Ok(())
            } else {
                Err(PackageError::InspectionFailure)
            }
        }
    }
}

fn extract_leaf_certificate(
    tools: &dyn ToolRunner,
    worker_bundle: &Path,
    certificate_prefix: &Path,
) -> Result<Vec<u8>, PackageError> {
    run_success(
        tools,
        Path::new(CODESIGN),
        &[
            OsString::from("--display"),
            OsString::from("--extract-certificates"),
            certificate_prefix.as_os_str().to_os_string(),
            worker_bundle.as_os_str().to_os_string(),
        ],
        "certificate inspection",
    )?;
    let mut leaf = certificate_prefix.as_os_str().to_os_string();
    leaf.push("0");
    read_bounded_plain_file(Path::new(&leaf), MAX_SIGNING_CERTIFICATE_BYTES)
        .map_err(|_| PackageError::InspectionFailure)
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
    profile: &ResolvedProfile,
) -> Result<(), PackageError> {
    let output = display_entitlements(tools, worker_bundle)?;
    let value = Value::from_reader(Cursor::new(output.stdout))
        .map_err(|_| PackageError::InspectionFailure)?;
    let dictionary = value
        .as_dictionary()
        .ok_or(PackageError::InspectionFailure)?;
    let common = dictionary
        .get(APP_SANDBOX_ENTITLEMENT)
        .and_then(Value::as_boolean)
        == Some(true)
        && dictionary
            .get(HYPERVISOR_ENTITLEMENT)
            .and_then(Value::as_boolean)
            == Some(true);
    let matches = match profile {
        ResolvedProfile::Networkless => dictionary.len() == 2 && common,
        ResolvedProfile::Vmnet { approved, .. } => {
            dictionary.len() == 5
                && common
                && dictionary
                    .get(VMNET_ENTITLEMENT)
                    .and_then(Value::as_boolean)
                    == Some(true)
                && dictionary
                    .get(APPLICATION_IDENTIFIER_ENTITLEMENT)
                    .and_then(Value::as_string)
                    == Some(approved.application_identifier())
                && dictionary
                    .get(TEAM_IDENTIFIER_ENTITLEMENT)
                    .and_then(Value::as_string)
                    == Some(approved.team_identifier())
        }
    };
    if matches {
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
    if output.stdout.iter().all(u8::is_ascii_whitespace) {
        Ok(())
    } else {
        let value = Value::from_reader(Cursor::new(output.stdout))
            .map_err(|_| PackageError::InspectionFailure)?;
        match value.as_dictionary() {
            Some(dictionary) if dictionary.is_empty() => Ok(()),
            Some(_) | None => Err(PackageError::InspectionFailure),
        }
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

#[cfg(test)]
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
    use std::ffi::CString;
    use std::fs::{self, File};
    use std::os::unix::ffi::OsStrExt;
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
        vmnet: RefCell<bool>,
        decoded_profile: RefCell<Option<Vec<u8>>>,
        leaf_certificate: RefCell<Option<Vec<u8>>>,
        worker_entitlements: RefCell<Option<Vec<u8>>>,
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

            if program == Path::new(SECURITY) {
                *self.vmnet.borrow_mut() = true;
                let destination =
                    argument_after(args, "-o").expect("decoded profile destination should exist");
                fs::write(
                    destination,
                    self.decoded_profile
                        .borrow()
                        .as_deref()
                        .unwrap_or(VALID_DECODED_PROFILE),
                )
                .expect("decoded profile should be synthesized");
                return Ok(output(0, b"", b""));
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
                let is_worker = plist.contains(WORKER_BUNDLE_NAME)
                    || plist.contains(AUTHORIZATION_PROBE_BUNDLE_NAME);
                let value = match (is_worker, key) {
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
                let is_worker = args.last().is_some_and(|path| {
                    let path = path.to_string_lossy();
                    path.contains(WORKER_BUNDLE_NAME)
                        || path.contains(AUTHORIZATION_PROBE_BUNDLE_NAME)
                });
                if !is_worker {
                    return Ok(output(0, b"", b""));
                }
                if let Some(entitlements) = self.worker_entitlements.borrow().as_deref() {
                    return Ok(output(0, entitlements, b""));
                }
                if *self.vmnet.borrow() {
                    return Ok(output(0, VMNET_ENTITLEMENTS, b""));
                }
                return Ok(output(
                    0,
                    b"<plist><dict><key>com.apple.security.app-sandbox</key><true/><key>com.apple.security.hypervisor</key><true/></dict></plist>",
                    b"",
                ));
            }
            if args
                .iter()
                .any(|argument| argument == "--extract-certificates")
            {
                let prefix = argument_after(args, "--extract-certificates")
                    .expect("certificate prefix should exist");
                let mut leaf = prefix.as_os_str().to_os_string();
                leaf.push("0");
                fs::write(
                    leaf,
                    self.leaf_certificate
                        .borrow()
                        .as_deref()
                        .unwrap_or(&[1, 2, 3]),
                )
                .expect("leaf certificate should be synthesized");
                return Ok(output(0, b"", b""));
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

    const VALID_DECODED_PROFILE: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>TeamIdentifier</key><array><string>TEAM123456</string></array>
<key>ApplicationIdentifierPrefix</key><array><string>APPID12345</string></array>
<key>CreationDate</key><date>2025-01-01T00:00:00Z</date>
<key>ExpirationDate</key><date>2035-01-01T00:00:00Z</date>
<key>DeveloperCertificates</key><array><data>AQID</data></array>
<key>Entitlements</key><dict>
<key>com.apple.application-identifier</key><string>APPID12345.dev.bangbang.worker</string>
<key>com.apple.developer.team-identifier</key><string>TEAM123456</string>
<key>com.apple.vm.networking</key><true/>
</dict></dict></plist>"#;

    const VMNET_ENTITLEMENTS: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>com.apple.security.app-sandbox</key><true/>
<key>com.apple.security.hypervisor</key><true/>
<key>com.apple.vm.networking</key><true/>
<key>com.apple.application-identifier</key><string>APPID12345.dev.bangbang.worker</string>
<key>com.apple.developer.team-identifier</key><string>TEAM123456</string>
</dict></plist>"#;

    fn argument_after<'a>(args: &'a [OsString], option: &str) -> Option<&'a Path> {
        args.iter()
            .position(|argument| argument == option)
            .and_then(|index| args.get(index + 1))
            .map(Path::new)
    }

    #[derive(Debug, Default)]
    struct RecordingAuthorization {
        calls: RefCell<Vec<PathBuf>>,
        failure: RefCell<Option<io::ErrorKind>>,
    }

    impl AuthorizationRunner for RecordingAuthorization {
        fn authorize(&self, executable: &Path) -> Result<(), io::ErrorKind> {
            self.calls.borrow_mut().push(executable.to_path_buf());
            match *self.failure.borrow() {
                Some(kind) => Err(kind),
                None => Ok(()),
            }
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
            profile: PackageProfile::Networkless,
            provisioning_profile: None,
            test_worker_resources: None,
        }
    }

    fn vmnet_options(test_dir: &TestDir) -> PackageOptions {
        let mut options = options(test_dir);
        let provisioning_profile = test_dir.0.join("approved.provisionprofile");
        fs::write(&provisioning_profile, b"captured-cms-profile")
            .expect("synthetic CMS input should be written");
        options.signing_identity = OsString::from("Developer ID Application: Private");
        options.profile = PackageProfile::Vmnet;
        options.provisioning_profile = Some(provisioning_profile);
        options
    }

    #[test]
    fn assembles_signs_inspects_and_publishes_fixed_layout() {
        let test_dir = TestDir::new();
        let options = options(&test_dir);
        let tools = RecordingTools::default();
        let authorization = RecordingAuthorization::default();
        let published = build_bundle_with(&options, &tools, &authorization, &RenamePublisher)
            .expect("bundle should publish");
        assert!(authorization.calls.borrow().is_empty());
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
        assert!(
            !published
                .join("Contents/Helpers")
                .join(WORKER_BUNDLE_NAME)
                .join("Contents/embedded.provisionprofile")
                .exists()
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
    fn vmnet_build_embeds_captured_profile_and_authorizes_only_trusted_probe() {
        let test_dir = TestDir::new();
        let options = vmnet_options(&test_dir);
        let supplied_worker = options.worker_binary.clone();
        let tools = RecordingTools::default();
        let authorization = RecordingAuthorization::default();
        let published = build_bundle_with(&options, &tools, &authorization, &RenamePublisher)
            .expect("synthetic approved vmnet bundle should publish");

        assert_eq!(
            fs::read(
                published
                    .join("Contents/Helpers")
                    .join(WORKER_BUNDLE_NAME)
                    .join("Contents/embedded.provisionprofile")
            )
            .expect("embedded profile should be readable"),
            b"captured-cms-profile"
        );
        let authorization_calls = authorization.calls.borrow();
        assert_eq!(authorization_calls.len(), 1);
        assert!(
            authorization_calls[0]
                .to_string_lossy()
                .contains(AUTHORIZATION_PROBE_BUNDLE_NAME)
        );
        assert_ne!(authorization_calls[0], supplied_worker);

        let calls = tools.calls.borrow();
        let sign_targets = calls
            .iter()
            .filter(|call| call.iter().any(|argument| argument == "--sign"))
            .filter_map(|call| call.last())
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(sign_targets.len(), 3);
        assert!(sign_targets[0].contains(WORKER_BUNDLE_NAME));
        assert!(sign_targets[1].ends_with(OUTER_BUNDLE_NAME));
        assert!(sign_targets[2].contains(AUTHORIZATION_PROBE_BUNDLE_NAME));
        assert!(calls.iter().any(|call| {
            call.iter()
                .any(|argument| argument == "--extract-certificates")
        }));
        let vmnet_requirement = calls
            .iter()
            .flat_map(|call| call.iter())
            .filter_map(|argument| argument.to_str())
            .find(|argument| argument.contains("certificate leaf[subject.OU]"))
            .expect("vmnet requirement inspection should run");
        for entitlement in [
            APP_SANDBOX_ENTITLEMENT,
            HYPERVISOR_ENTITLEMENT,
            VMNET_ENTITLEMENT,
        ] {
            assert!(vmnet_requirement.contains(&format!("entitlement[\"{entitlement}\"] exists")));
        }
        assert!(!vmnet_requirement.contains("] = true"));
        assert!(vmnet_requirement.contains("APPID12345.dev.bangbang.worker"));
        assert!(vmnet_requirement.contains("TEAM123456"));
    }

    #[test]
    fn vmnet_authorization_failure_never_publishes_and_cleans_staging() {
        #[derive(Debug)]
        struct ForbiddenPublisher;

        impl Publisher for ForbiddenPublisher {
            fn publish(&self, _source: &Path, _destination: &Path) -> Result<(), io::ErrorKind> {
                panic!("publication must not run before authorization succeeds");
            }
        }

        let test_dir = TestDir::new();
        let options = vmnet_options(&test_dir);
        let authorization = RecordingAuthorization::default();
        *authorization.failure.borrow_mut() = Some(io::ErrorKind::TimedOut);
        assert_eq!(
            build_bundle_with(
                &options,
                &RecordingTools::default(),
                &authorization,
                &ForbiddenPublisher,
            ),
            Err(PackageError::AuthorizationBlocked)
        );
        assert!(!options.output_bundle.exists());
        assert!(!has_stage_residue(&test_dir));
    }

    #[test]
    fn vmnet_preflight_uses_full_transaction_without_publication() {
        let test_dir = TestDir::new();
        let options = vmnet_options(&test_dir);
        let authorization = RecordingAuthorization::default();
        preflight_bundle_with(&options, &RecordingTools::default(), &authorization)
            .expect("synthetic preflight should pass");
        assert_eq!(authorization.calls.borrow().len(), 1);
        assert!(!options.output_bundle.exists());
        assert!(!has_stage_residue(&test_dir));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn system_authorization_uses_exact_argument_cwd_and_deadline() {
        let test_dir = TestDir::new();
        let success = test_dir.0.join("authorization-success");
        fs::write(
            &success,
            format!(
                "#!/bin/sh\n[ \"$#\" -eq 1 ] && [ \"$1\" = \"{VMNET_AUTHORIZATION_PROBE_ARG}\" ] && [ \"$(pwd)\" = \"/\" ] || exit 9\nexit 0\n"
            ),
        )
        .expect("success probe should be written");
        fs::set_permissions(&success, fs::Permissions::from_mode(0o700))
            .expect("success probe should be executable");
        SystemAuthorization {
            timeout: Duration::from_secs(2),
        }
        .authorize(&success)
        .expect("exact probe process should pass");

        let timeout = test_dir.0.join("authorization-timeout");
        fs::write(&timeout, "#!/bin/sh\nwhile :; do :; done\n")
            .expect("timeout probe should be written");
        fs::set_permissions(&timeout, fs::Permissions::from_mode(0o700))
            .expect("timeout probe should be executable");
        assert_eq!(
            SystemAuthorization {
                timeout: Duration::from_millis(25),
            }
            .authorize(&timeout),
            Err(io::ErrorKind::TimedOut)
        );
    }

    #[test]
    fn vmnet_leaf_mismatch_fails_before_authorization() {
        let test_dir = TestDir::new();
        let options = vmnet_options(&test_dir);
        let tools = RecordingTools::default();
        *tools.leaf_certificate.borrow_mut() = Some(vec![9, 9, 9]);
        let authorization = RecordingAuthorization::default();
        assert_eq!(
            build_bundle_with(&options, &tools, &authorization, &RenamePublisher),
            Err(PackageError::InspectionFailure)
        );
        assert!(authorization.calls.borrow().is_empty());
        assert!(!options.output_bundle.exists());
        assert!(!has_stage_residue(&test_dir));
    }

    #[test]
    fn vmnet_uses_only_once_captured_profile_bytes_after_path_substitution() {
        #[derive(Debug)]
        struct ReplacingTools<'a> {
            inner: &'a RecordingTools,
            source: PathBuf,
        }

        impl ToolRunner for ReplacingTools<'_> {
            fn run(&self, program: &Path, args: &[OsString]) -> Result<Output, io::ErrorKind> {
                if program == Path::new(SECURITY) {
                    fs::write(&self.source, b"substituted-profile-must-not-be-used")
                        .expect("profile pathname should be substituted");
                }
                self.inner.run(program, args)
            }
        }

        let test_dir = TestDir::new();
        let options = vmnet_options(&test_dir);
        let source = options
            .provisioning_profile
            .clone()
            .expect("profile path should exist");
        let inner = RecordingTools::default();
        let tools = ReplacingTools {
            inner: &inner,
            source,
        };
        let published = build_bundle_with(
            &options,
            &tools,
            &RecordingAuthorization::default(),
            &RenamePublisher,
        )
        .expect("captured profile should publish");
        assert_eq!(
            fs::read(worker_bundle_path(&published).join("Contents/embedded.provisionprofile"))
                .expect("embedded profile should be readable"),
            b"captured-cms-profile"
        );
    }

    #[test]
    fn vmnet_decode_failure_or_oversized_output_is_redacted_and_not_authorized() {
        let failure_dir = TestDir::new();
        let failure_options = vmnet_options(&failure_dir);
        let failed_tools = RecordingTools::default();
        *failed_tools.fail_stage.borrow_mut() = Some(0);
        let authorization = RecordingAuthorization::default();
        assert_eq!(
            build_bundle_with(
                &failure_options,
                &failed_tools,
                &authorization,
                &RenamePublisher,
            ),
            Err(PackageError::InvalidProvisioningProfile)
        );
        assert!(authorization.calls.borrow().is_empty());
        assert!(
            !PackageError::InvalidProvisioningProfile
                .to_string()
                .contains("private tool detail")
        );

        let oversized_dir = TestDir::new();
        let oversized_options = vmnet_options(&oversized_dir);
        let oversized_tools = RecordingTools::default();
        *oversized_tools.decoded_profile.borrow_mut() =
            Some(vec![0_u8; MAX_PROVISIONING_PROFILE_BYTES + 1]);
        assert_eq!(
            build_bundle_with(
                &oversized_options,
                &oversized_tools,
                &RecordingAuthorization::default(),
                &RenamePublisher,
            ),
            Err(PackageError::InvalidProvisioningProfile)
        );
        assert!(!has_stage_residue(&oversized_dir));
    }

    #[test]
    fn package_profile_relationships_fail_closed() {
        let test_dir = TestDir::new();

        let mut networkless_with_profile = options(&test_dir);
        networkless_with_profile.provisioning_profile = Some(test_dir.0.join("unused"));
        assert!(matches!(
            ValidatedInputs::new(&networkless_with_profile),
            Err(PackageError::InvalidInput)
        ));

        let mut adhoc_vmnet = vmnet_options(&test_dir);
        adhoc_vmnet.signing_identity = OsString::from("-");
        assert!(matches!(
            ValidatedInputs::new(&adhoc_vmnet),
            Err(PackageError::InvalidInput)
        ));

        let mut missing_profile = vmnet_options(&test_dir);
        missing_profile.provisioning_profile = None;
        assert!(matches!(
            ValidatedInputs::new(&missing_profile),
            Err(PackageError::InvalidInput)
        ));

        let mut test_overlay = vmnet_options(&test_dir);
        let resources = test_dir.0.join("vmnet-resources");
        fs::create_dir(&resources).expect("resource directory should be created");
        test_overlay.test_worker_resources = Some(resources);
        assert!(matches!(
            ValidatedInputs::new(&test_overlay),
            Err(PackageError::InvalidInput)
        ));
    }

    #[test]
    fn rejects_unsafe_or_oversized_provisioning_input() {
        let test_dir = TestDir::new();
        let options = vmnet_options(&test_dir);
        let profile = options
            .provisioning_profile
            .as_ref()
            .expect("profile path should exist");
        let target = test_dir.0.join("profile-target");
        fs::write(&target, b"profile").expect("target should be written");
        fs::remove_file(profile).expect("original profile should be removed");
        symlink(&target, profile).expect("profile symlink should be created");
        assert!(matches!(
            ValidatedInputs::new(&options),
            Err(PackageError::InvalidProvisioningProfile)
        ));

        fs::remove_file(profile).expect("profile symlink should be removed");
        let profile_c = CString::new(profile.as_os_str().as_bytes())
            .expect("test profile path must not contain NUL");
        // SAFETY: `profile_c` is a live NUL-terminated pathname and the mode is
        // an ordinary owner-only FIFO mode for this isolated test directory.
        assert_eq!(unsafe { libc::mkfifo(profile_c.as_ptr(), 0o600) }, 0);
        assert!(matches!(
            ValidatedInputs::new(&options),
            Err(PackageError::InvalidProvisioningProfile)
        ));

        fs::remove_file(profile).expect("profile FIFO should be removed");
        File::create(profile)
            .expect("oversized profile should be created")
            .set_len(MAX_PROVISIONING_PROFILE_BYTES as u64 + 1)
            .expect("oversized profile should be sized");
        assert!(matches!(
            ValidatedInputs::new(&options),
            Err(PackageError::InvalidProvisioningProfile)
        ));
    }

    #[test]
    fn refuses_existing_output_without_touching_it() {
        let test_dir = TestDir::new();
        let options = options(&test_dir);
        fs::create_dir(&options.output_bundle).expect("existing output should be created");
        let sentinel = options.output_bundle.join("sentinel");
        fs::write(&sentinel, b"owned").expect("sentinel should be written");
        assert_eq!(
            build_bundle_with(
                &options,
                &RecordingTools::default(),
                &RecordingAuthorization::default(),
                &RenamePublisher,
            ),
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
            build_bundle_with(
                &options,
                &tools,
                &RecordingAuthorization::default(),
                &RenamePublisher,
            ),
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
        build_bundle_with(
            &baseline_options,
            &baseline_tools,
            &RecordingAuthorization::default(),
            &RenamePublisher,
        )
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
                    build_bundle_with(
                        &options,
                        &tools,
                        &RecordingAuthorization::default(),
                        &RenamePublisher,
                    ),
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
            build_bundle_with(
                &options,
                &RecordingTools::default(),
                &RecordingAuthorization::default(),
                &CollidingPublisher,
            ),
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
            build_bundle_with(
                &options,
                &RecordingTools::default(),
                &RecordingAuthorization::default(),
                &RenamePublisher,
            ),
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
            build_bundle_with(
                &options,
                &RecordingTools::default(),
                &RecordingAuthorization::default(),
                &RenamePublisher,
            ),
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
            build_bundle_with(
                &options,
                &RecordingTools::default(),
                &RecordingAuthorization::default(),
                &RenamePublisher,
            ),
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
            build_bundle_with(
                &options,
                &RecordingTools::default(),
                &RecordingAuthorization::default(),
                &RenamePublisher,
            ),
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

    #[test]
    fn package_options_debug_redacts_every_caller_value() {
        let test_dir = TestDir::new();
        let options = vmnet_options(&test_dir);
        let debug = format!("{options:?}");
        assert!(debug.contains("Vmnet"));
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("Developer ID"));
        assert!(!debug.contains("approved.provisionprofile"));
        assert!(!debug.contains("launcher"));
        assert!(!debug.contains("worker"));
    }

    fn worker_bundle_path(bundle: &Path) -> PathBuf {
        bundle.join("Contents/Helpers").join(WORKER_BUNDLE_NAME)
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
