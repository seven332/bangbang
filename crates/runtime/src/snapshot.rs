//! Backend-neutral snapshot request and capability models.

use std::fmt;
use std::path::{Path, PathBuf};

const REDACTED: &str = "<redacted>";

/// Snapshot image kind requested by the API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotType {
    /// A complete guest-memory image.
    Full,
    /// A differential guest-memory image.
    Diff,
}

/// Guest-memory population backend requested for snapshot load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotMemoryBackendType {
    /// Populate guest memory from a file.
    File,
    /// Populate guest memory through userfaultfd.
    Uffd,
}

/// Normalized guest-memory backend for snapshot load.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotMemoryBackend {
    backend_path: PathBuf,
    backend_type: SnapshotMemoryBackendType,
}

impl SnapshotMemoryBackend {
    /// Creates a normalized memory backend.
    pub fn new(backend_path: impl Into<PathBuf>, backend_type: SnapshotMemoryBackendType) -> Self {
        Self {
            backend_path: backend_path.into(),
            backend_type,
        }
    }

    /// Returns the untrusted backend path.
    pub fn backend_path(&self) -> &Path {
        &self.backend_path
    }

    /// Returns the requested backend kind.
    pub const fn backend_type(&self) -> SnapshotMemoryBackendType {
        self.backend_type
    }
}

impl fmt::Debug for SnapshotMemoryBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotMemoryBackend")
            .field("backend_path", &REDACTED)
            .field("backend_type", &self.backend_type)
            .finish()
    }
}

/// Network backend override requested during snapshot load.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotNetworkOverride {
    iface_id: String,
    host_dev_name: String,
}

impl SnapshotNetworkOverride {
    /// Creates a network backend override.
    pub fn new(iface_id: impl Into<String>, host_dev_name: impl Into<String>) -> Self {
        Self {
            iface_id: iface_id.into(),
            host_dev_name: host_dev_name.into(),
        }
    }

    /// Returns the untrusted interface identifier.
    pub fn iface_id(&self) -> &str {
        &self.iface_id
    }

    /// Returns the untrusted host device name.
    pub fn host_dev_name(&self) -> &str {
        &self.host_dev_name
    }
}

impl fmt::Debug for SnapshotNetworkOverride {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotNetworkOverride")
            .field("iface_id", &REDACTED)
            .field("host_dev_name", &REDACTED)
            .finish()
    }
}

/// Vsock backend override requested during snapshot load.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotVsockOverride {
    uds_path: PathBuf,
}

impl SnapshotVsockOverride {
    /// Creates a vsock backend override.
    pub fn new(uds_path: impl Into<PathBuf>) -> Self {
        Self {
            uds_path: uds_path.into(),
        }
    }

    /// Returns the untrusted host socket path.
    pub fn uds_path(&self) -> &Path {
        &self.uds_path
    }
}

impl fmt::Debug for SnapshotVsockOverride {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotVsockOverride")
            .field("uds_path", &REDACTED)
            .finish()
    }
}

/// Normalized input for snapshot creation.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotCreateInput {
    snapshot_type: SnapshotType,
    snapshot_path: PathBuf,
    mem_file_path: PathBuf,
}

impl SnapshotCreateInput {
    /// Creates snapshot-creation input without accessing either path.
    pub fn new(
        snapshot_type: SnapshotType,
        snapshot_path: impl Into<PathBuf>,
        mem_file_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            snapshot_type,
            snapshot_path: snapshot_path.into(),
            mem_file_path: mem_file_path.into(),
        }
    }

    /// Returns the requested snapshot kind.
    pub const fn snapshot_type(&self) -> SnapshotType {
        self.snapshot_type
    }

    /// Returns the untrusted state-file path.
    pub fn snapshot_path(&self) -> &Path {
        &self.snapshot_path
    }

    /// Returns the untrusted memory-file path.
    pub fn mem_file_path(&self) -> &Path {
        &self.mem_file_path
    }
}

