use std::ffi::{CString, OsString};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bangbang_api::HTTP_MAX_PAYLOAD_SIZE;
use bangbang_api::http::{
    ActionRequest, ActionType, ApiRequest, BootSourceRequest, BootSourceResponse,
    DriveCacheType as ApiDriveCacheType, DriveConfigRequest, DriveConfigResponse,
    DriveIoEngine as ApiDriveIoEngine, HttpResponse, LoggerConfigRequest,
    LoggerLevel as ApiLoggerLevel, MachineConfigRequest, MachineConfigResponse,
    MetricsConfigRequest, MmdsConfigRequest, MmdsConfigResponse, MmdsContentRequest,
    MmdsVersion as ApiMmdsVersion, NetworkInterfaceConfigRequest, NetworkInterfaceConfigResponse,
    RequestError, VmConfigResponse, VsockConfigRequest, VsockConfigResponse, parse_request,
    request_total_len,
};
use bangbang_runtime::block::{DriveCacheType, DriveConfig, DriveConfigInput, DriveIoEngine};
use bangbang_runtime::boot::{BootSourceConfig, BootSourceConfigInput};
use bangbang_runtime::logger::{LoggerConfigInput, LoggerLevel};
use bangbang_runtime::machine::{
    MachineConfig, MachineConfigCpuTemplate as RuntimeMachineConfigCpuTemplate,
    MachineConfigHugePages as RuntimeMachineConfigHugePages, MachineConfigInput,
};
use bangbang_runtime::metrics::MetricsConfigInput;
use bangbang_runtime::mmds::{
    MmdsConfig, MmdsConfigInput, MmdsContentInput, MmdsVersion as RuntimeMmdsVersion,
};
#[cfg(test)]
use bangbang_runtime::network::MAX_NETWORK_INTERFACE_COUNT;
use bangbang_runtime::network::{NetworkInterfaceConfig, NetworkInterfaceConfigInput};
use bangbang_runtime::vsock::{VsockConfig, VsockConfigInput};
use bangbang_runtime::{VmConfiguration, VmmAction, VmmData};

use crate::vmm::VmmRequestHandler;

const READ_CHUNK_SIZE: usize = 4096;
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);
static NEXT_TEMP_SOCKET_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ApiServerError {
    Accept(std::io::ErrorKind),
    Bind(std::io::ErrorKind),
    Connection(std::io::ErrorKind),
    SocketMetadata(std::io::ErrorKind),
    SocketPathCheck(std::io::ErrorKind),
    SocketPathChanged,
    SocketPathExists,
    SocketPathIsNotSocket,
}

impl std::fmt::Display for ApiServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Accept(kind) => write!(f, "failed to accept API connection: {kind:?}"),
            Self::Bind(kind) => write!(f, "failed to bind API socket: {kind:?}"),
            Self::Connection(kind) => write!(f, "API connection I/O failed: {kind:?}"),
            Self::SocketMetadata(kind) => {
                write!(f, "failed to inspect bound API socket: {kind:?}")
            }
            Self::SocketPathCheck(kind) => write!(f, "failed to check API socket path: {kind:?}"),
            Self::SocketPathChanged => f.write_str("API socket path changed during bind"),
            Self::SocketPathExists => f.write_str("API socket path already exists"),
            Self::SocketPathIsNotSocket => f.write_str("bound API path is not a socket"),
        }
    }
}

impl std::error::Error for ApiServerError {}

#[derive(Debug)]
pub(crate) struct ApiServer {
    listener: UnixListener,
    _socket_guard: SocketGuard,
}

impl ApiServer {
    pub(crate) fn bind(path: impl AsRef<Path>) -> Result<Self, ApiServerError> {
        let path = path.as_ref();

        if path_exists_without_following_links(path)? {
            return Err(ApiServerError::SocketPathExists);
        }

        let (listener, metadata) = bind_unpublished_socket(path)?;
        publish_socket_path(&metadata.path, path).inspect_err(|_| {
            remove_socket_path_if_owned(&metadata.path, metadata.dev, metadata.ino);
        })?;
        let socket_guard = SocketGuard::new(path, metadata);
        ensure_socket_path_owner(path, socket_guard.dev, socket_guard.ino)?;

        Ok(Self {
            listener,
            _socket_guard: socket_guard,
        })
    }

    pub(crate) fn run_until(
        &self,
        vmm: &mut impl VmmRequestHandler,
        shutdown_wakeup: &mut UnixStream,
    ) -> Result<(), ApiServerError> {
        self.listener
            .set_nonblocking(true)
            .map_err(|err| ApiServerError::Accept(err.kind()))?;
        shutdown_wakeup
            .set_nonblocking(true)
            .map_err(|err| ApiServerError::Connection(err.kind()))?;

        loop {
            wait_for_listener_or_shutdown(&self.listener, shutdown_wakeup)?;
            if drain_shutdown_wakeup(shutdown_wakeup)? {
                return Ok(());
            }

            match self.serve_next(vmm) {
                Ok(()) => {}
                Err(ApiServerError::Accept(kind)) if is_transient_accept_error(kind) => {}
                Err(err) => return Err(err),
            }
        }
    }

    fn serve_next(&self, vmm: &mut impl VmmRequestHandler) -> Result<(), ApiServerError> {
        let (mut stream, _) = self
            .listener
            .accept()
            .map_err(|err| ApiServerError::Accept(err.kind()))?;
        stream
            .set_nonblocking(false)
            .map_err(|err| ApiServerError::Connection(err.kind()))?;

        let _ = handle_connection(&mut stream, vmm);

        Ok(())
    }
}

fn is_transient_accept_error(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::Interrupted
            | std::io::ErrorKind::ConnectionAborted
    )
}

fn wait_for_listener_or_shutdown(
    listener: &UnixListener,
    shutdown_wakeup: &UnixStream,
) -> Result<(), ApiServerError> {
    let mut poll_fds = [
        libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: shutdown_wakeup.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];

    loop {
        for poll_fd in &mut poll_fds {
            poll_fd.revents = 0;
        }

        // SAFETY: `poll_fds` points to two initialized `pollfd` values and
        // remains valid for the duration of the call. The timeout is infinite.
        let result = unsafe { libc::poll(poll_fds.as_mut_ptr(), poll_fds.len() as _, -1) };
        if result > 0 {
            return Ok(());
        }

        let kind = std::io::Error::last_os_error().kind();
        if kind != std::io::ErrorKind::Interrupted {
            return Err(ApiServerError::Accept(kind));
        }
    }
}

fn drain_shutdown_wakeup(shutdown_wakeup: &mut UnixStream) -> Result<bool, ApiServerError> {
    let mut drained = false;
    let mut buffer = [0; 64];

    loop {
        match shutdown_wakeup.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(_) => drained = true,
            Err(err) if matches!(err.kind(), std::io::ErrorKind::WouldBlock) => {
                return Ok(drained);
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(ApiServerError::Connection(err.kind())),
        }
    }
}

#[derive(Debug)]
struct BoundSocketMetadata {
    path: PathBuf,
    dev: u64,
    ino: u64,
}

#[derive(Debug)]
struct SocketGuard {
    path: PathBuf,
    dev: u64,
    ino: u64,
}

impl SocketGuard {
    fn new(path: &Path, metadata: BoundSocketMetadata) -> Self {
        Self {
            path: path.to_path_buf(),
            dev: metadata.dev,
            ino: metadata.ino,
        }
    }

    fn owns_current_path(&self) -> bool {
        socket_path_is_owned(&self.path, self.dev, self.ino).unwrap_or(false)
    }
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        if self.owns_current_path() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn socket_path_metadata(path: &Path) -> Result<fs::Metadata, ApiServerError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|err| ApiServerError::SocketMetadata(err.kind()))?;

    if !metadata.file_type().is_socket() {
        return Err(ApiServerError::SocketPathIsNotSocket);
    }

    Ok(metadata)
}

fn bind_unpublished_socket(
    path: &Path,
) -> Result<(UnixListener, BoundSocketMetadata), ApiServerError> {
    for _ in 0..16 {
        let temp_path = next_temporary_socket_path(path);
        let listener = match UnixListener::bind(&temp_path) {
            Ok(listener) => listener,
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::AddrInUse | std::io::ErrorKind::AlreadyExists
                ) =>
            {
                continue;
            }
            Err(err) => return Err(ApiServerError::Bind(err.kind())),
        };
        let metadata = socket_path_metadata(&temp_path)?;

        return Ok((
            listener,
            BoundSocketMetadata {
                path: temp_path,
                dev: metadata.dev(),
                ino: metadata.ino(),
            },
        ));
    }

    Err(ApiServerError::Bind(std::io::ErrorKind::AlreadyExists))
}

fn next_temporary_socket_path(path: &Path) -> PathBuf {
    next_temporary_socket_path_from(path, &NEXT_TEMP_SOCKET_ID)
}

fn next_temporary_socket_path_from(path: &Path, next_id: &AtomicU64) -> PathBuf {
    loop {
        let id = next_id.fetch_add(1, Ordering::Relaxed);
        let temp_path = temporary_socket_path(path, id);
        if temp_path != path {
            return temp_path;
        }
    }
}

fn temporary_socket_path(path: &Path, id: u64) -> PathBuf {
    let mut temp_name = OsString::from(".bb.");
    temp_name.push(format!("{}.{}", std::process::id(), id));

    path.with_file_name(temp_name)
}

fn ensure_socket_path_owner(path: &Path, dev: u64, ino: u64) -> Result<(), ApiServerError> {
    if socket_path_is_owned(path, dev, ino)? {
        Ok(())
    } else {
        Err(ApiServerError::SocketPathChanged)
    }
}

fn remove_socket_path_if_owned(path: &Path, dev: u64, ino: u64) {
    if socket_path_is_owned(path, dev, ino).unwrap_or(false) {
        let _ = fs::remove_file(path);
    }
}

fn socket_path_is_owned(path: &Path, dev: u64, ino: u64) -> Result<bool, ApiServerError> {
    let metadata = socket_path_metadata(path)?;

    Ok(metadata.dev() == dev && metadata.ino() == ino)
}

#[cfg(target_os = "macos")]
fn publish_socket_path(from: &Path, to: &Path) -> Result<(), ApiServerError> {
    use std::os::raw::{c_char, c_int, c_uint};

    const RENAME_EXCL: c_uint = 0x0000_0004;

    unsafe extern "C" {
        fn renamex_np(from: *const c_char, to: *const c_char, flags: c_uint) -> c_int;
    }

    let from = path_to_cstring(from)?;
    let to = path_to_cstring(to)?;
    // SAFETY: both pointers come from live `CString` values and are valid
    // NUL-terminated paths for the duration of this call.
    let result = unsafe { renamex_np(from.as_ptr(), to.as_ptr(), RENAME_EXCL) };
    if result == 0 {
        return Ok(());
    }

    let kind = std::io::Error::last_os_error().kind();
    if kind == std::io::ErrorKind::AlreadyExists {
        Err(ApiServerError::SocketPathExists)
    } else {
        Err(ApiServerError::Bind(kind))
    }
}

#[cfg(not(target_os = "macos"))]
fn publish_socket_path(from: &Path, to: &Path) -> Result<(), ApiServerError> {
    if path_exists_without_following_links(to)? {
        return Err(ApiServerError::SocketPathExists);
    }

    fs::rename(from, to).map_err(|err| ApiServerError::Bind(err.kind()))
}

fn path_to_cstring(path: &Path) -> Result<CString, ApiServerError> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| ApiServerError::Bind(std::io::ErrorKind::InvalidInput))
}

fn path_exists_without_following_links(path: &Path) -> Result<bool, ApiServerError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(ApiServerError::SocketPathCheck(err.kind())),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum RequestRead {
    Complete(Vec<u8>),
    TooLarge,
}

fn handle_connection(
    stream: &mut UnixStream,
    vmm: &mut impl VmmRequestHandler,
) -> Result<(), ApiServerError> {
    stream
        .set_write_timeout(Some(CONNECTION_TIMEOUT))
        .map_err(|err| ApiServerError::Connection(err.kind()))?;

    let response = match read_request(stream, CONNECTION_TIMEOUT)? {
        RequestRead::Complete(request) => handle_request_bytes(&request, vmm),
        RequestRead::TooLarge => HttpResponse::fault(RequestError::PayloadTooLarge.fault_message()),
    };

    stream
        .write_all(&response.to_http_bytes())
        .map_err(|err| ApiServerError::Connection(err.kind()))
}

fn handle_request_bytes(bytes: &[u8], vmm: &mut impl VmmRequestHandler) -> HttpResponse {
    match parse_request(bytes) {
        Ok(request) => handle_api_request(request, vmm),
        Err(err) => HttpResponse::fault(err.fault_message()),
    }
}

fn handle_api_request(request: ApiRequest, vmm: &mut impl VmmRequestHandler) -> HttpResponse {
    match request {
        ApiRequest::GetInstanceInfo => {
            handle_instance_info(vmm.handle_action(VmmAction::GetVmInstanceInfo))
        }
        ApiRequest::GetVersion => handle_vmm_version(vmm.handle_action(VmmAction::GetVmmVersion)),
        ApiRequest::GetMachineConfig => {
            handle_machine_config(vmm.handle_action(VmmAction::GetMachineConfig))
        }
        ApiRequest::GetMmds => handle_mmds(vmm.handle_action(VmmAction::GetMmds)),
        ApiRequest::GetVmConfig => handle_vm_config(vmm.handle_action(VmmAction::GetVmConfig)),
        ApiRequest::PutAction(action) => {
            handle_empty(vmm.handle_action(action_from_request(action.as_ref())))
        }
        ApiRequest::PutBootSource(config) => handle_empty(vmm.handle_action(
            VmmAction::PutBootSource(boot_source_input_from_request(config.as_ref())),
        )),
        ApiRequest::PutLogger(config) => handle_empty(vmm.handle_action(VmmAction::PutLogger(
            logger_config_input_from_request(config.as_ref()),
        ))),
        ApiRequest::PutMachineConfig(config) => handle_empty(vmm.handle_action(
            VmmAction::PutMachineConfig(machine_config_input_from_request(config.as_ref())),
        )),
        ApiRequest::PutMetrics(config) => handle_empty(vmm.handle_action(VmmAction::PutMetrics(
            metrics_config_input_from_request(config.as_ref()),
        ))),
        ApiRequest::PutMmds(content) => handle_empty(vmm.handle_action(VmmAction::PutMmds(
            mmds_content_input_from_request(content.as_ref()),
        ))),
        ApiRequest::PatchMmds(content) => handle_empty(vmm.handle_action(VmmAction::PatchMmds(
            mmds_content_input_from_request(content.as_ref()),
        ))),
        ApiRequest::PutMmdsConfig(config) => handle_empty(vmm.handle_action(
            VmmAction::PutMmdsConfig(mmds_config_input_from_request(config.as_ref())),
        )),
        ApiRequest::PutDrive(config) => handle_empty(vmm.handle_action(VmmAction::PutDrive(
            drive_config_input_from_request(config.as_ref()),
        ))),
        ApiRequest::PutNetworkInterface(config) => {
            handle_empty(vmm.handle_action(VmmAction::PutNetworkInterface(
                network_interface_config_input_from_request(config.as_ref()),
            )))
        }
        ApiRequest::PutVsock(config) => handle_empty(vmm.handle_action(VmmAction::PutVsock(
            vsock_config_input_from_request(config.as_ref()),
        ))),
    }
}

fn action_from_request(action: &ActionRequest) -> VmmAction {
    match action.action_type() {
        ActionType::InstanceStart => VmmAction::InstanceStart,
        ActionType::FlushMetrics => VmmAction::FlushMetrics,
    }
}

fn boot_source_input_from_request(config: &BootSourceRequest) -> BootSourceConfigInput {
    let mut input = BootSourceConfigInput::new(config.kernel_image_path());

    if let Some(initrd_path) = config.initrd_path() {
        input = input.with_initrd_path(initrd_path);
    }
    if let Some(boot_args) = config.boot_args() {
        input = input.with_boot_args(boot_args);
    }

    input
}

fn handle_vmm_version(result: Result<VmmData, bangbang_runtime::VmmActionError>) -> HttpResponse {
    match result {
        Ok(VmmData::VmmVersion(version)) => HttpResponse::version(&version),
        Ok(
            VmmData::Empty
            | VmmData::InstanceInformation(_)
            | VmmData::MachineConfiguration(_)
            | VmmData::MmdsValue(_)
            | VmmData::VmConfiguration(_),
        ) => HttpResponse::fault("version request returned unexpected VMM data."),
        Err(err) => HttpResponse::fault(&err.to_string()),
    }
}

fn handle_instance_info(result: Result<VmmData, bangbang_runtime::VmmActionError>) -> HttpResponse {
    match result {
        Ok(VmmData::InstanceInformation(info)) => {
            let state = info.state.to_string();
            HttpResponse::instance_info(&info.id, &state, &info.vmm_version, &info.app_name)
        }
        Ok(
            VmmData::Empty
            | VmmData::VmmVersion(_)
            | VmmData::MachineConfiguration(_)
            | VmmData::MmdsValue(_)
            | VmmData::VmConfiguration(_),
        ) => HttpResponse::fault("instance info request returned unexpected VMM data."),
        Err(err) => HttpResponse::fault(&err.to_string()),
    }
}

fn handle_machine_config(
    result: Result<VmmData, bangbang_runtime::VmmActionError>,
) -> HttpResponse {
    match result {
        Ok(VmmData::MachineConfiguration(config)) => HttpResponse::machine_config(
            config.vcpu_count(),
            config.mem_size_mib(),
            config.smt(),
            config.track_dirty_pages(),
            machine_config_huge_pages_name(config.huge_pages()),
        ),
        Ok(
            VmmData::Empty
            | VmmData::VmmVersion(_)
            | VmmData::InstanceInformation(_)
            | VmmData::MmdsValue(_)
            | VmmData::VmConfiguration(_),
        ) => HttpResponse::fault("machine config request returned unexpected VMM data."),
        Err(err) => HttpResponse::fault(&err.to_string()),
    }
}

fn handle_vm_config(result: Result<VmmData, bangbang_runtime::VmmActionError>) -> HttpResponse {
    match result {
        Ok(VmmData::VmConfiguration(config)) => {
            HttpResponse::vm_config(&vm_config_response_from_runtime(&config))
        }
        Ok(
            VmmData::Empty
            | VmmData::VmmVersion(_)
            | VmmData::InstanceInformation(_)
            | VmmData::MachineConfiguration(_)
            | VmmData::MmdsValue(_),
        ) => HttpResponse::fault("VM config request returned unexpected VMM data."),
        Err(err) => HttpResponse::fault(&err.to_string()),
    }
}

fn handle_mmds(result: Result<VmmData, bangbang_runtime::VmmActionError>) -> HttpResponse {
    match result {
        Ok(VmmData::MmdsValue(value)) => HttpResponse::mmds(&value),
        Ok(
            VmmData::Empty
            | VmmData::VmmVersion(_)
            | VmmData::InstanceInformation(_)
            | VmmData::MachineConfiguration(_)
            | VmmData::VmConfiguration(_),
        ) => HttpResponse::fault("MMDS request returned unexpected VMM data."),
        Err(err) => HttpResponse::fault(&err.to_string()),
    }
}

fn handle_empty(result: Result<VmmData, bangbang_runtime::VmmActionError>) -> HttpResponse {
    match result {
        Ok(VmmData::Empty) => HttpResponse::no_content(),
        Ok(
            VmmData::InstanceInformation(_)
            | VmmData::VmmVersion(_)
            | VmmData::MachineConfiguration(_)
            | VmmData::MmdsValue(_)
            | VmmData::VmConfiguration(_),
        ) => HttpResponse::fault("no-content request returned unexpected VMM data."),
        Err(err) => HttpResponse::fault(&err.to_string()),
    }
}

fn vm_config_response_from_runtime(config: &VmConfiguration) -> VmConfigResponse {
    VmConfigResponse::new(
        machine_config_response_from_runtime(config.machine_config()),
        config
            .boot_source_config()
            .map(boot_source_response_from_runtime),
        config
            .drive_configs()
            .iter()
            .map(drive_config_response_from_runtime)
            .collect(),
        config
            .network_interface_configs()
            .iter()
            .map(network_interface_config_response_from_runtime)
            .collect(),
        config.mmds_config().map(mmds_config_response_from_runtime),
        config
            .vsock_config()
            .map(vsock_config_response_from_runtime),
    )
}

fn machine_config_response_from_runtime(config: MachineConfig) -> MachineConfigResponse {
    MachineConfigResponse::new(
        config.vcpu_count(),
        config.mem_size_mib(),
        config.smt(),
        config.track_dirty_pages(),
        machine_config_huge_pages_name(config.huge_pages()),
    )
}

fn boot_source_response_from_runtime(config: &BootSourceConfig) -> BootSourceResponse {
    let mut response = BootSourceResponse::new(path_text(config.kernel_image_path()));
    if let Some(initrd_path) = config.initrd_path() {
        response = response.with_initrd_path(path_text(initrd_path));
    }
    if let Some(boot_args) = config.boot_args() {
        response = response.with_boot_args(boot_args);
    }

    response
}