impl fmt::Debug for SnapshotCreateInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotCreateInput")
            .field("snapshot_type", &self.snapshot_type)
            .field("snapshot_path", &REDACTED)
            .field("mem_file_path", &REDACTED)
            .finish()
    }
}

/// Normalized input for snapshot load.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotLoadInput {
    snapshot_path: PathBuf,
    mem_backend: SnapshotMemoryBackend,
    track_dirty_pages: bool,
    resume_vm: bool,
    network_overrides: Vec<SnapshotNetworkOverride>,
    vsock_override: Option<SnapshotVsockOverride>,
    clock_realtime: bool,
    deprecated_fields_used: bool,
}

impl SnapshotLoadInput {
    /// Creates load input with all optional behavior disabled.
    pub fn new(snapshot_path: impl Into<PathBuf>, mem_backend: SnapshotMemoryBackend) -> Self {
        Self {
            snapshot_path: snapshot_path.into(),
            mem_backend,
            track_dirty_pages: false,
            resume_vm: false,
            network_overrides: Vec::new(),
            vsock_override: None,
            clock_realtime: false,
            deprecated_fields_used: false,
        }
    }

    /// Enables or disables dirty-page tracking for the restored VM.
    pub const fn with_track_dirty_pages(mut self, track_dirty_pages: bool) -> Self {
        self.track_dirty_pages = track_dirty_pages;
        self
    }

    /// Requests resume after a complete load.
    pub const fn with_resume_vm(mut self, resume_vm: bool) -> Self {
        self.resume_vm = resume_vm;
        self
    }

    /// Adds normalized network backend overrides.
    pub fn with_network_overrides(
        mut self,
        network_overrides: Vec<SnapshotNetworkOverride>,
    ) -> Self {
        self.network_overrides = network_overrides;
        self
    }

    /// Adds a normalized vsock backend override.
    pub fn with_vsock_override(mut self, vsock_override: SnapshotVsockOverride) -> Self {
        self.vsock_override = Some(vsock_override);
        self
    }

    /// Enables or disables realtime clock adjustment.
    pub const fn with_clock_realtime(mut self, clock_realtime: bool) -> Self {
        self.clock_realtime = clock_realtime;
        self
    }

    /// Records deprecated-field use retained from API parsing.
    pub const fn with_deprecated_fields_used(mut self, deprecated_fields_used: bool) -> Self {
        self.deprecated_fields_used = deprecated_fields_used;
        self
    }

    /// Returns the untrusted state-file path.
    pub fn snapshot_path(&self) -> &Path {
        &self.snapshot_path
    }

    /// Returns the normalized memory backend.
    pub const fn mem_backend(&self) -> &SnapshotMemoryBackend {
        &self.mem_backend
    }

    /// Returns whether dirty-page tracking was requested.
    pub const fn track_dirty_pages(&self) -> bool {
        self.track_dirty_pages
    }

    /// Returns whether the VM should resume after a future successful load.
    pub const fn resume_vm(&self) -> bool {
        self.resume_vm
    }

    /// Returns normalized network backend overrides.
    pub fn network_overrides(&self) -> &[SnapshotNetworkOverride] {
        &self.network_overrides
    }

    /// Returns the normalized vsock backend override.
    pub const fn vsock_override(&self) -> Option<&SnapshotVsockOverride> {
        self.vsock_override.as_ref()
    }

    /// Returns whether realtime clock adjustment was requested.
    pub const fn clock_realtime(&self) -> bool {
        self.clock_realtime
    }

    /// Returns whether parsing observed a deprecated request field.
    pub const fn deprecated_fields_used(&self) -> bool {
        self.deprecated_fields_used
    }
}

impl fmt::Debug for SnapshotLoadInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotLoadInput")
            .field("snapshot_path", &REDACTED)
            .field("mem_backend", &self.mem_backend)
            .field("track_dirty_pages", &self.track_dirty_pages)
            .field("resume_vm", &self.resume_vm)
            .field("network_overrides", &REDACTED)
            .field(
                "vsock_override",
                &self.vsock_override.as_ref().map(|_| REDACTED),
            )
            .field("clock_realtime", &self.clock_realtime)
            .field("deprecated_fields_used", &self.deprecated_fields_used)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnapshotV1Rejection {
    CreateSnapshotType,
    LoadMemoryBackend,
    LoadDirtyTracking,
    LoadClockRealtime,
    LoadNetworkOverrides,
    LoadVsockOverride,
    BootSource,
    MachineProfile,
    DriveProfile,
    NetworkDevice,
    VsockDevice,
    PmemDevice,
    BalloonDevice,
    MemoryHotplugDevice,
    EntropyDevice,
    MmdsState,
    SerialConfig,
    LoadProcessConfigured,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SnapshotV1VmProfile {
    pub(crate) machine_supported: bool,
    pub(crate) drive_supported: bool,
    pub(crate) network_configured: bool,
    pub(crate) vsock_configured: bool,
    pub(crate) pmem_configured: bool,
    pub(crate) balloon_configured: bool,
    pub(crate) memory_hotplug_configured: bool,
    pub(crate) entropy_configured: bool,
    pub(crate) mmds_configured: bool,
    pub(crate) serial_supported: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SnapshotV1LoadProfile {
    pub(crate) machine_is_default: bool,
    pub(crate) boot_source_configured: bool,
    pub(crate) drive_configured: bool,
    pub(crate) network_configured: bool,
    pub(crate) vsock_configured: bool,
    pub(crate) pmem_configured: bool,
    pub(crate) balloon_configured: bool,
    pub(crate) memory_hotplug_configured: bool,
    pub(crate) entropy_configured: bool,
    pub(crate) mmds_configured: bool,
    pub(crate) serial_is_default: bool,
}

#[cfg(test)]
pub(crate) fn classify_v1_create(
    input: &SnapshotCreateInput,
    profile: SnapshotV1VmProfile,
) -> Result<(), SnapshotV1Rejection> {
    classify_v1_create_request(input)?;
    classify_v1_create_profile(profile)
}

pub(crate) fn classify_v1_create_request(
    input: &SnapshotCreateInput,
) -> Result<(), SnapshotV1Rejection> {
    if input.snapshot_type != SnapshotType::Full {
        return Err(SnapshotV1Rejection::CreateSnapshotType);
    }

    Ok(())
}

pub(crate) fn classify_v1_create_profile(
    profile: SnapshotV1VmProfile,
) -> Result<(), SnapshotV1Rejection> {
    if !profile.machine_supported {
        return Err(SnapshotV1Rejection::MachineProfile);
    }
    if !profile.drive_supported {
        return Err(SnapshotV1Rejection::DriveProfile);
    }
    if profile.network_configured {
        return Err(SnapshotV1Rejection::NetworkDevice);
    }
    if profile.vsock_configured {
        return Err(SnapshotV1Rejection::VsockDevice);
    }
    if profile.pmem_configured {
        return Err(SnapshotV1Rejection::PmemDevice);
    }
    if profile.balloon_configured {
        return Err(SnapshotV1Rejection::BalloonDevice);
    }
    if profile.memory_hotplug_configured {
        return Err(SnapshotV1Rejection::MemoryHotplugDevice);
    }
    if profile.entropy_configured {
        return Err(SnapshotV1Rejection::EntropyDevice);
    }
    if profile.mmds_configured {
        return Err(SnapshotV1Rejection::MmdsState);
    }
    if !profile.serial_supported {
        return Err(SnapshotV1Rejection::SerialConfig);
    }

    Ok(())
}

#[cfg(test)]
pub(crate) fn classify_v1_load(
    input: &SnapshotLoadInput,
    snapshot_load_history_fresh: bool,
    profile: SnapshotV1LoadProfile,
) -> Result<(), SnapshotV1Rejection> {
    classify_v1_load_request(input)?;
    classify_v1_load_eligibility(snapshot_load_history_fresh, profile)
}

pub(crate) fn classify_v1_load_request(
    input: &SnapshotLoadInput,
) -> Result<(), SnapshotV1Rejection> {
    if input.mem_backend.backend_type != SnapshotMemoryBackendType::File {
        return Err(SnapshotV1Rejection::LoadMemoryBackend);
    }
    if input.track_dirty_pages {
        return Err(SnapshotV1Rejection::LoadDirtyTracking);
    }
    if input.clock_realtime {
        return Err(SnapshotV1Rejection::LoadClockRealtime);
    }
    if !input.network_overrides.is_empty() {
        return Err(SnapshotV1Rejection::LoadNetworkOverrides);
    }
    if input.vsock_override.is_some() {
        return Err(SnapshotV1Rejection::LoadVsockOverride);
    }

    Ok(())
}

pub(crate) fn classify_v1_load_eligibility(
    snapshot_load_history_fresh: bool,
    profile: SnapshotV1LoadProfile,
) -> Result<(), SnapshotV1Rejection> {
    if !snapshot_load_history_fresh {
        return Err(SnapshotV1Rejection::LoadProcessConfigured);
    }
    if !profile.machine_is_default {
        return Err(SnapshotV1Rejection::MachineProfile);
    }
    if profile.boot_source_configured {
        return Err(SnapshotV1Rejection::BootSource);
    }
    if profile.drive_configured {
        return Err(SnapshotV1Rejection::DriveProfile);
    }
    if profile.network_configured {
        return Err(SnapshotV1Rejection::NetworkDevice);
    }
    if profile.vsock_configured {
        return Err(SnapshotV1Rejection::VsockDevice);
    }
    if profile.pmem_configured {
        return Err(SnapshotV1Rejection::PmemDevice);
    }
    if profile.balloon_configured {
        return Err(SnapshotV1Rejection::BalloonDevice);
    }
    if profile.memory_hotplug_configured {
        return Err(SnapshotV1Rejection::MemoryHotplugDevice);
    }
    if profile.entropy_configured {
        return Err(SnapshotV1Rejection::EntropyDevice);
    }
    if profile.mmds_configured {
        return Err(SnapshotV1Rejection::MmdsState);
    }
    if !profile.serial_is_default {
        return Err(SnapshotV1Rejection::SerialConfig);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn supported_profile() -> SnapshotV1VmProfile {
        SnapshotV1VmProfile {
            machine_supported: true,
            drive_supported: true,
            network_configured: false,
            vsock_configured: false,
            pmem_configured: false,
            balloon_configured: false,
            memory_hotplug_configured: false,
            entropy_configured: false,
            mmds_configured: false,
            serial_supported: true,
        }
    }

    fn file_load() -> SnapshotLoadInput {
        SnapshotLoadInput::new(
            "/private/state",
            SnapshotMemoryBackend::new("/private/memory", SnapshotMemoryBackendType::File),
        )
    }

    fn clean_load_profile() -> SnapshotV1LoadProfile {
        SnapshotV1LoadProfile {
            machine_is_default: true,
            boot_source_configured: false,
            drive_configured: false,
            network_configured: false,
            vsock_configured: false,
            pmem_configured: false,
            balloon_configured: false,
            memory_hotplug_configured: false,
            entropy_configured: false,
            mmds_configured: false,
            serial_is_default: true,
        }
    }

    #[test]
    fn snapshot_inputs_retain_values_and_redact_debug() {
        let create = SnapshotCreateInput::new(
            SnapshotType::Full,
            "/private/create-state",
            "/private/create-memory",
        );
        assert_eq!(create.snapshot_type(), SnapshotType::Full);
        assert_eq!(create.snapshot_path(), Path::new("/private/create-state"));
        assert_eq!(create.mem_file_path(), Path::new("/private/create-memory"));

        let load = file_load()
            .with_track_dirty_pages(true)
            .with_resume_vm(true)
            .with_network_overrides(vec![SnapshotNetworkOverride::new(
                "private-iface",
                "private-host-device",
            )])
            .with_vsock_override(SnapshotVsockOverride::new("/private/vsock"))
            .with_clock_realtime(true)
            .with_deprecated_fields_used(true);
        assert_eq!(load.snapshot_path(), Path::new("/private/state"));
        assert_eq!(
            load.mem_backend().backend_path(),
            Path::new("/private/memory")
        );
        assert!(load.track_dirty_pages());
        assert!(load.resume_vm());
        assert_eq!(load.network_overrides()[0].iface_id(), "private-iface");
        assert_eq!(
            load.network_overrides()[0].host_dev_name(),
            "private-host-device"
        );
        assert_eq!(
            load.vsock_override().map(SnapshotVsockOverride::uds_path),
            Some(Path::new("/private/vsock"))
        );
        assert!(load.clock_realtime());
        assert!(load.deprecated_fields_used());

        let debug = format!("{create:?} {load:?}");
        for private in [
            "/private/create-state",
            "/private/create-memory",
            "/private/state",
            "/private/memory",
            "private-iface",
            "private-host-device",
            "/private/vsock",
        ] {
            assert!(
                !debug.contains(private),
                "debug leaked {private:?}: {debug}"
            );
        }
        assert!(debug.contains(REDACTED));
    }

    #[test]
    fn native_v1_create_policy_rejects_each_unsupported_dimension() {
        let full = SnapshotCreateInput::new(SnapshotType::Full, "state", "memory");
        assert_eq!(classify_v1_create(&full, supported_profile()), Ok(()));
        assert_eq!(
            classify_v1_create(
                &SnapshotCreateInput::new(SnapshotType::Diff, "state", "memory"),
                supported_profile(),
            ),
            Err(SnapshotV1Rejection::CreateSnapshotType)
        );

        let cases = [
            (
                SnapshotV1VmProfile {
                    machine_supported: false,
                    ..supported_profile()
                },
                SnapshotV1Rejection::MachineProfile,
            ),
            (
                SnapshotV1VmProfile {
                    drive_supported: false,
                    ..supported_profile()
                },
                SnapshotV1Rejection::DriveProfile,
            ),
            (
                SnapshotV1VmProfile {
                    network_configured: true,
                    ..supported_profile()
                },
                SnapshotV1Rejection::NetworkDevice,
            ),
            (
                SnapshotV1VmProfile {
                    vsock_configured: true,
                    ..supported_profile()
                },
                SnapshotV1Rejection::VsockDevice,
            ),
            (
                SnapshotV1VmProfile {
                    pmem_configured: true,
                    ..supported_profile()
                },
                SnapshotV1Rejection::PmemDevice,
            ),
            (
                SnapshotV1VmProfile {
                    balloon_configured: true,
                    ..supported_profile()
                },
                SnapshotV1Rejection::BalloonDevice,
            ),
            (
                SnapshotV1VmProfile {
                    memory_hotplug_configured: true,
                    ..supported_profile()
                },
                SnapshotV1Rejection::MemoryHotplugDevice,
            ),
            (
                SnapshotV1VmProfile {
                    entropy_configured: true,
                    ..supported_profile()
                },
                SnapshotV1Rejection::EntropyDevice,
            ),
            (
                SnapshotV1VmProfile {
                    mmds_configured: true,
                    ..supported_profile()
                },
                SnapshotV1Rejection::MmdsState,
            ),
            (
                SnapshotV1VmProfile {
                    serial_supported: false,
                    ..supported_profile()
                },
                SnapshotV1Rejection::SerialConfig,
            ),
        ];

        for (profile, expected) in cases {
            assert_eq!(classify_v1_create(&full, profile), Err(expected));
        }
    }

    #[test]
    fn native_v1_load_policy_rejects_each_unsupported_dimension() {
        assert_eq!(
            classify_v1_load(&file_load(), true, clean_load_profile()),
            Ok(())
        );
        assert_eq!(
            classify_v1_load(
                &file_load().with_resume_vm(true),
                true,
                clean_load_profile(),
            ),
            Ok(())
        );
        assert_eq!(
            classify_v1_load(
                &SnapshotLoadInput::new(
                    "state",
                    SnapshotMemoryBackend::new("memory", SnapshotMemoryBackendType::Uffd),
                ),
                true,
                clean_load_profile(),
            ),
            Err(SnapshotV1Rejection::LoadMemoryBackend)
        );
        assert_eq!(
            classify_v1_load(
                &file_load().with_track_dirty_pages(true),
                true,
                clean_load_profile(),
            ),
            Err(SnapshotV1Rejection::LoadDirtyTracking)
        );
        assert_eq!(
            classify_v1_load(
                &file_load().with_clock_realtime(true),
                true,
                clean_load_profile(),
            ),
            Err(SnapshotV1Rejection::LoadClockRealtime)
        );
        assert_eq!(
            classify_v1_load(
                &file_load()
                    .with_network_overrides(vec![SnapshotNetworkOverride::new("eth0", "tap0",)]),
                true,
                clean_load_profile(),
            ),
            Err(SnapshotV1Rejection::LoadNetworkOverrides)
        );
        assert_eq!(
            classify_v1_load(
                &file_load().with_vsock_override(SnapshotVsockOverride::new("vsock")),
                true,
                clean_load_profile(),
            ),
            Err(SnapshotV1Rejection::LoadVsockOverride)
        );
        assert_eq!(
            classify_v1_load(&file_load(), false, clean_load_profile()),
            Err(SnapshotV1Rejection::LoadProcessConfigured)
        );

        let profile_cases = [
            (
                SnapshotV1LoadProfile {
                    machine_is_default: false,
                    ..clean_load_profile()
                },
                SnapshotV1Rejection::MachineProfile,
            ),
            (
                SnapshotV1LoadProfile {
                    boot_source_configured: true,
                    ..clean_load_profile()
                },
                SnapshotV1Rejection::BootSource,
            ),
            (
                SnapshotV1LoadProfile {
                    drive_configured: true,
                    ..clean_load_profile()
                },
                SnapshotV1Rejection::DriveProfile,
            ),
            (
                SnapshotV1LoadProfile {
                    network_configured: true,
                    ..clean_load_profile()
                },
                SnapshotV1Rejection::NetworkDevice,
            ),
            (
                SnapshotV1LoadProfile {
                    vsock_configured: true,
                    ..clean_load_profile()
                },
                SnapshotV1Rejection::VsockDevice,
            ),
            (
                SnapshotV1LoadProfile {
                    pmem_configured: true,
                    ..clean_load_profile()
                },
                SnapshotV1Rejection::PmemDevice,
            ),
            (
                SnapshotV1LoadProfile {
                    balloon_configured: true,
                    ..clean_load_profile()
                },
                SnapshotV1Rejection::BalloonDevice,
            ),
            (
                SnapshotV1LoadProfile {
                    memory_hotplug_configured: true,
                    ..clean_load_profile()
                },
                SnapshotV1Rejection::MemoryHotplugDevice,
            ),
            (
                SnapshotV1LoadProfile {
                    entropy_configured: true,
                    ..clean_load_profile()
                },
                SnapshotV1Rejection::EntropyDevice,
            ),
            (
                SnapshotV1LoadProfile {
                    mmds_configured: true,
                    ..clean_load_profile()
                },
                SnapshotV1Rejection::MmdsState,
            ),
            (
                SnapshotV1LoadProfile {
                    serial_is_default: false,
                    ..clean_load_profile()
                },
                SnapshotV1Rejection::SerialConfig,
            ),
        ];
        for (profile, expected) in profile_cases {
            assert_eq!(classify_v1_load(&file_load(), true, profile), Err(expected));
        }
    }
}