fn drive_config_response_from_runtime(config: &DriveConfig) -> DriveConfigResponse {
    let mut response = DriveConfigResponse::new(
        config.drive_id(),
        path_text(config.path_on_host()),
        config.is_root_device(),
        config.is_read_only(),
        config.cache_type().to_string(),
        config.io_engine().to_string(),
    );
    if let Some(partuuid) = config.partuuid() {
        response = response.with_partuuid(partuuid);
    }

    response
}

fn network_interface_config_response_from_runtime(
    config: &NetworkInterfaceConfig,
) -> NetworkInterfaceConfigResponse {
    let mut response =
        NetworkInterfaceConfigResponse::new(config.iface_id(), config.host_dev_name());
    if let Some(guest_mac) = config.guest_mac() {
        response = response.with_guest_mac(guest_mac.to_string());
    }

    response
}

fn mmds_config_response_from_runtime(config: &MmdsConfig) -> MmdsConfigResponse {
    let mut response = MmdsConfigResponse::new(
        config.network_interfaces().to_vec(),
        mmds_version_name(config.version()),
        config.imds_compat(),
    );
    if let Some(ipv4_address) = config.ipv4_address() {
        response = response.with_ipv4_address(ipv4_address.to_string());
    }

    response
}

fn vsock_config_response_from_runtime(config: &VsockConfig) -> VsockConfigResponse {
    VsockConfigResponse::new(config.guest_cid(), path_text(config.uds_path()))
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn machine_config_input_from_request(config: &MachineConfigRequest) -> MachineConfigInput {
    let mut input = MachineConfigInput::new(config.vcpu_count(), config.mem_size_mib())
        .with_smt(config.smt())
        .with_track_dirty_pages(config.track_dirty_pages())
        .with_huge_pages(match config.huge_pages() {
            bangbang_api::http::MachineConfigHugePages::None => RuntimeMachineConfigHugePages::None,
            bangbang_api::http::MachineConfigHugePages::TwoM => RuntimeMachineConfigHugePages::TwoM,
        });

    if let Some(cpu_template) = config.cpu_template() {
        input = input.with_cpu_template(match cpu_template {
            bangbang_api::http::MachineConfigCpuTemplate::None => {
                RuntimeMachineConfigCpuTemplate::None
            }
        });
    }

    input
}

fn metrics_config_input_from_request(config: &MetricsConfigRequest) -> MetricsConfigInput {
    MetricsConfigInput::new(config.metrics_path())
}

fn mmds_content_input_from_request(content: &MmdsContentRequest) -> MmdsContentInput {
    MmdsContentInput::new(content.value().clone())
}

fn mmds_config_input_from_request(config: &MmdsConfigRequest) -> MmdsConfigInput {
    let mut input = MmdsConfigInput::new(config.network_interfaces().to_vec())
        .with_version(match config.version() {
            ApiMmdsVersion::V1 => RuntimeMmdsVersion::V1,
            ApiMmdsVersion::V2 => RuntimeMmdsVersion::V2,
        })
        .with_imds_compat(config.imds_compat());

    if let Some(ipv4_address) = config.ipv4_address() {
        input = input.with_ipv4_address(ipv4_address);
    }

    input
}

fn logger_config_input_from_request(config: &LoggerConfigRequest) -> LoggerConfigInput {
    let mut input = LoggerConfigInput::new();

    if let Some(log_path) = config.log_path() {
        input = input.with_log_path(log_path);
    }
    if let Some(level) = config.level() {
        input = input.with_level(match level {
            ApiLoggerLevel::Off => LoggerLevel::Off,
            ApiLoggerLevel::Trace => LoggerLevel::Trace,
            ApiLoggerLevel::Debug => LoggerLevel::Debug,
            ApiLoggerLevel::Info => LoggerLevel::Info,
            ApiLoggerLevel::Warn => LoggerLevel::Warn,
            ApiLoggerLevel::Error => LoggerLevel::Error,
        });
    }
    if let Some(show_level) = config.show_level() {
        input = input.with_show_level(show_level);
    }
    if let Some(show_log_origin) = config.show_log_origin() {
        input = input.with_show_log_origin(show_log_origin);
    }
    if let Some(module) = config.module() {
        input = input.with_module(module);
    }

    input
}

fn machine_config_huge_pages_name(huge_pages: RuntimeMachineConfigHugePages) -> &'static str {
    match huge_pages {
        RuntimeMachineConfigHugePages::None => "None",
        RuntimeMachineConfigHugePages::TwoM => "2M",
    }
}

fn mmds_version_name(version: RuntimeMmdsVersion) -> &'static str {
    match version {
        RuntimeMmdsVersion::V1 => "V1",
        RuntimeMmdsVersion::V2 => "V2",
    }
}

fn drive_config_input_from_request(config: &DriveConfigRequest) -> DriveConfigInput {
    let mut input = DriveConfigInput::new(
        config.path_drive_id(),
        config.body_drive_id(),
        config.path_on_host(),
        config.is_root_device(),
    );

    if let Some(is_read_only) = config.is_read_only() {
        input = input.with_is_read_only(is_read_only);
    }
    if let Some(partuuid) = config.partuuid() {
        input = input.with_partuuid(partuuid);
    }
    if let Some(cache_type) = config.cache_type() {
        input = input.with_cache_type(match cache_type {
            ApiDriveCacheType::Unsafe => DriveCacheType::Unsafe,
            ApiDriveCacheType::Writeback => DriveCacheType::Writeback,
        });
    }
    if let Some(io_engine) = config.io_engine() {
        input = input.with_io_engine(match io_engine {
            ApiDriveIoEngine::Sync => DriveIoEngine::Sync,
            ApiDriveIoEngine::Async => DriveIoEngine::Async,
        });
    }
    if config.rate_limiter_configured() {
        input = input.with_rate_limiter_configured();
    }
    if let Some(socket) = config.socket() {
        input = input.with_socket(socket);
    }

    input
}

fn network_interface_config_input_from_request(
    config: &NetworkInterfaceConfigRequest,
) -> NetworkInterfaceConfigInput {
    let mut input = NetworkInterfaceConfigInput::new(
        config.path_iface_id(),
        config.body_iface_id(),
        config.host_dev_name(),
    );

    if let Some(guest_mac) = config.guest_mac() {
        input = input.with_guest_mac(guest_mac);
    }
    if config.mtu_configured() {
        input = input.with_mtu_configured();
    }
    if config.rx_rate_limiter_configured() {
        input = input.with_rx_rate_limiter_configured();
    }
    if config.tx_rate_limiter_configured() {
        input = input.with_tx_rate_limiter_configured();
    }

    input
}

fn vsock_config_input_from_request(config: &VsockConfigRequest) -> VsockConfigInput {
    let mut input = VsockConfigInput::new(config.guest_cid(), config.uds_path());
    if let Some(vsock_id) = config.vsock_id() {
        input = input.with_vsock_id(vsock_id);
    }

    input
}

fn read_request(stream: &mut UnixStream, timeout: Duration) -> Result<RequestRead, ApiServerError> {
    let deadline = Instant::now() + timeout;
    let mut now = Instant::now;

    read_request_until(stream, deadline, &mut now)
}

fn read_request_until(
    stream: &mut UnixStream,
    deadline: Instant,
    now: &mut impl FnMut() -> Instant,
) -> Result<RequestRead, ApiServerError> {
    let mut request = Vec::new();
    let mut chunk = [0; READ_CHUNK_SIZE];

    loop {
        match request_total_len(&request) {
            Ok(Some(total_len)) if request.len() >= total_len => {
                request.truncate(total_len);
                return Ok(RequestRead::Complete(request));
            }
            Ok(Some(_)) | Ok(None) => {}
            Err(RequestError::PayloadTooLarge) => return Ok(RequestRead::TooLarge),
            Err(_) => return Ok(RequestRead::Complete(request)),
        }

        let remaining = HTTP_MAX_PAYLOAD_SIZE.saturating_sub(request.len());
        if remaining == 0 {
            return Ok(RequestRead::TooLarge);
        }

        let read_len = chunk.len().min(remaining);
        let Some(read_timeout) = deadline.checked_duration_since(now()) else {
            return Ok(RequestRead::Complete(request));
        };
        if read_timeout.is_zero() {
            return Ok(RequestRead::Complete(request));
        }
        stream
            .set_read_timeout(Some(read_timeout))
            .map_err(|err| ApiServerError::Connection(err.kind()))?;

        let read_buffer = chunk
            .get_mut(..read_len)
            .ok_or(ApiServerError::Connection(std::io::ErrorKind::InvalidInput))?;
        let bytes_read = match stream.read(read_buffer) {
            Ok(bytes_read) => bytes_read,
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                return Ok(RequestRead::Complete(request));
            }
            Err(err) => return Err(ApiServerError::Connection(err.kind())),
        };

        if bytes_read == 0 {
            return Ok(RequestRead::Complete(request));
        }

        let bytes = chunk
            .get(..bytes_read)
            .ok_or(ApiServerError::Connection(std::io::ErrorKind::InvalidInput))?;
        request.extend_from_slice(bytes);
    }
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use bangbang_runtime::BackendError;

    use crate::vmm::{InstanceStartExecutor, ProcessVmm};

    use super::*;

    const VERSION: &str = "0.1.0";

    #[derive(Debug, Clone)]
    struct TestInstanceStarter {
        result: Result<(), BackendError>,
    }

    impl TestInstanceStarter {
        const fn success() -> Self {
            Self { result: Ok(()) }
        }

        const fn failure() -> Self {
            Self {
                result: Err(BackendError::InvalidState("test startup failed")),
            }
        }
    }

    impl InstanceStartExecutor for TestInstanceStarter {
        type Session = ();

        fn start(
            &mut self,
            _controller: &bangbang_runtime::VmmController,
        ) -> Result<Self::Session, BackendError> {
            self.result.clone()
        }
    }

    fn test_controller() -> ProcessVmm<TestInstanceStarter> {
        test_controller_with_starter(TestInstanceStarter::failure())
    }

    fn test_controller_with_starter(
        starter: TestInstanceStarter,
    ) -> ProcessVmm<TestInstanceStarter> {
        ProcessVmm::with_starter("demo-1", VERSION, "bangbang", starter)
    }

    fn test_controller_with_id_and_version(
        id: &str,
        version: &str,
    ) -> ProcessVmm<TestInstanceStarter> {
        ProcessVmm::with_starter(id, version, "bangbang", TestInstanceStarter::failure())
    }

    fn unique_socket_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        env::temp_dir().join(format!("bb-{name}-{}-{nanos}.sock", std::process::id()))
    }

    fn put_action_over_socket(
        vmm: &mut impl VmmRequestHandler,
        socket_name: &str,
        action_type: &str,
    ) -> String {
        let path = unique_socket_path(socket_name);
        let server = ApiServer::bind(&path).expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");
        let body = format!(r#"{{"action_type":"{action_type}"}}"#);
        let request = format!(
            "PUT /actions HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        client
            .write_all(request.as_bytes())
            .expect("client should write request");
        server
            .serve_next(vmm)
            .expect("server should handle one request");

        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("client should read response");
        response
    }

    fn unique_temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        let path = PathBuf::from("/tmp").join(format!("bb-{name}-{}-{nanos}", std::process::id()));
        fs::create_dir(&path).expect("fixture directory should be created");
        path
    }

    fn temporary_socket_entries(dir: &Path) -> Vec<PathBuf> {
        let prefix = format!(".bb.{}.", std::process::id());
        let mut paths = fs::read_dir(dir)
            .expect("fixture directory should be readable")
            .filter_map(|entry| {
                let entry = entry.expect("fixture directory entry should be readable");
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with(&prefix).then(|| entry.path())
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths
    }

    #[test]
    fn temporary_socket_path_skips_requested_path_collision() {
        let id = 7;
        let path = PathBuf::from("/tmp").join(format!(".bb.{}.{}", std::process::id(), id));
        let next_id = AtomicU64::new(id);

        let temp_path = next_temporary_socket_path_from(&path, &next_id);

        assert_ne!(temp_path, path);
        assert_eq!(
            temp_path,
            PathBuf::from("/tmp").join(format!(".bb.{}.{}", std::process::id(), id + 1))
        );
        assert_eq!(next_id.load(Ordering::Relaxed), id + 2);
    }

    #[test]
    fn classifies_transient_accept_errors() {
        assert!(is_transient_accept_error(std::io::ErrorKind::WouldBlock));
        assert!(is_transient_accept_error(std::io::ErrorKind::Interrupted));
        assert!(is_transient_accept_error(
            std::io::ErrorKind::ConnectionAborted
        ));
        assert!(!is_transient_accept_error(
            std::io::ErrorKind::PermissionDenied
        ));
    }

    #[test]
    fn dispatches_version_request_through_vmm_controller() {
        let mut vmm = test_controller_with_id_and_version("demo-1", "9.9.9");

        let response = handle_request_bytes(
            b"GET /version HTTP/1.1\r\nHost: localhost\r\n\r\n",
            &mut vmm,
        );

        assert_eq!(response.status(), bangbang_api::http::StatusCode::Ok);
        assert_eq!(response.body(), r#"{"firecracker_version":"9.9.9"}"#);
    }

    #[test]
    fn dispatches_instance_info_request_through_vmm_controller() {
        let mut vmm = test_controller_with_id_and_version("demo-9", "9.9.9");

        let response = handle_request_bytes(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n", &mut vmm);

        assert_eq!(response.status(), bangbang_api::http::StatusCode::Ok);
        assert!(response.body().contains(r#""id":"demo-9""#));
        assert!(response.body().contains(r#""state":"Not started""#));
        assert!(response.body().contains(r#""vmm_version":"9.9.9""#));
        assert!(response.body().contains(r#""app_name":"bangbang""#));
    }

    #[test]
    fn dispatches_machine_config_requests_through_vmm_controller() {
        let mut vmm = test_controller();

        let get_response = handle_request_bytes(
            b"GET /machine-config HTTP/1.1\r\nHost: localhost\r\n\r\n",
            &mut vmm,
        );

        assert_eq!(get_response.status(), bangbang_api::http::StatusCode::Ok);
        assert!(get_response.body().contains(r#""vcpu_count":1"#));
        assert!(get_response.body().contains(r#""mem_size_mib":128"#));
        assert!(get_response.body().contains(r#""smt":false"#));
        assert!(get_response.body().contains(r#""track_dirty_pages":false"#));
        assert!(get_response.body().contains(r#""huge_pages":"None""#));
        assert!(vmm.drive_configs().is_empty());

        let body = r#"{"vcpu_count":2,"mem_size_mib":256}"#;
        let request = format!(
            "PUT /machine-config HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        let put_response = handle_request_bytes(request.as_bytes(), &mut vmm);

        assert_eq!(
            put_response.status(),
            bangbang_api::http::StatusCode::NoContent
        );
        assert_eq!(put_response.body(), "");
        assert_eq!(vmm.machine_config().vcpu_count(), 2);
        assert_eq!(vmm.machine_config().mem_size_mib(), 256);
        assert!(vmm.drive_configs().is_empty());

        let get_response = handle_request_bytes(
            b"GET /machine-config HTTP/1.1\r\nHost: localhost\r\n\r\n",
            &mut vmm,
        );

        assert!(get_response.body().contains(r#""vcpu_count":2"#));
        assert!(get_response.body().contains(r#""mem_size_mib":256"#));
    }

    #[test]
    fn dispatches_vm_config_request_through_vmm_controller() {
        let mut vmm = test_controller();

        let default_response = handle_request_bytes(
            b"GET /vm/config HTTP/1.1\r\nHost: localhost\r\n\r\n",
            &mut vmm,
        );

        assert_eq!(
            default_response.status(),
            bangbang_api::http::StatusCode::Ok
        );
        assert!(default_response.body().contains(r#""drives":[]"#));
        assert!(default_response.body().contains(r#""machine-config":"#));
        assert!(
            default_response
                .body()
                .contains(r#""network-interfaces":[]"#)
        );
        assert!(default_response.body().contains(r#""vcpu_count":1"#));
        assert!(!default_response.body().contains(r#""boot-source":"#));
        assert!(!default_response.body().contains(r#""mmds-config":"#));
        assert!(!default_response.body().contains(r#""vsock":"#));
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::NotStarted
        );

        let machine_body = r#"{"vcpu_count":2,"mem_size_mib":256}"#;
        let machine_request = format!(
            "PUT /machine-config HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{machine_body}",
            machine_body.len()
        );
        assert_eq!(
            handle_request_bytes(machine_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let boot_body = r#"{
            "kernel_image_path": "/tmp/vmlinux",
            "initrd_path": "/tmp/initrd.img",
            "boot_args": "console=hvc0 reboot=k panic=1"
        }"#;
        let boot_request = format!(
            "PUT /boot-source HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{boot_body}",
            boot_body.len()
        );
        assert_eq!(
            handle_request_bytes(boot_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let drive_body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/rootfs.ext4",
            "is_root_device": true,
            "is_read_only": true,
            "partuuid": "0eaa91a0-01"
        }"#;
        let drive_request = format!(
            "PUT /drives/rootfs HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{drive_body}",
            drive_body.len()
        );
        assert_eq!(
            handle_request_bytes(drive_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let network_body = r#"{
            "iface_id": "eth0",
            "host_dev_name": "tap0",
            "guest_mac": "12:34:56:78:9a:bc"
        }"#;
        let network_request = format!(
            "PUT /network-interfaces/eth0 HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{network_body}",
            network_body.len()
        );
        assert_eq!(
            handle_request_bytes(network_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let mmds_config_body = r#"{"network_interfaces":["eth0"],"version":"V2","ipv4_address":"169.254.169.254","imds_compat":true}"#;
        let mmds_config_request = format!(
            "PUT /mmds/config HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{mmds_config_body}",
            mmds_config_body.len()
        );
        assert_eq!(
            handle_request_bytes(mmds_config_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let vsock_body = r#"{
            "vsock_id": "vsock0",
            "guest_cid": 3,
            "uds_path": "./v.sock"
        }"#;
        let vsock_request = format!(
            "PUT /vsock HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{vsock_body}",
            vsock_body.len()
        );
        assert_eq!(
            handle_request_bytes(vsock_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let response = handle_request_bytes(
            b"GET /vm/config HTTP/1.1\r\nHost: localhost\r\n\r\n",
            &mut vmm,
        );

        assert_eq!(response.status(), bangbang_api::http::StatusCode::Ok);
        assert!(response.body().contains(r#""boot-source":"#));
        assert!(
            response
                .body()
                .contains(r#""kernel_image_path":"/tmp/vmlinux""#)
        );
        assert!(
            response
                .body()
                .contains(r#""initrd_path":"/tmp/initrd.img""#)
        );
        assert!(
            response
                .body()
                .contains(r#""boot_args":"console=hvc0 reboot=k panic=1""#)
        );
        assert!(response.body().contains(r#""machine-config":"#));
        assert!(response.body().contains(r#""vcpu_count":2"#));
        assert!(response.body().contains(r#""mem_size_mib":256"#));
        assert!(response.body().contains(r#""drive_id":"rootfs""#));
        assert!(
            response
                .body()
                .contains(r#""path_on_host":"/tmp/rootfs.ext4""#)
        );
        assert!(response.body().contains(r#""is_root_device":true"#));
        assert!(response.body().contains(r#""is_read_only":true"#));
        assert!(response.body().contains(r#""partuuid":"0eaa91a0-01""#));
        assert!(response.body().contains(r#""network-interfaces":["#));
        assert!(response.body().contains(r#""iface_id":"eth0""#));
        assert!(response.body().contains(r#""host_dev_name":"tap0""#));
        assert!(
            response
                .body()
                .contains(r#""guest_mac":"12:34:56:78:9a:bc""#)
        );
        assert!(response.body().contains(r#""mmds-config":"#));
        assert!(response.body().contains(r#""network_interfaces":["eth0"]"#));
        assert!(response.body().contains(r#""version":"V2""#));
        assert!(
            response
                .body()
                .contains(r#""ipv4_address":"169.254.169.254""#)
        );
        assert!(response.body().contains(r#""imds_compat":true"#));
        assert!(response.body().contains(r#""vsock":"#));
        assert!(response.body().contains(r#""guest_cid":3"#));
        assert!(response.body().contains(r#""uds_path":"./v.sock""#));
        assert!(!response.body().contains("vsock_id"));
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::NotStarted
        );
    }

    #[test]
    fn dispatches_mmds_requests_to_runtime_store() {
        let mut vmm = test_controller();

        let get_response =
            handle_request_bytes(b"GET /mmds HTTP/1.1\r\nHost: localhost\r\n\r\n", &mut vmm);

        assert_eq!(
            get_response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );
        assert_eq!(
            get_response.body(),
            r#"{"fault_message":"The MMDS data store is not initialized."}"#
        );

        let network_body = r#"{"iface_id":"eth0","host_dev_name":"tap0"}"#;
        let network_request = format!(
            "PUT /network-interfaces/eth0 HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{network_body}",
            network_body.len()
        );
        assert_eq!(
            handle_request_bytes(network_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let config_body = r#"{"network_interfaces":["eth0"],"version":"V2","ipv4_address":"169.254.169.254","imds_compat":true}"#;
        let config_request = format!(
            "PUT /mmds/config HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{config_body}",
            config_body.len()
        );
        assert_eq!(
            handle_request_bytes(config_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let put_body = r#"{"latest":{"meta-data":{"ami-id":"ami-123","remove-me":true},"user-data":"before"}}"#;
        let put_request = format!(
            "PUT /mmds HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{put_body}",
            put_body.len()
        );
        assert_eq!(
            handle_request_bytes(put_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let patch_body = r#"{"latest":{"dynamic":{"instance-identity":"document"},"meta-data":{"ami-id":"ami-456","remove-me":null}}}"#;
        let patch_request = format!(
            "PATCH /mmds HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{patch_body}",
            patch_body.len()
        );
        assert_eq!(
            handle_request_bytes(patch_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let response =
            handle_request_bytes(b"GET /mmds HTTP/1.1\r\nHost: localhost\r\n\r\n", &mut vmm);

        assert_eq!(response.status(), bangbang_api::http::StatusCode::Ok);
        assert!(response.body().contains(r#""ami-id":"ami-456""#));
        assert!(
            response
                .body()
                .contains(r#""instance-identity":"document""#)
        );
        assert!(response.body().contains(r#""user-data":"before""#));
        assert!(!response.body().contains("remove-me"));
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::NotStarted
        );
        assert!(vmm.boot_source_config().is_none());
        assert!(vmm.drive_configs().is_empty());
    }

    #[test]
    fn mmds_config_rejects_unknown_runtime_network_interface_id() {
        let mut vmm = test_controller();
        let body = r#"{"network_interfaces":["eth0"]}"#;
        let request = format!(
            "PUT /mmds/config HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        let response = handle_request_bytes(request.as_bytes(), &mut vmm);

        assert_eq!(
            response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );
        assert_eq!(
            response.body(),
            r#"{"fault_message":"MMDS network interface id is not configured: eth0"}"#
        );

        let put_body = r#"{"latest":{"meta-data":{}}}"#;
        let put_request = format!(
            "PUT /mmds HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{put_body}",
            put_body.len()
        );
        assert_eq!(
            handle_request_bytes(put_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );
        let get_response =
            handle_request_bytes(b"GET /mmds HTTP/1.1\r\nHost: localhost\r\n\r\n", &mut vmm);
        assert_eq!(get_response.status(), bangbang_api::http::StatusCode::Ok);
        assert_eq!(get_response.body(), put_body);
    }

    #[test]
    fn patch_mmds_without_initialized_store_returns_fault() {
        let mut vmm = test_controller();
        let body = r#"{"latest":{"dynamic":{}}}"#;
        let request = format!(
            "PATCH /mmds HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        let response = handle_request_bytes(request.as_bytes(), &mut vmm);

        assert_eq!(
            response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );
        assert_eq!(
            response.body(),
            r#"{"fault_message":"The MMDS data store is not initialized."}"#
        );
        let get_response =
            handle_request_bytes(b"GET /mmds HTTP/1.1\r\nHost: localhost\r\n\r\n", &mut vmm);
        assert_eq!(
            get_response.body(),
            r#"{"fault_message":"The MMDS data store is not initialized."}"#
        );
    }

    #[test]
    fn put_mmds_request_with_object_body_returns_no_content() {
        let mut vmm = test_controller();
        for (method, path, body) in [
            ("PUT", "/mmds", r#"{"latest":{"meta-data":{}}}"#),
            ("PATCH", "/mmds", r#"{"latest":{"dynamic":{}}}"#),
        ] {
            let request = format!(
                "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let response = handle_request_bytes(request.as_bytes(), &mut vmm);
            assert_eq!(response.status(), bangbang_api::http::StatusCode::NoContent);
        }
    }

    #[test]
    fn invalid_mmds_config_request_does_not_reach_runtime() {
        let mut vmm = test_controller();
        let body = r#"{"network_interfaces":[]}"#;
        let request = format!(
            "PUT /mmds/config HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        let response = handle_request_bytes(request.as_bytes(), &mut vmm);

        assert_eq!(
            response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );
        assert_eq!(
            response.body(),
            r#"{"fault_message":"Malformed HTTP request."}"#
        );
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::NotStarted
        );
        assert!(vmm.boot_source_config().is_none());
        assert!(vmm.drive_configs().is_empty());
    }

    #[test]
    fn mmds_config_after_start_returns_state_fault() {
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        let boot_body = r#"{"kernel_image_path":"/tmp/vmlinux"}"#;
        let boot_request = format!(
            "PUT /boot-source HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{boot_body}",
            boot_body.len()
        );
        assert_eq!(
            handle_request_bytes(boot_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );
        let start_body = r#"{"action_type":"InstanceStart"}"#;
        let start_request = format!(
            "PUT /actions HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{start_body}",
            start_body.len()
        );
        assert_eq!(
            handle_request_bytes(start_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let body = r#"{"network_interfaces":["eth0"]}"#;
        let request = format!(
            "PUT /mmds/config HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let response = handle_request_bytes(request.as_bytes(), &mut vmm);

        assert_eq!(
            response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );
        assert_eq!(
            response.body(),
            r#"{"fault_message":"The requested operation is not supported in Running state: PutMmdsConfig"}"#
        );
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::Running
        );
        assert!(vmm.boot_source_config().is_some());
    }

    #[test]
    fn invalid_machine_config_request_does_not_mutate_vmm_state() {
        let mut vmm = test_controller();
        let body = r#"{"vcpu_count":2,"mem_size_mib":256}"#;
        let request = format!(
            "PUT /machine-config HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        assert_eq!(
            handle_request_bytes(request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let invalid_body = r#"{"vcpu_count":4,"mem_size_mib":512,"track_dirty_pages":true}"#;
        let invalid_request = format!(
            "PUT /machine-config HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{invalid_body}",
            invalid_body.len()
        );

        let response = handle_request_bytes(invalid_request.as_bytes(), &mut vmm);

        assert_eq!(
            response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );
        assert_eq!(
            response.body(),
            r#"{"fault_message":"Malformed HTTP request."}"#
        );
        assert_eq!(vmm.machine_config().vcpu_count(), 2);
        assert_eq!(vmm.machine_config().mem_size_mib(), 256);
    }

    #[test]
    fn configures_boot_source_over_unix_socket() {
        let mut vmm = test_controller();
        let path = unique_socket_path("boot-source");
        let server = ApiServer::bind(&path).expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");
        let boot_body = r#"{
            "kernel_image_path": "/tmp/nonexistent-private-vmlinux",
            "initrd_path": "/tmp/nonexistent-private-initrd.img",
            "boot_args": "console=ttyS0 reboot=k panic=1"
        }"#;
        let boot_request = format!(
            "PUT /boot-source HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{boot_body}",
            boot_body.len()
        );

        client
            .write_all(boot_request.as_bytes())
            .expect("client should write request");
        server
            .serve_next(&mut vmm)
            .expect("server should handle one request");

        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("client should read response");

        assert!(response.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert!(response.contains("Content-Length: 0\r\n"));
        assert!(response.ends_with("\r\n\r\n"));
        let config = vmm
            .boot_source_config()
            .expect("boot source config should be stored");
        assert_eq!(
            config.kernel_image_path(),
            Path::new("/tmp/nonexistent-private-vmlinux")
        );
        assert_eq!(
            config.initrd_path(),
            Some(Path::new("/tmp/nonexistent-private-initrd.img"))
        );
        assert_eq!(config.boot_args(), Some("console=ttyS0 reboot=k panic=1"));
        assert!(vmm.drive_configs().is_empty());
    }

    #[test]
    fn returns_fault_for_invalid_boot_source_without_storing() {
        let mut vmm = test_controller();
        let original_body = r#"{"kernel_image_path":"/tmp/original-vmlinux"}"#;
        let original_request = format!(
            "PUT /boot-source HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{original_body}",
            original_body.len()
        );
        assert_eq!(
            handle_request_bytes(original_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let path = unique_socket_path("boot-source-invalid");
        let server = ApiServer::bind(&path).expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");
        let invalid_body =
            r#"{"kernel_image_path":"/tmp/private-vmlinux","boot_args":"secret\u0000debug"}"#;
        let invalid_request = format!(
            "PUT /boot-source HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{invalid_body}",
            invalid_body.len()
        );

        client
            .write_all(invalid_request.as_bytes())
            .expect("client should write request");
        server
            .serve_next(&mut vmm)
            .expect("server should handle one request");

        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("client should read response");

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(response.contains(
            r#"{"fault_message":"kernel command line is invalid: contains a NUL byte"}"#
        ));
        assert!(!response.contains("secret"));
        assert!(!response.contains("/tmp/private-vmlinux"));
        let config = vmm
            .boot_source_config()
            .expect("original boot source config should remain stored");
        assert_eq!(
            config.kernel_image_path(),
            Path::new("/tmp/original-vmlinux")
        );
        assert_eq!(config.initrd_path(), None);
        assert_eq!(config.boot_args(), None);
    }

    #[test]
    fn configures_vsock_over_unix_socket() {
        let mut vmm = test_controller();
        let path = unique_socket_path("vsock");
        let server = ApiServer::bind(&path).expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");
        let body = r#"{
            "guest_cid": 3,
            "uds_path": "./v.sock"
        }"#;
        let request = format!(
            "PUT /vsock HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        client
            .write_all(request.as_bytes())
            .expect("client should write request");
        server
            .serve_next(&mut vmm)
            .expect("server should handle one request");

        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("client should read response");

        assert!(response.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert!(response.contains("Content-Length: 0\r\n"));
        let config_response = handle_request_bytes(
            b"GET /vm/config HTTP/1.1\r\nHost: localhost\r\n\r\n",
            &mut vmm,
        );
        assert_eq!(config_response.status(), bangbang_api::http::StatusCode::Ok);
        assert!(config_response.body().contains(r#""vsock":"#));
        assert!(config_response.body().contains(r#""guest_cid":3"#));
        assert!(config_response.body().contains(r#""uds_path":"./v.sock""#));
    }

    #[test]
    fn returns_fault_for_invalid_vsock_without_mutating() {
        let mut vmm = test_controller();
        let original_body = r#"{"guest_cid":3,"uds_path":"./original.sock"}"#;
        let original_request = format!(
            "PUT /vsock HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{original_body}",
            original_body.len()
        );
        assert_eq!(
            handle_request_bytes(original_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let invalid_body = r#"{"guest_cid":2,"uds_path":"/tmp/private-v.sock"}"#;
        let invalid_request = format!(
            "PUT /vsock HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{invalid_body}",
            invalid_body.len()
        );

        let response = handle_request_bytes(invalid_request.as_bytes(), &mut vmm);

        assert_eq!(
            response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );
        assert_eq!(
            response.body(),
            r#"{"fault_message":"vsock guest_cid 2 is below minimum 3"}"#
        );
        assert!(!response.body().contains("/tmp/private-v.sock"));

        let config_response = handle_request_bytes(
            b"GET /vm/config HTTP/1.1\r\nHost: localhost\r\n\r\n",
            &mut vmm,
        );
        assert!(config_response.body().contains(r#""guest_cid":3"#));
        assert!(
            config_response
                .body()
                .contains(r#""uds_path":"./original.sock""#)
        );
        assert!(!config_response.body().contains("/tmp/private-v.sock"));
    }

    #[test]
    fn rejects_vsock_after_start_without_creating_socket_path() {
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        let boot_body = r#"{"kernel_image_path":"/tmp/original-vmlinux"}"#;
        let boot_request = format!(
            "PUT /boot-source HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{boot_body}",
            boot_body.len()
        );
        assert_eq!(
            handle_request_bytes(boot_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );
        let start_response =
            put_action_over_socket(&mut vmm, "start-before-vsock", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));

        let uds_path = unique_socket_path("vsock-after-start");
        let body = format!(
            r#"{{"guest_cid":3,"uds_path":"{}"}}"#,
            uds_path.to_string_lossy()
        );
        let request = format!(
            "PUT /vsock HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        let response = handle_request_bytes(request.as_bytes(), &mut vmm);

        assert_eq!(
            response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );
        assert_eq!(
            response.body(),
            r#"{"fault_message":"The requested operation is not supported in Running state: PutVsock"}"#
        );
        assert!(!uds_path.exists());
    }

    #[test]
    fn returns_missing_boot_source_fault_for_instance_start_without_mutating_state() {
        let mut vmm = test_controller();
        let response = put_action_over_socket(&mut vmm, "start-missing-boot", "InstanceStart");

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(response.contains(
            r#"{"fault_message":"boot source must be configured before InstanceStart"}"#
        ));
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::NotStarted
        );
        assert_eq!(
            vmm.machine_config().vcpu_count(),
            bangbang_runtime::machine::DEFAULT_VCPU_COUNT
        );
        assert!(vmm.boot_source_config().is_none());
        assert!(vmm.drive_configs().is_empty());
    }

    #[test]
    fn returns_fault_for_configured_instance_start_failure_without_mutating_state() {
        let mut vmm = test_controller();
        let boot_body = r#"{"kernel_image_path":"/tmp/original-vmlinux"}"#;
        let boot_request = format!(
            "PUT /boot-source HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{boot_body}",
            boot_body.len()
        );
        assert_eq!(
            handle_request_bytes(boot_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let response = put_action_over_socket(&mut vmm, "start-configured", "InstanceStart");

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(response.contains(
            r#"{"fault_message":"failed to start microVM: invalid backend state: test startup failed"}"#
        ));
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::NotStarted
        );
        let config = vmm
            .boot_source_config()
            .expect("boot source config should remain stored");
        assert_eq!(
            config.kernel_image_path(),
            Path::new("/tmp/original-vmlinux")
        );
        assert!(vmm.drive_configs().is_empty());
    }

    #[test]
    fn configured_instance_start_success_commits_running_over_socket() {
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        let boot_body = r#"{"kernel_image_path":"/tmp/original-vmlinux"}"#;
        let boot_request = format!(
            "PUT /boot-source HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{boot_body}",
            boot_body.len()
        );
        assert_eq!(
            handle_request_bytes(boot_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let response = put_action_over_socket(&mut vmm, "start-ok", "InstanceStart");

        assert!(response.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert!(response.contains("Content-Length: 0\r\n"));
        assert!(response.ends_with("\r\n\r\n"));
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::Running
        );
        assert!(vmm.has_started_session());

        let second_response = put_action_over_socket(&mut vmm, "start-second", "InstanceStart");
        assert!(second_response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(second_response.contains(
            r#"{"fault_message":"The requested operation is not supported in Running state: InstanceStart"}"#
        ));
    }

    #[test]
    fn configures_metrics_without_adding_vm_config_section() {
        let mut vmm = test_controller();
        let metrics_path = unique_socket_path("metrics-output").with_extension("metrics");
        let body = format!(r#"{{"metrics_path":"{}"}}"#, metrics_path.to_string_lossy());
        let request = format!(
            "PUT /metrics HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        let response = handle_request_bytes(request.as_bytes(), &mut vmm);

        assert_eq!(response.status(), bangbang_api::http::StatusCode::NoContent);
        let config_response = handle_request_bytes(
            b"GET /vm/config HTTP/1.1\r\nHost: localhost\r\n\r\n",
            &mut vmm,
        );
        assert_eq!(config_response.status(), bangbang_api::http::StatusCode::Ok);
        assert!(!config_response.body().contains("metrics"));

        fs::remove_file(metrics_path).expect("fixture should clean up");
    }

    #[test]
    fn configures_logger_without_adding_vm_config_section() {
        let mut vmm = test_controller();
        let logger_path = unique_socket_path("logger-output").with_extension("log");
        let body = format!(
            r#"{{
                "log_path": "{}",
                "level": "Warning",
                "show_level": true,
                "show_log_origin": true,
                "module": "api_server"
            }}"#,
            logger_path.to_string_lossy()
        );
        let request = format!(
            "PUT /logger HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        let response = handle_request_bytes(request.as_bytes(), &mut vmm);

        assert_eq!(response.status(), bangbang_api::http::StatusCode::NoContent);
        assert!(logger_path.exists());

        let config_response = handle_request_bytes(
            b"GET /vm/config HTTP/1.1\r\nHost: localhost\r\n\r\n",
            &mut vmm,
        );
        assert_eq!(config_response.status(), bangbang_api::http::StatusCode::Ok);
        assert!(!config_response.body().contains("logger"));

        fs::remove_file(logger_path).expect("fixture should clean up");
    }

    #[test]
    fn rejects_logger_after_start_without_creating_output() {
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        let boot_body = r#"{"kernel_image_path":"/tmp/original-vmlinux"}"#;
        let boot_request = format!(
            "PUT /boot-source HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{boot_body}",
            boot_body.len()
        );
        assert_eq!(
            handle_request_bytes(boot_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );
        let start_response =
            put_action_over_socket(&mut vmm, "start-before-logger", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));

        let logger_path = unique_socket_path("logger-after-start").with_extension("log");
        let body = format!(r#"{{"log_path":"{}"}}"#, logger_path.to_string_lossy());
        let request = format!(
            "PUT /logger HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        let response = handle_request_bytes(request.as_bytes(), &mut vmm);

        assert_eq!(
            response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );
        assert_eq!(
            response.body(),
            r#"{"fault_message":"The requested operation is not supported in Running state: PutLogger"}"#
        );
        assert!(!logger_path.exists());
    }

    #[test]
    fn returns_state_fault_for_preboot_flush_metrics_without_mutating_state() {
        let mut vmm = test_controller();
        let response = put_action_over_socket(&mut vmm, "flush-metrics", "FlushMetrics");

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(response.contains(
            r#"{"fault_message":"The requested operation is not supported in Not started state: FlushMetrics"}"#
        ));
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::NotStarted
        );
        assert_eq!(
            vmm.machine_config().vcpu_count(),
            bangbang_runtime::machine::DEFAULT_VCPU_COUNT
        );
        assert!(vmm.boot_source_config().is_none());
        assert!(vmm.drive_configs().is_empty());
    }

    #[test]
    fn flush_metrics_after_start_without_configuration_is_noop() {
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        let boot_body = r#"{"kernel_image_path":"/tmp/original-vmlinux"}"#;
        let boot_request = format!(
            "PUT /boot-source HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{boot_body}",
            boot_body.len()
        );
        assert_eq!(
            handle_request_bytes(boot_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );
        let start_response =
            put_action_over_socket(&mut vmm, "start-before-flush", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));

        let flush_response = put_action_over_socket(&mut vmm, "flush-unconfigured", "FlushMetrics");

        assert!(flush_response.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert!(flush_response.contains("Content-Length: 0\r\n"));
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::Running
        );
    }

    #[test]
    fn configured_metrics_flush_writes_json_line_after_start() {
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        let metrics_path = unique_socket_path("metrics-flush").with_extension("metrics");
        let metrics_body = format!(r#"{{"metrics_path":"{}"}}"#, metrics_path.to_string_lossy());
        let metrics_request = format!(
            "PUT /metrics HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{metrics_body}",
            metrics_body.len()
        );
        assert_eq!(
            handle_request_bytes(metrics_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );
        let boot_body = r#"{"kernel_image_path":"/tmp/original-vmlinux"}"#;
        let boot_request = format!(
            "PUT /boot-source HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{boot_body}",
            boot_body.len()
        );
        assert_eq!(
            handle_request_bytes(boot_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );
        let start_response =
            put_action_over_socket(&mut vmm, "start-with-metrics", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));

        let flush_response = put_action_over_socket(&mut vmm, "flush-configured", "FlushMetrics");

        assert!(flush_response.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert_eq!(
            fs::read_to_string(&metrics_path).expect("metrics output should be readable"),
            "{\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(metrics_path).expect("fixture should clean up");
    }

    #[test]
    fn socket_path_cleanup_keeps_replaced_path() {
        let path = unique_socket_path("cln");
        let listener = UnixListener::bind(&path).expect("temporary listener should bind");
        let metadata = socket_path_metadata(&path).expect("temporary listener path should exist");

        fs::remove_file(&path).expect("temporary socket path should be removable");
        fs::write(&path, "replacement").expect("replacement should be written");

        remove_socket_path_if_owned(&path, metadata.dev(), metadata.ino());

        assert_eq!(
            fs::read_to_string(&path).expect("replacement should remain"),
            "replacement"
        );

        drop(listener);
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn socket_path_owner_check_rejects_replaced_socket() {
        let path = unique_socket_path("own");
        let listener = UnixListener::bind(&path).expect("temporary listener should bind");
        let metadata = socket_path_metadata(&path).expect("temporary listener path should exist");

        fs::remove_file(&path).expect("temporary socket path should be removable");
        let replacement =
            UnixListener::bind(&path).expect("replacement listener should bind same path");

        let err = ensure_socket_path_owner(&path, metadata.dev(), metadata.ino())
            .expect_err("replaced socket should not be owned");

        assert_eq!(err, ApiServerError::SocketPathChanged);

        drop(listener);
        drop(replacement);
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn serves_version_over_unix_socket() {
        let path = unique_socket_path("version");
        let server = ApiServer::bind(&path).expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");

        client
            .write_all(b"GET /version HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .expect("client should write request");
        let mut vmm = test_controller();
        server
            .serve_next(&mut vmm)
            .expect("server should handle one request");

        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("client should read response");

        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("Content-Type: application/json\r\n"));
        assert!(response.contains(r#"{"firecracker_version":"0.1.0"}"#));
    }

    #[test]
    fn serves_instance_info_over_unix_socket() {
        let path = unique_socket_path("instance-info");
        let server = ApiServer::bind(&path).expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");

        client
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .expect("client should write request");
        let mut vmm = test_controller();
        server
            .serve_next(&mut vmm)
            .expect("server should handle one request");

        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("client should read response");

        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("Content-Type: application/json\r\n"));
        assert!(response.contains(r#""id":"demo-1""#));
        assert!(response.contains(r#""state":"Not started""#));
        assert!(response.contains(r#""vmm_version":"0.1.0""#));
        assert!(response.contains(r#""app_name":"bangbang""#));
    }

    #[test]
    fn serves_vm_config_over_unix_socket() {
        let path = unique_socket_path("vm-config");
        let server = ApiServer::bind(&path).expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");
        let mut vmm = test_controller();
        vmm.handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(2, 256)))
            .expect("machine config should be stored");
        vmm.handle_action(VmmAction::PutBootSource(
            BootSourceConfigInput::new("/tmp/vmlinux")
                .with_initrd_path("/tmp/initrd.img")
                .with_boot_args("console=hvc0 reboot=k panic=1"),
        ))
        .expect("boot source config should be stored");
        vmm.handle_action(VmmAction::PutDrive(
            DriveConfigInput::new("rootfs", "rootfs", "/tmp/rootfs.ext4", true)
                .with_is_read_only(true)
                .with_partuuid("0eaa91a0-01"),
        ))
        .expect("drive config should be stored");
        vmm.handle_action(VmmAction::PutNetworkInterface(
            NetworkInterfaceConfigInput::new("eth0", "eth0", "tap0")
                .with_guest_mac("12:34:56:78:9a:bc"),
        ))
        .expect("network interface config should be stored");
        vmm.handle_action(VmmAction::PutVsock(
            VsockConfigInput::new(3, "./v.sock").with_vsock_id("vsock0"),
        ))
        .expect("vsock config should be stored");

        client
            .write_all(b"GET /vm/config HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .expect("client should write request");
        server
            .serve_next(&mut vmm)
            .expect("server should handle one request");

        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("client should read response");

        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("Content-Type: application/json\r\n"));
        assert!(response.contains(r#""boot-source":"#));
        assert!(response.contains(r#""kernel_image_path":"/tmp/vmlinux""#));
        assert!(response.contains(r#""initrd_path":"/tmp/initrd.img""#));
        assert!(response.contains(r#""boot_args":"console=hvc0 reboot=k panic=1""#));
        assert!(response.contains(r#""machine-config":"#));
        assert!(response.contains(r#""vcpu_count":2"#));
        assert!(response.contains(r#""mem_size_mib":256"#));
        assert!(response.contains(r#""drives":["#));
        assert!(response.contains(r#""drive_id":"rootfs""#));
        assert!(response.contains(r#""path_on_host":"/tmp/rootfs.ext4""#));
        assert!(response.contains(r#""is_root_device":true"#));
        assert!(response.contains(r#""is_read_only":true"#));
        assert!(response.contains(r#""partuuid":"0eaa91a0-01""#));
        assert!(response.contains(r#""network-interfaces":["#));
        assert!(response.contains(r#""iface_id":"eth0""#));
        assert!(response.contains(r#""host_dev_name":"tap0""#));
        assert!(response.contains(r#""guest_mac":"12:34:56:78:9a:bc""#));
        assert!(response.contains(r#""vsock":"#));
        assert!(response.contains(r#""guest_cid":3"#));
        assert!(response.contains(r#""uds_path":"./v.sock""#));
        assert!(!response.contains("vsock_id"));
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::NotStarted
        );
    }

    #[test]
    fn serves_mmds_over_unix_socket() {
        let path = unique_socket_path("mmds");
        let server = ApiServer::bind(&path).expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");
        let mut vmm = test_controller();
        let body = r#"{"latest":{"meta-data":{"ami-id":"ami-123"}}}"#;
        let request = format!(
            "PUT /mmds HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        assert_eq!(
            handle_request_bytes(request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        client
            .write_all(b"GET /mmds HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .expect("client should write request");
        server
            .serve_next(&mut vmm)
            .expect("server should handle one request");

        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("client should read response");

        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("Content-Type: application/json\r\n"));
        assert!(response.contains(r#""ami-id":"ami-123""#));
    }

    #[test]
    fn returns_fault_for_unsupported_path() {
        let path = unique_socket_path("fault");
        let server = ApiServer::bind(&path).expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");

        client
            .write_all(b"GET /unknown HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .expect("client should write request");
        let mut vmm = test_controller();
        server
            .serve_next(&mut vmm)
            .expect("server should handle one request");

        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("client should read response");

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(response.contains(r#"{"fault_message":"Invalid request method and/or path."}"#));
    }

    #[test]
    fn configures_machine_config_over_unix_socket() {
        let path = unique_socket_path("machine-config");
        let server = ApiServer::bind(&path).expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");
        let body = r#"{"vcpu_count":2,"mem_size_mib":256}"#;
        let request = format!(
            "PUT /machine-config HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        client
            .write_all(request.as_bytes())
            .expect("client should write request");
        let mut vmm = test_controller();
        server
            .serve_next(&mut vmm)
            .expect("server should handle one request");

        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("client should read response");

        assert!(response.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert!(response.contains("Content-Length: 0\r\n"));
        assert!(response.ends_with("\r\n\r\n"));
        assert_eq!(vmm.machine_config().vcpu_count(), 2);
        assert_eq!(vmm.machine_config().mem_size_mib(), 256);
        assert!(vmm.drive_configs().is_empty());
    }

    #[test]
    fn configures_drive_over_unix_socket() {
        let path = unique_socket_path("drive-config");
        let server = ApiServer::bind(&path).expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");
        let body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/rootfs.ext4",
            "is_root_device": true,
            "is_read_only": true,
            "partuuid": "0eaa91a0-01",
            "cache_type": "Unsafe",
            "io_engine": "Sync"
        }"#;
        let request = format!(
            "PUT /drives/rootfs HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        client
            .write_all(request.as_bytes())
            .expect("client should write request");
        let mut vmm = test_controller();
        server
            .serve_next(&mut vmm)
            .expect("server should handle one request");

        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("client should read response");

        assert!(response.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert!(response.contains("Content-Length: 0\r\n"));
        assert!(response.ends_with("\r\n\r\n"));
        assert_eq!(vmm.drive_configs().len(), 1);
        let config = &vmm.drive_configs()[0];
        assert_eq!(config.drive_id(), "rootfs");
        assert_eq!(config.path_on_host(), PathBuf::from("/tmp/rootfs.ext4"));
        assert!(config.is_root_device());
        assert!(config.is_read_only());
        assert_eq!(config.partuuid(), Some("0eaa91a0-01"));
        assert_eq!(config.cache_type(), DriveCacheType::Unsafe);
        assert_eq!(config.io_engine(), DriveIoEngine::Sync);
    }

    #[test]
    fn configures_network_interface_over_unix_socket() {
        let path = unique_socket_path("net-config");
        let server = ApiServer::bind(&path).expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");
        let body = r#"{
            "iface_id": "eth0",
            "host_dev_name": "tap0",
            "guest_mac": "12:34:56:78:9a:BC"
        }"#;
        let request = format!(
            "PUT /network-interfaces/eth0 HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        client
            .write_all(request.as_bytes())
            .expect("client should write request");
        let mut vmm = test_controller();
        server
            .serve_next(&mut vmm)
            .expect("server should handle one request");

        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("client should read response");

        assert!(response.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert!(response.contains("Content-Length: 0\r\n"));
        assert!(response.ends_with("\r\n\r\n"));

        let data = vmm
            .handle_action(VmmAction::GetVmConfig)
            .expect("VM config should be returned");
        let VmmData::VmConfiguration(config) = data else {
            panic!("expected VM config");
        };
        assert_eq!(config.network_interface_configs().len(), 1);
        let config = &config.network_interface_configs()[0];
        assert_eq!(config.iface_id(), "eth0");
        assert_eq!(config.host_dev_name(), "tap0");
        assert_eq!(
            config.guest_mac().map(|guest_mac| guest_mac.to_string()),
            Some("12:34:56:78:9a:bc".to_string())
        );
    }

    #[test]
    fn returns_fault_for_network_interface_count_over_limit_without_storing() {
        let mut vmm = test_controller();

        for index in 0..MAX_NETWORK_INTERFACE_COUNT {
            let body = format!(r#"{{"iface_id":"eth{index}","host_dev_name":"tap{index}"}}"#);
            let request = format!(
                "PUT /network-interfaces/eth{index} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let response = handle_request_bytes(request.as_bytes(), &mut vmm);

            assert_eq!(response.status(), bangbang_api::http::StatusCode::NoContent);
        }

        let path = unique_socket_path("net-limit");
        let server = ApiServer::bind(&path).expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");
        let body = format!(
            r#"{{"iface_id":"eth{MAX_NETWORK_INTERFACE_COUNT}","host_dev_name":"tap{MAX_NETWORK_INTERFACE_COUNT}"}}"#
        );
        let request = format!(
            "PUT /network-interfaces/eth{MAX_NETWORK_INTERFACE_COUNT} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        client
            .write_all(request.as_bytes())
            .expect("client should write request");
        server
            .serve_next(&mut vmm)
            .expect("server should handle one request");

        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("client should read response");

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(
            response.contains(r#"{"fault_message":"network interface count exceeds maximum 16"}"#)
        );

        let data = vmm
            .handle_action(VmmAction::GetVmConfig)
            .expect("VM config should be returned");
        let VmmData::VmConfiguration(config) = data else {
            panic!("expected VM config");
        };
        assert_eq!(
            config.network_interface_configs().len(),
            MAX_NETWORK_INTERFACE_COUNT
        );
    }

    #[test]
    fn returns_fault_for_invalid_drive_config_without_storing() {
        let path = unique_socket_path("drive-invalid");
        let server = ApiServer::bind(&path).expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");
        let body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "",
            "is_root_device": true
        }"#;
        let request = format!(
            "PUT /drives/rootfs HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        client
            .write_all(request.as_bytes())
            .expect("client should write request");
        let mut vmm = test_controller();
        server
            .serve_next(&mut vmm)
            .expect("server should handle one request");

        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("client should read response");

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(response.contains(r#"{"fault_message":"drive path_on_host must not be empty"}"#));
        assert!(vmm.drive_configs().is_empty());
    }

    #[test]
    fn returns_fault_for_unsupported_network_mtu_without_storing() {
        let mut vmm = test_controller();
        let body = r#"{
            "iface_id": "eth0",
            "host_dev_name": "tap0",
            "mtu": 1500
        }"#;
        let request = format!(
            "PUT /network-interfaces/eth0 HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        let response = handle_request_bytes(request.as_bytes(), &mut vmm);

        assert_eq!(
            response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );
        assert_eq!(
            response.body(),
            r#"{"fault_message":"network mtu is not supported"}"#
        );

        let config_response = handle_request_bytes(
            b"GET /vm/config HTTP/1.1\r\nHost: localhost\r\n\r\n",
            &mut vmm,
        );
        assert_eq!(config_response.status(), bangbang_api::http::StatusCode::Ok);
        assert!(
            config_response
                .body()
                .contains(r#""network-interfaces":[]"#)
        );
    }

    #[test]
    fn returns_fault_for_unsupported_drive_socket_without_leaking_path() {
        let path = unique_socket_path("drive-socket");
        let server = ApiServer::bind(&path).expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");
        let body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/rootfs.ext4",
            "is_root_device": true,
            "socket": "/tmp/private-vhost.sock"
        }"#;
        let request = format!(
            "PUT /drives/rootfs HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        client
            .write_all(request.as_bytes())
            .expect("client should write request");
        let mut vmm = test_controller();
        server
            .serve_next(&mut vmm)
            .expect("server should handle one request");

        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("client should read response");

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(response.contains(r#"{"fault_message":"drive socket is not supported"}"#));
        assert!(!response.contains("/tmp/private-vhost.sock"));
        assert!(vmm.drive_configs().is_empty());
    }

    #[test]
    fn returns_fault_for_unsupported_drive_cache_without_storing() {
        let path = unique_socket_path("drive-cache");
        let server = ApiServer::bind(&path).expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");
        let body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/rootfs.ext4",
            "is_root_device": true,
            "cache_type": "Writeback"
        }"#;
        let request = format!(
            "PUT /drives/rootfs HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        client
            .write_all(request.as_bytes())
            .expect("client should write request");
        let mut vmm = test_controller();
        server
            .serve_next(&mut vmm)
            .expect("server should handle one request");

        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("client should read response");

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(
            response.contains(r#"{"fault_message":"drive cache_type Writeback is not supported"}"#)
        );
        assert!(vmm.drive_configs().is_empty());
    }

    #[test]
    fn returns_fault_for_unsupported_drive_io_engine_without_storing() {
        let path = unique_socket_path("drive-io");
        let server = ApiServer::bind(&path).expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");
        let body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/rootfs.ext4",
            "is_root_device": true,
            "io_engine": "Async"
        }"#;
        let request = format!(
            "PUT /drives/rootfs HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        client
            .write_all(request.as_bytes())
            .expect("client should write request");
        let mut vmm = test_controller();
        server
            .serve_next(&mut vmm)
            .expect("server should handle one request");

        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("client should read response");

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(response.contains(r#"{"fault_message":"drive io_engine Async is not supported"}"#));
        assert!(vmm.drive_configs().is_empty());
    }

    #[test]
    fn returns_fault_for_request_over_payload_limit() {
        let path = unique_socket_path("limit");
        let server = ApiServer::bind(&path).expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");
        let request = format!(
            "GET /version HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            HTTP_MAX_PAYLOAD_SIZE + 1
        );

        client
            .write_all(request.as_bytes())
            .expect("client should write request");
        let mut vmm = test_controller();
        server
            .serve_next(&mut vmm)
            .expect("server should handle one request");

        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("client should read response");

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(
            response.contains(
                r#"{"fault_message":"HTTP request payload exceeds the configured limit."}"#
            )
        );
    }

    #[test]
    fn client_disconnect_does_not_fail_server() {
        let path = unique_socket_path("disconnect");
        let server = ApiServer::bind(&path).expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");

        client
            .write_all(b"GET /version HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .expect("client should write request");
        drop(client);

        let mut vmm = test_controller();
        server
            .serve_next(&mut vmm)
            .expect("client disconnect should not fail server");
    }

    #[test]
    fn run_until_cleans_socket_after_shutdown_request() {
        let path = unique_socket_path("shutdown");
        let server = ApiServer::bind(&path).expect("server should bind");
        let (mut shutdown_reader, mut shutdown_writer) =
            UnixStream::pair().expect("shutdown stream pair should be created");
        let mut client = UnixStream::connect(&path).expect("client should connect");

        let handle = thread::spawn(move || {
            let mut vmm = test_controller();
            server.run_until(&mut vmm, &mut shutdown_reader)
        });

        client
            .write_all(b"GET /version HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .expect("client should write request");

        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("client should read response");
        shutdown_writer
            .write_all(b"x")
            .expect("shutdown wakeup should be written");

        assert_eq!(
            handle.join().expect("server thread should not panic"),
            Ok(())
        );
        assert!(response.contains(r#"{"firecracker_version":"0.1.0"}"#));
        assert!(!path.exists());
    }

    #[test]
    fn run_until_cleans_idle_socket_after_shutdown_request() {
        let path = unique_socket_path("idle-shutdown");
        let server = ApiServer::bind(&path).expect("server should bind");
        let (mut shutdown_reader, mut shutdown_writer) =
            UnixStream::pair().expect("shutdown stream pair should be created");
        let handle = thread::spawn(move || {
            let mut vmm = test_controller();
            server.run_until(&mut vmm, &mut shutdown_reader)
        });

        shutdown_writer
            .write_all(b"x")
            .expect("shutdown wakeup should be written");

        assert_eq!(
            handle.join().expect("server thread should not panic"),
            Ok(())
        );
        assert!(!path.exists());
    }

    #[test]
    fn request_read_timeout_returns_partial_request_after_expired_deadline() {
        let (mut client, mut server) = UnixStream::pair().expect("stream pair should be created");
        let partial_request = b"GET /version HTTP/1.1\r\n";

        client
            .write_all(partial_request)
            .expect("client should write partial request");

        let start = Instant::now();
        let deadline = start + Duration::from_secs(1);
        let mut first_now = true;
        let mut now = || {
            if std::mem::replace(&mut first_now, false) {
                start
            } else {
                deadline + Duration::from_nanos(1)
            }
        };

        let request = read_request_until(&mut server, deadline, &mut now)
            .expect("read timeout should not fail");

        assert_eq!(request, RequestRead::Complete(partial_request.to_vec()));
    }

    #[test]
    fn fails_when_socket_path_exists_without_deleting_it() {
        let path = unique_socket_path("exists");
        fs::write(&path, "existing file").expect("fixture file should be written");

        let err = ApiServer::bind(&path).expect_err("existing path should fail");

        assert_eq!(err, ApiServerError::SocketPathExists);
        assert_eq!(
            fs::read_to_string(&path).expect("existing file should remain"),
            "existing file"
        );

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn fails_when_socket_path_is_broken_symlink_without_deleting_it() {
        let path = unique_socket_path("symlink");
        let target = unique_socket_path("missing-target");
        std::os::unix::fs::symlink(&target, &path).expect("fixture symlink should be created");

        let err = ApiServer::bind(&path).expect_err("existing symlink path should fail");

        assert_eq!(err, ApiServerError::SocketPathExists);
        assert!(
            fs::symlink_metadata(&path)
                .expect("symlink should remain")
                .file_type()
                .is_symlink()
        );

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn publish_does_not_replace_existing_socket_path() {
        let path = unique_socket_path("publish-race");
        let temp_path = unique_socket_path("publish-temp");
        let temp_listener = UnixListener::bind(&temp_path).expect("temporary listener should bind");
        fs::write(&path, "replacement").expect("replacement should be written");

        let err = publish_socket_path(&temp_path, &path)
            .expect_err("publishing over an existing path should fail");

        assert_eq!(err, ApiServerError::SocketPathExists);
        assert_eq!(
            fs::read_to_string(&path).expect("replacement should remain"),
            "replacement"
        );
        assert!(temp_path.exists());

        drop(temp_listener);
        fs::remove_file(temp_path).expect("temporary socket should clean up");
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn concurrent_binds_allow_only_one_owner() {
        const ATTEMPTS: usize = 8;

        let dir = unique_temp_dir("concurrent");
        let path = dir.join("api.sock");
        let start = Arc::new(Barrier::new(ATTEMPTS));
        let finish = Arc::new(Barrier::new(ATTEMPTS));
        let handles = (0..ATTEMPTS)
            .map(|_| {
                let path = path.clone();
                let start = Arc::clone(&start);
                let finish = Arc::clone(&finish);

                thread::spawn(move || {
                    start.wait();
                    let result = ApiServer::bind(&path);
                    let outcome = (
                        result.is_ok(),
                        matches!(
                            result.as_ref().err(),
                            Some(ApiServerError::SocketPathExists)
                        ),
                    );
                    finish.wait();
                    outcome
                })
            })
            .collect::<Vec<_>>();

        let results = handles
            .into_iter()
            .map(|handle| handle.join().expect("bind thread should not panic"))
            .collect::<Vec<_>>();

        assert_eq!(results.iter().filter(|(is_ok, _)| *is_ok).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|(_, is_path_exists)| *is_path_exists)
                .count(),
            ATTEMPTS - 1
        );
        assert!(!path.exists());
        assert_eq!(temporary_socket_entries(&dir), Vec::<PathBuf>::new());

        fs::remove_dir(dir).expect("fixture directory should clean up");
    }

    #[test]
    fn removes_owned_socket_on_drop() {
        let path = unique_socket_path("cleanup");
        let server = ApiServer::bind(&path).expect("server should bind");

        assert!(path.exists());

        drop(server);

        assert!(!path.exists());
    }

    #[test]
    fn does_not_remove_replaced_socket_path_on_drop() {
        let path = unique_socket_path("replaced");
        let server = ApiServer::bind(&path).expect("server should bind");

        fs::remove_file(&path).expect("socket path should be removable");
        fs::write(&path, "replacement").expect("replacement file should be written");

        drop(server);

        assert_eq!(
            fs::read_to_string(&path).expect("replacement should remain"),
            "replacement"
        );

        fs::remove_file(path).expect("fixture should clean up");
    }
}
