use std::ffi::{CString, OsString};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[cfg(test)]
use bangbang_api::HTTP_MAX_PAYLOAD_SIZE;
use bangbang_api::http::{
    ActionRequest, ActionType, ApiRequest, ApiRequestMetricEndpoint, ApiRequestMetricPatchEndpoint,
    ApiRequestMetricPutEndpoint, BootSourceRequest, BootSourceResponse, CpuConfigRequest,
    DriveCacheType as ApiDriveCacheType, DriveConfigRequest, DriveConfigResponse,
    DriveIoEngine as ApiDriveIoEngine, DrivePatchRequest, EntropyConfigRequest,
    EntropyConfigResponse, HotUnplugDeviceKind as ApiHotUnplugDeviceKind, HotUnplugDeviceRequest,
    HttpResponse, LoggerConfigRequest, LoggerLevel as ApiLoggerLevel, MachineConfigPatchRequest,
    MachineConfigRequest, MachineConfigResponse, MetricsConfigRequest, MmdsConfigRequest,
    MmdsConfigResponse, MmdsContentRequest, MmdsVersion as ApiMmdsVersion,
    NetworkInterfaceConfigRequest, NetworkInterfaceConfigResponse, NetworkInterfacePatchRequest,
    RequestError, SerialConfigRequest, VmConfigResponse, VmStateUpdate, VmStateUpdateRequest,
    VsockConfigRequest, VsockConfigResponse, api_request_metric_endpoint, parse_request_with_limit,
    request_total_len_with_limit,
};
use bangbang_runtime::block::{
    DriveCacheType, DriveConfig, DriveConfigInput, DriveIoEngine, DriveUpdateInput,
};
use bangbang_runtime::boot::{BootSourceConfig, BootSourceConfigInput};
use bangbang_runtime::cpu::CpuConfigInput;
use bangbang_runtime::entropy::EntropyConfigInput;
use bangbang_runtime::logger::{LoggerConfigInput, LoggerLevel};
use bangbang_runtime::machine::{
    MachineConfig, MachineConfigCpuTemplate as RuntimeMachineConfigCpuTemplate,
    MachineConfigHugePages as RuntimeMachineConfigHugePages, MachineConfigInput,
    MachineConfigPatchInput,
};
use bangbang_runtime::metrics::MetricsConfigInput;
use bangbang_runtime::mmds::{
    MmdsConfig, MmdsConfigInput, MmdsContentInput, MmdsVersion as RuntimeMmdsVersion,
};
#[cfg(test)]
use bangbang_runtime::network::MAX_NETWORK_INTERFACE_COUNT;
use bangbang_runtime::network::{
    NetworkInterfaceConfig, NetworkInterfaceConfigInput, NetworkInterfaceUpdateInput,
};
use bangbang_runtime::serial::SerialConfigInput;
use bangbang_runtime::vsock::{VsockConfig, VsockConfigInput};
use bangbang_runtime::{
    HotUnplugDeviceInput, HotUnplugDeviceKind as RuntimeHotUnplugDeviceKind, VmConfiguration,
    VmmAction, VmmActionError, VmmData,
};

use crate::periodic_metrics::PeriodicMetricsScheduler;
use crate::vmm::{
    ApiRequestMetricParseFailure, ApiRequestMetricPatchParseFailure,
    ApiRequestMetricPutParseFailure, GetApiRequest, PatchApiRequest, ProcessSessionExitDecision,
    PutApiRequest, VmmRequestHandler,
};

const READ_CHUNK_SIZE: usize = 4096;
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);
const API_SOCKET_MODE: u32 = 0o600;
static NEXT_TEMP_SOCKET_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ApiServerError {
    Accept(std::io::ErrorKind),
    Bind(std::io::ErrorKind),
    Connection(std::io::ErrorKind),
    PeriodicMetricsFlush(VmmActionError),
    ProcessSessionTerminal,
    SocketMetadata(std::io::ErrorKind),
    SocketPermissions(std::io::ErrorKind),
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
            Self::PeriodicMetricsFlush(err) => {
                write!(f, "failed to flush periodic metrics: {err}")
            }
            Self::ProcessSessionTerminal => {
                f.write_str("process-owned boot run loop exited with failure")
            }
            Self::SocketMetadata(kind) => {
                write!(f, "failed to inspect bound API socket: {kind:?}")
            }
            Self::SocketPermissions(kind) => {
                write!(f, "failed to restrict API socket permissions: {kind:?}")
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
    http_api_max_payload_size: usize,
    _socket_guard: SocketGuard,
}

impl ApiServer {
    #[cfg(test)]
    pub(crate) fn bind(path: impl AsRef<Path>) -> Result<Self, ApiServerError> {
        Self::bind_with_max_payload_size(path, HTTP_MAX_PAYLOAD_SIZE)
    }

    pub(crate) fn bind_with_max_payload_size(
        path: impl AsRef<Path>,
        http_api_max_payload_size: usize,
    ) -> Result<Self, ApiServerError> {
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
            http_api_max_payload_size,
            _socket_guard: socket_guard,
        })
    }

    pub(crate) fn run_until(
        &self,
        vmm: &mut impl VmmRequestHandler,
        shutdown_wakeup: &mut UnixStream,
    ) -> Result<(), ApiServerError> {
        self.run_until_with_periodic_metrics_scheduler(
            vmm,
            shutdown_wakeup,
            PeriodicMetricsScheduler::new(Instant::now()),
        )
    }

    fn run_until_with_periodic_metrics_scheduler(
        &self,
        vmm: &mut impl VmmRequestHandler,
        shutdown_wakeup: &mut UnixStream,
        mut metrics_scheduler: PeriodicMetricsScheduler,
    ) -> Result<(), ApiServerError> {
        self.listener
            .set_nonblocking(true)
            .map_err(|err| ApiServerError::Accept(err.kind()))?;
        shutdown_wakeup
            .set_nonblocking(true)
            .map_err(|err| ApiServerError::Connection(err.kind()))?;

        loop {
            let now = Instant::now();
            match wait_for_listener_or_shutdown(
                &self.listener,
                shutdown_wakeup,
                vmm.process_exit_wakeup_fd(),
                Some(metrics_scheduler.poll_timeout_ms(now)),
            )? {
                ApiServerWaitResult::Ready => {}
                ApiServerWaitResult::TimedOut => {
                    vmm.handle_periodic_metrics_flush()
                        .map_err(ApiServerError::PeriodicMetricsFlush)?;
                    metrics_scheduler.schedule_next(Instant::now());
                    continue;
                }
            }
            if drain_shutdown_wakeup(shutdown_wakeup)? {
                return Ok(());
            }
            vmm.drain_process_exit_wakeup()
                .map_err(ApiServerError::Connection)?;
            match vmm.process_exit_status().decision() {
                ProcessSessionExitDecision::Continue => {}
                ProcessSessionExitDecision::ExitSuccessfully => return Ok(()),
                ProcessSessionExitDecision::ExitWithFailure => {
                    return Err(ApiServerError::ProcessSessionTerminal);
                }
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

        let _ = handle_connection(&mut stream, vmm, self.http_api_max_payload_size);

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApiServerWaitResult {
    Ready,
    TimedOut,
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
    process_exit_wakeup_fd: Option<RawFd>,
    timeout_ms: Option<i32>,
) -> Result<ApiServerWaitResult, ApiServerError> {
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
        libc::pollfd {
            fd: process_exit_wakeup_fd.unwrap_or(-1),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    let poll_fd_count = if process_exit_wakeup_fd.is_some() {
        poll_fds.len()
    } else {
        poll_fds.len() - 1
    };
    let poll_fds = poll_fds
        .get_mut(..poll_fd_count)
        .ok_or(ApiServerError::Connection(std::io::ErrorKind::InvalidInput))?;

    loop {
        for poll_fd in poll_fds.iter_mut() {
            poll_fd.revents = 0;
        }

        // SAFETY: `poll_fds` points to initialized `pollfd` values and remains
        // valid for the duration of the call.
        let result = unsafe {
            libc::poll(
                poll_fds.as_mut_ptr(),
                poll_fds.len() as _,
                timeout_ms.unwrap_or(-1),
            )
        };
        if result > 0 {
            return Ok(ApiServerWaitResult::Ready);
        }
        if result == 0 {
            return Ok(ApiServerWaitResult::TimedOut);
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
        let bound_metadata = BoundSocketMetadata {
            path: temp_path,
            dev: metadata.dev(),
            ino: metadata.ino(),
        };
        if let Err(err) = restrict_socket_path_permissions(
            &bound_metadata.path,
            bound_metadata.dev,
            bound_metadata.ino,
        ) {
            remove_socket_path_if_owned(
                &bound_metadata.path,
                bound_metadata.dev,
                bound_metadata.ino,
            );
            return Err(err);
        }

        return Ok((listener, bound_metadata));
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

fn restrict_socket_path_permissions(path: &Path, dev: u64, ino: u64) -> Result<(), ApiServerError> {
    ensure_socket_path_owner(path, dev, ino)?;
    fs::set_permissions(path, fs::Permissions::from_mode(API_SOCKET_MODE))
        .map_err(|err| ApiServerError::SocketPermissions(err.kind()))?;
    ensure_socket_path_owner(path, dev, ino)
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
    http_api_max_payload_size: usize,
) -> Result<(), ApiServerError> {
    stream
        .set_write_timeout(Some(CONNECTION_TIMEOUT))
        .map_err(|err| ApiServerError::Connection(err.kind()))?;

    let response =
        match read_request_with_limit(stream, CONNECTION_TIMEOUT, http_api_max_payload_size)? {
            RequestRead::Complete(request) => {
                handle_request_bytes_with_limit(&request, vmm, http_api_max_payload_size)
            }
            RequestRead::TooLarge => {
                HttpResponse::fault(RequestError::PayloadTooLarge.fault_message())
            }
        };

    stream
        .write_all(&response.to_http_bytes())
        .map_err(|err| ApiServerError::Connection(err.kind()))
}

#[cfg(test)]
fn handle_request_bytes(bytes: &[u8], vmm: &mut impl VmmRequestHandler) -> HttpResponse {
    handle_request_bytes_with_limit(bytes, vmm, HTTP_MAX_PAYLOAD_SIZE)
}

fn handle_request_bytes_with_limit(
    bytes: &[u8],
    vmm: &mut impl VmmRequestHandler,
    http_api_max_payload_size: usize,
) -> HttpResponse {
    match parse_request_with_limit(bytes, http_api_max_payload_size) {
        Ok(request) => handle_api_request(request, vmm),
        Err(err) => {
            if err == RequestError::SendCtrlAltDelUnsupported {
                record_unsupported_put_action_request(bytes, vmm);
            } else if should_record_api_request_parse_failure(&err) {
                record_api_request_parse_failure(bytes, vmm);
            }
            HttpResponse::fault(err.fault_message())
        }
    }
}

fn record_unsupported_put_action_request(bytes: &[u8], vmm: &mut impl VmmRequestHandler) {
    if matches!(
        api_request_metric_endpoint(bytes),
        Some(ApiRequestMetricEndpoint::Put(
            ApiRequestMetricPutEndpoint::Actions
        ))
    ) {
        vmm.record_put_actions_request();
    }
}

const fn should_record_api_request_parse_failure(err: &RequestError) -> bool {
    matches!(
        err,
        RequestError::MalformedRequest
            | RequestError::MismatchedDriveId
            | RequestError::MismatchedInterfaceId
            | RequestError::MismatchedPmemId
    )
}

fn record_api_request_parse_failure(bytes: &[u8], vmm: &mut impl VmmRequestHandler) {
    let Some(endpoint) = api_request_metric_endpoint(bytes) else {
        return;
    };

    vmm.record_api_request_parse_failure(api_request_metric_parse_failure(endpoint));
}

const fn api_request_metric_parse_failure(
    endpoint: ApiRequestMetricEndpoint,
) -> ApiRequestMetricParseFailure {
    match endpoint {
        ApiRequestMetricEndpoint::Patch(endpoint) => {
            ApiRequestMetricParseFailure::Patch(api_request_metric_patch_parse_failure(endpoint))
        }
        ApiRequestMetricEndpoint::Put(endpoint) => {
            ApiRequestMetricParseFailure::Put(api_request_metric_put_parse_failure(endpoint))
        }
    }
}

const fn api_request_metric_put_parse_failure(
    endpoint: ApiRequestMetricPutEndpoint,
) -> ApiRequestMetricPutParseFailure {
    match endpoint {
        ApiRequestMetricPutEndpoint::Actions => ApiRequestMetricPutParseFailure::Actions,
        ApiRequestMetricPutEndpoint::BootSource => ApiRequestMetricPutParseFailure::BootSource,
        ApiRequestMetricPutEndpoint::CpuConfig => ApiRequestMetricPutParseFailure::CpuConfig,
        ApiRequestMetricPutEndpoint::Drive => ApiRequestMetricPutParseFailure::Drive,
        ApiRequestMetricPutEndpoint::HotplugMemory => {
            ApiRequestMetricPutParseFailure::HotplugMemory
        }
        ApiRequestMetricPutEndpoint::Logger => ApiRequestMetricPutParseFailure::Logger,
        ApiRequestMetricPutEndpoint::MachineConfig => {
            ApiRequestMetricPutParseFailure::MachineConfig
        }
        ApiRequestMetricPutEndpoint::Metrics => ApiRequestMetricPutParseFailure::Metrics,
        ApiRequestMetricPutEndpoint::Mmds => ApiRequestMetricPutParseFailure::Mmds,
        ApiRequestMetricPutEndpoint::Network => ApiRequestMetricPutParseFailure::Network,
        ApiRequestMetricPutEndpoint::Pmem => ApiRequestMetricPutParseFailure::Pmem,
        ApiRequestMetricPutEndpoint::Serial => ApiRequestMetricPutParseFailure::Serial,
        ApiRequestMetricPutEndpoint::Vsock => ApiRequestMetricPutParseFailure::Vsock,
    }
}

const fn api_request_metric_patch_parse_failure(
    endpoint: ApiRequestMetricPatchEndpoint,
) -> ApiRequestMetricPatchParseFailure {
    match endpoint {
        ApiRequestMetricPatchEndpoint::Drive => ApiRequestMetricPatchParseFailure::Drive,
        ApiRequestMetricPatchEndpoint::HotplugMemory => {
            ApiRequestMetricPatchParseFailure::HotplugMemory
        }
        ApiRequestMetricPatchEndpoint::MachineConfig => {
            ApiRequestMetricPatchParseFailure::MachineConfig
        }
        ApiRequestMetricPatchEndpoint::Mmds => ApiRequestMetricPatchParseFailure::Mmds,
        ApiRequestMetricPatchEndpoint::Network => ApiRequestMetricPatchParseFailure::Network,
        ApiRequestMetricPatchEndpoint::Pmem => ApiRequestMetricPatchParseFailure::Pmem,
    }
}

fn handle_api_request(request: ApiRequest, vmm: &mut impl VmmRequestHandler) -> HttpResponse {
    record_deprecated_api_usage(&request, vmm);

    match request {
        ApiRequest::GetInstanceInfo => {
            handle_instance_info(vmm.handle_get_request(GetApiRequest::InstanceInfo))
        }
        ApiRequest::GetVersion => {
            handle_vmm_version(vmm.handle_get_request(GetApiRequest::VmmVersion))
        }
        ApiRequest::GetMachineConfig => {
            handle_machine_config(vmm.handle_get_request(GetApiRequest::MachineConfig))
        }
        ApiRequest::GetMmds => handle_mmds(vmm.handle_get_request(GetApiRequest::Mmds)),
        ApiRequest::GetVmConfig => handle_vm_config(vmm.handle_action(VmmAction::GetVmConfig)),
        ApiRequest::PutAction(action) => {
            handle_empty(vmm.handle_put_action_request(action_from_request(action.as_ref())))
        }
        ApiRequest::PutBootSource(config) => handle_empty(vmm.handle_put_request(
            PutApiRequest::boot_source(boot_source_input_from_request(config.as_ref())),
        )),
        ApiRequest::PutCpuConfig(config) => handle_empty(vmm.handle_put_request(
            PutApiRequest::cpu_config(cpu_config_input_from_request(config.as_ref())),
        )),
        ApiRequest::PutLogger(config) => handle_empty(vmm.handle_put_request(
            PutApiRequest::logger(logger_config_input_from_request(config.as_ref())),
        )),
        ApiRequest::PutMachineConfig(config) => handle_empty(vmm.handle_put_request(
            PutApiRequest::machine_config(machine_config_input_from_request(config.as_ref())),
        )),
        ApiRequest::PatchMachineConfig(config) => {
            handle_empty(vmm.handle_patch_request(PatchApiRequest::machine_config(
                machine_config_patch_input_from_request(config.as_ref()),
            )))
        }
        ApiRequest::PutMetrics(config) => handle_empty(vmm.handle_put_request(
            PutApiRequest::metrics(metrics_config_input_from_request(config.as_ref())),
        )),
        ApiRequest::PutMmds(content) => handle_empty(vmm.handle_put_request(PutApiRequest::mmds(
            mmds_content_input_from_request(content.as_ref()),
        ))),
        ApiRequest::PatchMmds(content) => handle_empty(vmm.handle_patch_request(
            PatchApiRequest::mmds(mmds_content_input_from_request(content.as_ref())),
        )),
        ApiRequest::PutMmdsConfig(config) => handle_empty(vmm.handle_put_request(
            PutApiRequest::mmds_config(mmds_config_input_from_request(config.as_ref())),
        )),
        ApiRequest::PutDrive(config) => handle_empty(vmm.handle_put_request(PutApiRequest::drive(
            drive_config_input_from_request(config.as_ref()),
        ))),
        ApiRequest::PatchDrive(config) => handle_empty(vmm.handle_patch_request(
            PatchApiRequest::drive(drive_update_input_from_request(config.as_ref())),
        )),
        ApiRequest::PatchVmState(update) => {
            handle_empty(vmm.handle_action(vm_state_action_from_request(update.as_ref())))
        }
        ApiRequest::PutNetworkInterface(config) => handle_empty(vmm.handle_put_request(
            PutApiRequest::network(network_interface_config_input_from_request(config.as_ref())),
        )),
        ApiRequest::PatchNetworkInterface(config) => handle_empty(vmm.handle_patch_request(
            PatchApiRequest::network(network_interface_update_input_from_request(config.as_ref())),
        )),
        ApiRequest::HotUnplugDevice(request) => {
            handle_empty(vmm.handle_action(hot_unplug_action_from_request(request.as_ref())))
        }
        ApiRequest::PutSerial(config) => handle_empty(vmm.handle_put_request(
            PutApiRequest::serial(serial_config_input_from_request(config.as_ref())),
        )),
        ApiRequest::GetBalloon => handle_empty(vmm.handle_get_request(GetApiRequest::Balloon)),
        ApiRequest::GetBalloonStats => {
            handle_empty(vmm.handle_get_request(GetApiRequest::BalloonStats))
        }
        ApiRequest::GetBalloonHintingStatus => {
            handle_empty(vmm.handle_get_request(GetApiRequest::BalloonHintingStatus))
        }
        ApiRequest::PutBalloon => handle_empty(vmm.handle_put_request(PutApiRequest::balloon())),
        ApiRequest::PatchBalloon => {
            handle_empty(vmm.handle_patch_request(PatchApiRequest::balloon()))
        }
        ApiRequest::PatchBalloonStats => {
            handle_empty(vmm.handle_patch_request(PatchApiRequest::balloon_stats()))
        }
        ApiRequest::PatchBalloonHintingStart => {
            handle_empty(vmm.handle_patch_request(PatchApiRequest::balloon_hinting_start()))
        }
        ApiRequest::PatchBalloonHintingStop => {
            handle_empty(vmm.handle_patch_request(PatchApiRequest::balloon_hinting_stop()))
        }
        ApiRequest::GetMemoryHotplug => {
            handle_empty(vmm.handle_get_request(GetApiRequest::HotplugMemory))
        }
        ApiRequest::PutMemoryHotplug => {
            handle_empty(vmm.handle_put_request(PutApiRequest::memory_hotplug()))
        }
        ApiRequest::PatchMemoryHotplug => {
            handle_empty(vmm.handle_patch_request(PatchApiRequest::memory_hotplug()))
        }
        ApiRequest::PutEntropy(config) => handle_empty(vmm.handle_action(VmmAction::PutEntropy(
            entropy_config_input_from_request(config.as_ref()),
        ))),
        ApiRequest::PutPmem => handle_empty(vmm.handle_put_request(PutApiRequest::pmem())),
        ApiRequest::PatchPmem => handle_empty(vmm.handle_patch_request(PatchApiRequest::pmem())),
        ApiRequest::PutSnapshotCreate => handle_empty(vmm.handle_action(VmmAction::CreateSnapshot)),
        ApiRequest::PutSnapshotLoad(_) => handle_empty(vmm.handle_action(VmmAction::LoadSnapshot)),
        ApiRequest::PutVsock(config) => handle_empty(vmm.handle_put_request(PutApiRequest::vsock(
            vsock_config_input_from_request(config.as_ref()),
        ))),
    }
}

fn record_deprecated_api_usage(request: &ApiRequest, vmm: &mut impl VmmRequestHandler) {
    if request_uses_deprecated_api(request) {
        vmm.record_deprecated_api_call();
    }
}

fn request_uses_deprecated_api(request: &ApiRequest) -> bool {
    match request {
        ApiRequest::PutMachineConfig(config) => config.cpu_template().is_some(),
        ApiRequest::PatchMachineConfig(config) => config.cpu_template().is_some(),
        ApiRequest::PutMmdsConfig(config) => config.version() == ApiMmdsVersion::V1,
        ApiRequest::PutSnapshotLoad(config) => config.deprecated_fields_used(),
        ApiRequest::PutVsock(config) => config.vsock_id().is_some(),
        ApiRequest::GetInstanceInfo
        | ApiRequest::GetBalloon
        | ApiRequest::GetBalloonStats
        | ApiRequest::GetBalloonHintingStatus
        | ApiRequest::GetMachineConfig
        | ApiRequest::GetMemoryHotplug
        | ApiRequest::GetMmds
        | ApiRequest::GetVmConfig
        | ApiRequest::GetVersion
        | ApiRequest::HotUnplugDevice(_)
        | ApiRequest::PatchBalloon
        | ApiRequest::PatchBalloonStats
        | ApiRequest::PatchBalloonHintingStart
        | ApiRequest::PatchBalloonHintingStop
        | ApiRequest::PatchDrive(_)
        | ApiRequest::PatchMemoryHotplug
        | ApiRequest::PatchMmds(_)
        | ApiRequest::PatchNetworkInterface(_)
        | ApiRequest::PatchPmem
        | ApiRequest::PatchVmState(_)
        | ApiRequest::PutAction(_)
        | ApiRequest::PutBalloon
        | ApiRequest::PutBootSource(_)
        | ApiRequest::PutCpuConfig(_)
        | ApiRequest::PutDrive(_)
        | ApiRequest::PutEntropy(_)
        | ApiRequest::PutLogger(_)
        | ApiRequest::PutMemoryHotplug
        | ApiRequest::PutMetrics(_)
        | ApiRequest::PutMmds(_)
        | ApiRequest::PutNetworkInterface(_)
        | ApiRequest::PutPmem
        | ApiRequest::PutSerial(_)
        | ApiRequest::PutSnapshotCreate => false,
    }
}

fn action_from_request(action: &ActionRequest) -> VmmAction {
    match action.action_type() {
        ActionType::InstanceStart => VmmAction::InstanceStart,
        ActionType::FlushMetrics => VmmAction::FlushMetrics,
    }
}

fn vm_state_action_from_request(update: &VmStateUpdateRequest) -> VmmAction {
    match update.state() {
        VmStateUpdate::Paused => VmmAction::Pause,
        VmStateUpdate::Resumed => VmmAction::Resume,
    }
}

fn hot_unplug_action_from_request(request: &HotUnplugDeviceRequest) -> VmmAction {
    VmmAction::HotUnplugDevice(HotUnplugDeviceInput::new(
        hot_unplug_kind_from_request(request.kind()),
        request.id(),
    ))
}

fn hot_unplug_kind_from_request(kind: ApiHotUnplugDeviceKind) -> RuntimeHotUnplugDeviceKind {
    match kind {
        ApiHotUnplugDeviceKind::Drive => RuntimeHotUnplugDeviceKind::Drive,
        ApiHotUnplugDeviceKind::NetworkInterface => RuntimeHotUnplugDeviceKind::NetworkInterface,
        ApiHotUnplugDeviceKind::Pmem => RuntimeHotUnplugDeviceKind::Pmem,
    }
}

pub(crate) fn config_vmm_action_from_api_request(request: ApiRequest) -> Option<VmmAction> {
    match request {
        ApiRequest::PutBootSource(config) => Some(VmmAction::PutBootSource(
            boot_source_input_from_request(config.as_ref()),
        )),
        ApiRequest::PutCpuConfig(config) => Some(VmmAction::PutCpuConfig(
            cpu_config_input_from_request(config.as_ref()),
        )),
        ApiRequest::PutDrive(config) => Some(VmmAction::PutDrive(drive_config_input_from_request(
            config.as_ref(),
        ))),
        ApiRequest::PutEntropy(config) => Some(VmmAction::PutEntropy(
            entropy_config_input_from_request(config.as_ref()),
        )),
        ApiRequest::PutLogger(config) => Some(VmmAction::PutLogger(
            logger_config_input_from_request(config.as_ref()),
        )),
        ApiRequest::PutMachineConfig(config) => Some(VmmAction::PutMachineConfig(
            machine_config_input_from_request(config.as_ref()),
        )),
        ApiRequest::PutMetrics(config) => Some(VmmAction::PutMetrics(
            metrics_config_input_from_request(config.as_ref()),
        )),
        ApiRequest::PutMmdsConfig(config) => Some(VmmAction::PutMmdsConfig(
            mmds_config_input_from_request(config.as_ref()),
        )),
        ApiRequest::PutNetworkInterface(config) => Some(VmmAction::PutNetworkInterface(
            network_interface_config_input_from_request(config.as_ref()),
        )),
        ApiRequest::PutSerial(config) => Some(VmmAction::PutSerial(
            serial_config_input_from_request(config.as_ref()),
        )),
        ApiRequest::PutVsock(config) => Some(VmmAction::PutVsock(vsock_config_input_from_request(
            config.as_ref(),
        ))),
        ApiRequest::GetInstanceInfo
        | ApiRequest::GetBalloon
        | ApiRequest::GetBalloonStats
        | ApiRequest::GetBalloonHintingStatus
        | ApiRequest::GetMachineConfig
        | ApiRequest::GetMemoryHotplug
        | ApiRequest::GetMmds
        | ApiRequest::GetVmConfig
        | ApiRequest::GetVersion
        | ApiRequest::HotUnplugDevice(_)
        | ApiRequest::PatchBalloon
        | ApiRequest::PatchBalloonStats
        | ApiRequest::PatchBalloonHintingStart
        | ApiRequest::PatchBalloonHintingStop
        | ApiRequest::PatchDrive(_)
        | ApiRequest::PatchMachineConfig(_)
        | ApiRequest::PatchMmds(_)
        | ApiRequest::PatchMemoryHotplug
        | ApiRequest::PatchNetworkInterface(_)
        | ApiRequest::PatchPmem
        | ApiRequest::PatchVmState(_)
        | ApiRequest::PutAction(_)
        | ApiRequest::PutBalloon
        | ApiRequest::PutMemoryHotplug
        | ApiRequest::PutMmds(_)
        | ApiRequest::PutPmem
        | ApiRequest::PutSnapshotCreate
        | ApiRequest::PutSnapshotLoad(_) => None,
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

fn cpu_config_input_from_request(config: &CpuConfigRequest) -> CpuConfigInput {
    CpuConfigInput::new(config.custom_template_configured())
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
        config
            .entropy_config()
            .map(entropy_config_response_from_runtime),
    )
}

fn entropy_config_response_from_runtime(
    _config: bangbang_runtime::entropy::EntropyConfig,
) -> EntropyConfigResponse {
    EntropyConfigResponse::new()
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
    if let Some(mtu) = config.mtu() {
        response = response.with_mtu(mtu);
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

fn machine_cpu_template_from_request(
    cpu_template: bangbang_api::http::MachineConfigCpuTemplate,
) -> RuntimeMachineConfigCpuTemplate {
    match cpu_template {
        bangbang_api::http::MachineConfigCpuTemplate::C3 => RuntimeMachineConfigCpuTemplate::C3,
        bangbang_api::http::MachineConfigCpuTemplate::T2 => RuntimeMachineConfigCpuTemplate::T2,
        bangbang_api::http::MachineConfigCpuTemplate::T2S => RuntimeMachineConfigCpuTemplate::T2S,
        bangbang_api::http::MachineConfigCpuTemplate::T2CL => RuntimeMachineConfigCpuTemplate::T2CL,
        bangbang_api::http::MachineConfigCpuTemplate::T2A => RuntimeMachineConfigCpuTemplate::T2A,
        bangbang_api::http::MachineConfigCpuTemplate::V1N1 => RuntimeMachineConfigCpuTemplate::V1N1,
        bangbang_api::http::MachineConfigCpuTemplate::None => RuntimeMachineConfigCpuTemplate::None,
    }
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
        input = input.with_cpu_template(machine_cpu_template_from_request(cpu_template));
    }

    input
}

fn machine_config_patch_input_from_request(
    config: &MachineConfigPatchRequest,
) -> MachineConfigPatchInput {
    let mut input = MachineConfigPatchInput::new();

    if let Some(vcpu_count) = config.vcpu_count() {
        input = input.with_vcpu_count(vcpu_count);
    }
    if let Some(mem_size_mib) = config.mem_size_mib() {
        input = input.with_mem_size_mib(mem_size_mib);
    }
    if let Some(smt) = config.smt() {
        input = input.with_smt(smt);
    }
    if let Some(cpu_template) = config.cpu_template() {
        input = input.with_cpu_template(machine_cpu_template_from_request(cpu_template));
    }
    if let Some(track_dirty_pages) = config.track_dirty_pages() {
        input = input.with_track_dirty_pages(track_dirty_pages);
    }
    if let Some(huge_pages) = config.huge_pages() {
        input = input.with_huge_pages(match huge_pages {
            bangbang_api::http::MachineConfigHugePages::None => RuntimeMachineConfigHugePages::None,
            bangbang_api::http::MachineConfigHugePages::TwoM => RuntimeMachineConfigHugePages::TwoM,
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

fn drive_update_input_from_request(config: &DrivePatchRequest) -> DriveUpdateInput {
    let mut input = DriveUpdateInput::new(
        config.path_drive_id(),
        config.body_drive_id(),
        config.path_on_host().map(std::path::PathBuf::from),
    );

    if config.rate_limiter_configured() {
        input = input.with_rate_limiter_configured();
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
    if let Some(mtu) = config.mtu() {
        input = input.with_mtu(mtu);
    }
    if config.rx_rate_limiter_configured() {
        input = input.with_rx_rate_limiter_configured();
    }
    if config.tx_rate_limiter_configured() {
        input = input.with_tx_rate_limiter_configured();
    }

    input
}

fn network_interface_update_input_from_request(
    config: &NetworkInterfacePatchRequest,
) -> NetworkInterfaceUpdateInput {
    NetworkInterfaceUpdateInput::new(config.path_iface_id(), config.body_iface_id())
}

fn serial_config_input_from_request(config: &SerialConfigRequest) -> SerialConfigInput {
    let mut input = SerialConfigInput::new();

    if let Some(serial_out_path) = config.serial_out_path() {
        input = input.with_serial_out_path(serial_out_path);
    }
    if config.rate_limiter_configured() {
        input = input.with_rate_limiter_configured();
    }

    input
}

fn entropy_config_input_from_request(config: &EntropyConfigRequest) -> EntropyConfigInput {
    let mut input = EntropyConfigInput::new();

    if config.rate_limiter_configured() {
        input = input.with_rate_limiter_configured();
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

fn read_request_with_limit(
    stream: &mut UnixStream,
    timeout: Duration,
    http_api_max_payload_size: usize,
) -> Result<RequestRead, ApiServerError> {
    let deadline = Instant::now() + timeout;
    let mut now = Instant::now;

    read_request_until_with_limit(stream, deadline, &mut now, http_api_max_payload_size)
}

#[cfg(test)]
fn read_request_until(
    stream: &mut UnixStream,
    deadline: Instant,
    now: &mut impl FnMut() -> Instant,
) -> Result<RequestRead, ApiServerError> {
    read_request_until_with_limit(stream, deadline, now, HTTP_MAX_PAYLOAD_SIZE)
}

fn read_request_until_with_limit(
    stream: &mut UnixStream,
    deadline: Instant,
    now: &mut impl FnMut() -> Instant,
    http_api_max_payload_size: usize,
) -> Result<RequestRead, ApiServerError> {
    let mut request = Vec::new();
    let mut chunk = [0; READ_CHUNK_SIZE];

    loop {
        match request_total_len_with_limit(&request, http_api_max_payload_size) {
            Ok(Some(total_len)) if request.len() >= total_len => {
                request.truncate(total_len);
                return Ok(RequestRead::Complete(request));
            }
            Ok(Some(_)) | Ok(None) => {}
            Err(RequestError::PayloadTooLarge) => return Ok(RequestRead::TooLarge),
            Err(_) => return Ok(RequestRead::Complete(request)),
        }

        let remaining = http_api_max_payload_size.saturating_sub(request.len());
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
    use std::os::unix::io::{AsRawFd, RawFd};
    use std::os::unix::net::UnixStream;
    use std::sync::{Arc, Barrier, Mutex};
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use bangbang_runtime::block::DriveUpdateError;
    use bangbang_runtime::logger::{LoggerConfigInput, LoggerWriteError};
    use bangbang_runtime::machine::MAX_MEM_SIZE_MIB;
    use bangbang_runtime::metrics::{
        BootRunLoopMetricStatus, MetricsConfigInput, MetricsDiagnostics,
    };
    use bangbang_runtime::{BackendError, VmmActionError};

    use crate::vmm::{
        InstanceStartExecutor, ProcessSessionDiagnostics, ProcessSessionExitStatus, ProcessVmm,
    };

    use super::*;

    const VERSION: &str = "0.1.0";

    #[derive(Debug, Clone)]
    struct TestInstanceStarter {
        result: Result<TestSession, BackendError>,
    }

    #[derive(Debug, Clone)]
    struct TestSession {
        boot_run_loop_status: Option<BootRunLoopMetricStatus>,
        process_exit_signal: Option<TestProcessExitSignal>,
        drive_update_result: Option<DriveUpdateError>,
    }

    impl TestSession {
        const fn without_boot_run_loop_status() -> Self {
            Self {
                boot_run_loop_status: None,
                process_exit_signal: None,
                drive_update_result: None,
            }
        }

        const fn with_boot_run_loop_status(status: BootRunLoopMetricStatus) -> Self {
            Self {
                boot_run_loop_status: Some(status),
                process_exit_signal: None,
                drive_update_result: None,
            }
        }

        fn with_process_exit_signal(signal: TestProcessExitSignal) -> Self {
            Self {
                boot_run_loop_status: None,
                process_exit_signal: Some(signal),
                drive_update_result: None,
            }
        }
    }

    impl ProcessSessionDiagnostics for TestSession {
        fn metrics_diagnostics(&self) -> MetricsDiagnostics {
            self.boot_run_loop_status
                .map(|status| MetricsDiagnostics::new().with_boot_run_loop_status(status))
                .unwrap_or_default()
        }

        fn update_block_device(&mut self, _config: &DriveConfig) -> Result<(), DriveUpdateError> {
            match self.drive_update_result.clone() {
                Some(err) => Err(err),
                None => Ok(()),
            }
        }

        fn process_exit_wakeup_fd(&self) -> Option<RawFd> {
            self.process_exit_signal
                .as_ref()
                .map(TestProcessExitSignal::wakeup_fd)
        }

        fn drain_process_exit_wakeup(&mut self) -> Result<(), std::io::ErrorKind> {
            if let Some(signal) = self.process_exit_signal.as_mut() {
                signal.drain()?;
            }

            Ok(())
        }

        fn process_exit_status(&self) -> ProcessSessionExitStatus {
            self.process_exit_signal
                .as_ref()
                .map(TestProcessExitSignal::status)
                .unwrap_or_default()
        }
    }

    #[derive(Debug, Clone)]
    struct TestProcessExitSignal {
        reader: Arc<Mutex<UnixStream>>,
        writer: Arc<Mutex<UnixStream>>,
        reader_fd: RawFd,
        status: Arc<Mutex<ProcessSessionExitStatus>>,
    }

    impl TestProcessExitSignal {
        fn new() -> Self {
            let (reader, writer) =
                UnixStream::pair().expect("test process-exit signal should be created");
            reader
                .set_nonblocking(true)
                .expect("test process-exit reader should be nonblocking");
            let reader_fd = reader.as_raw_fd();

            Self {
                reader: Arc::new(Mutex::new(reader)),
                writer: Arc::new(Mutex::new(writer)),
                reader_fd,
                status: Arc::new(Mutex::new(ProcessSessionExitStatus::Running)),
            }
        }

        const fn wakeup_fd(&self) -> RawFd {
            self.reader_fd
        }

        fn status(&self) -> ProcessSessionExitStatus {
            *self
                .status
                .lock()
                .expect("test process-exit status should lock")
        }

        fn trigger(&self, status: ProcessSessionExitStatus) {
            *self
                .status
                .lock()
                .expect("test process-exit status should lock") = status;
            self.writer
                .lock()
                .expect("test process-exit writer should lock")
                .write_all(&[1])
                .expect("test process-exit signal should write");
        }

        fn drain(&mut self) -> Result<(), std::io::ErrorKind> {
            let mut reader = self
                .reader
                .lock()
                .expect("test process-exit reader should lock");
            let mut buffer = [0; 64];

            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => return Ok(()),
                    Ok(_) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(err) => return Err(err.kind()),
                }
            }
        }
    }

    impl TestInstanceStarter {
        const fn success() -> Self {
            Self {
                result: Ok(TestSession::without_boot_run_loop_status()),
            }
        }

        const fn success_with_boot_run_loop_status(status: BootRunLoopMetricStatus) -> Self {
            Self {
                result: Ok(TestSession::with_boot_run_loop_status(status)),
            }
        }

        fn success_with_process_exit_signal(signal: TestProcessExitSignal) -> Self {
            Self {
                result: Ok(TestSession::with_process_exit_signal(signal)),
            }
        }

        const fn failure() -> Self {
            Self {
                result: Err(BackendError::InvalidState("test startup failed")),
            }
        }
    }

    impl InstanceStartExecutor for TestInstanceStarter {
        type Session = TestSession;

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

    fn test_controller_with_mmds_data_store_limit(
        mmds_data_store_limit_bytes: usize,
    ) -> ProcessVmm<TestInstanceStarter> {
        test_controller_with_starter_and_mmds_data_store_limit(
            TestInstanceStarter::failure(),
            mmds_data_store_limit_bytes,
        )
    }

    fn test_controller_with_starter_and_mmds_data_store_limit(
        starter: TestInstanceStarter,
        mmds_data_store_limit_bytes: usize,
    ) -> ProcessVmm<TestInstanceStarter> {
        ProcessVmm::with_starter_and_mmds_data_store_limit(
            "demo-1",
            VERSION,
            "bangbang",
            starter,
            mmds_data_store_limit_bytes,
        )
    }

    #[test]
    fn handle_empty_maps_logger_write_errors_to_fault() {
        let response = handle_empty(Err(VmmActionError::LoggerWrite(LoggerWriteError::Write(
            std::io::ErrorKind::BrokenPipe,
        ))));

        assert_eq!(
            response.body(),
            r#"{"fault_message":"failed to write logger output: BrokenPipe"}"#
        );
    }

    fn unique_socket_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        env::temp_dir().join(format!("bb-{name}-{}-{nanos}.sock", std::process::id()))
    }

    fn request_over_socket(
        vmm: &mut impl VmmRequestHandler,
        socket_name: &str,
        request: &str,
    ) -> String {
        let path = unique_socket_path(socket_name);
        let server = ApiServer::bind(&path).expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");

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

    fn request_with_body(method: &str, path: &str, body: &str) -> String {
        format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    fn read_metrics_json(path: &Path) -> serde_json::Value {
        let output = fs::read_to_string(path).expect("metrics output should be readable");
        serde_json::from_str(output.trim_end()).expect("metrics output should be JSON")
    }

    fn assert_metric(metrics: &serde_json::Value, group: &str, field: &str, expected: u64) {
        let actual = metrics
            .get(group)
            .and_then(|group| group.get(field))
            .and_then(serde_json::Value::as_u64)
            .expect("metric should be present");
        assert_eq!(actual, expected, "{group}.{field}");
    }

    fn put_action_over_socket(
        vmm: &mut impl VmmRequestHandler,
        socket_name: &str,
        action_type: &str,
    ) -> String {
        let body = format!(r#"{{"action_type":"{action_type}"}}"#);
        let request = format!(
            "PUT /actions HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        request_over_socket(vmm, socket_name, &request)
    }

    struct ShutdownAfterPeriodicFlush<S>
    where
        S: InstanceStartExecutor,
    {
        inner: ProcessVmm<S>,
        shutdown_writer: UnixStream,
    }

    impl<S> ShutdownAfterPeriodicFlush<S>
    where
        S: InstanceStartExecutor,
    {
        fn new(inner: ProcessVmm<S>, shutdown_writer: &UnixStream) -> Self {
            Self {
                inner,
                shutdown_writer: shutdown_writer
                    .try_clone()
                    .expect("shutdown writer should clone"),
            }
        }
    }

    impl<S> VmmRequestHandler for ShutdownAfterPeriodicFlush<S>
    where
        S: InstanceStartExecutor,
    {
        fn handle_action(&mut self, action: VmmAction) -> Result<VmmData, VmmActionError> {
            self.inner.handle_action(action)
        }

        fn handle_get_request(
            &mut self,
            request: GetApiRequest,
        ) -> Result<VmmData, VmmActionError> {
            self.inner.handle_get_request(request)
        }

        fn handle_patch_request(
            &mut self,
            request: PatchApiRequest,
        ) -> Result<VmmData, VmmActionError> {
            self.inner.handle_patch_request(request)
        }

        fn handle_put_request(
            &mut self,
            request: PutApiRequest,
        ) -> Result<VmmData, VmmActionError> {
            self.inner.handle_put_request(request)
        }

        fn record_api_request_parse_failure(&mut self, request: ApiRequestMetricParseFailure) {
            self.inner.record_api_request_parse_failure(request);
        }

        fn record_put_actions_request(&mut self) {
            self.inner.record_put_actions_request();
        }

        fn handle_put_action_request(
            &mut self,
            action: VmmAction,
        ) -> Result<VmmData, VmmActionError> {
            self.inner.handle_put_action_request(action)
        }

        fn record_deprecated_api_call(&mut self) {
            self.inner.record_deprecated_api_call();
        }

        fn handle_periodic_metrics_flush(&mut self) -> Result<bool, VmmActionError> {
            let result = self.inner.handle_periodic_metrics_flush();
            self.shutdown_writer
                .write_all(b"x")
                .expect("periodic flush test should signal shutdown");
            result
        }

        fn process_exit_wakeup_fd(&self) -> Option<RawFd> {
            self.inner.process_exit_wakeup_fd()
        }

        fn drain_process_exit_wakeup(&mut self) -> Result<(), std::io::ErrorKind> {
            self.inner.drain_process_exit_wakeup()
        }

        fn process_exit_status(&self) -> ProcessSessionExitStatus {
            self.inner.process_exit_status()
        }
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

        let patch_body = r#"{"mem_size_mib":512}"#;
        let patch_request = format!(
            "PATCH /machine-config HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{patch_body}",
            patch_body.len()
        );

        let patch_response = handle_request_bytes(patch_request.as_bytes(), &mut vmm);

        assert_eq!(
            patch_response.status(),
            bangbang_api::http::StatusCode::NoContent
        );
        assert_eq!(patch_response.body(), "");
        assert_eq!(vmm.machine_config().vcpu_count(), 2);
        assert_eq!(vmm.machine_config().mem_size_mib(), 512);

        let get_response = handle_request_bytes(
            b"GET /machine-config HTTP/1.1\r\nHost: localhost\r\n\r\n",
            &mut vmm,
        );

        assert!(get_response.body().contains(r#""vcpu_count":2"#));
        assert!(get_response.body().contains(r#""mem_size_mib":512"#));

        let vm_config_response = handle_request_bytes(
            b"GET /vm/config HTTP/1.1\r\nHost: localhost\r\n\r\n",
            &mut vmm,
        );

        assert!(vm_config_response.body().contains(r#""machine-config":"#));
        assert!(vm_config_response.body().contains(r#""vcpu_count":2"#));
        assert!(vm_config_response.body().contains(r#""mem_size_mib":512"#));
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
    fn empty_mmds_config_network_interface_list_reaches_runtime_without_mutating() {
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
            r#"{"fault_message":"MMDS network_interfaces must not be empty"}"#
        );
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::NotStarted
        );
        let vm_config_response = handle_request_bytes(
            b"GET /vm/config HTTP/1.1\r\nHost: localhost\r\n\r\n",
            &mut vmm,
        );
        assert_eq!(
            vm_config_response.status(),
            bangbang_api::http::StatusCode::Ok
        );
        assert!(!vm_config_response.body().contains(r#""mmds-config":"#));
        assert!(vmm.boot_source_config().is_none());
        assert!(vmm.drive_configs().is_empty());
    }

    #[test]
    fn empty_mmds_config_network_interface_list_faults_over_unix_socket() {
        let mut vmm = test_controller();
        let body = r#"{"network_interfaces":[]}"#;
        let request = format!(
            "PUT /mmds/config HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        let response = request_over_socket(&mut vmm, "mmds-empty", &request);

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(response.contains("Content-Type: application/json\r\n"));
        assert!(
            response.contains(r#"{"fault_message":"MMDS network_interfaces must not be empty"}"#)
        );
        let vm_config_response = handle_request_bytes(
            b"GET /vm/config HTTP/1.1\r\nHost: localhost\r\n\r\n",
            &mut vmm,
        );
        assert_eq!(
            vm_config_response.status(),
            bangbang_api::http::StatusCode::Ok
        );
        assert!(!vm_config_response.body().contains(r#""mmds-config":"#));
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
    fn machine_config_faults_do_not_mutate_vmm_state() {
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
            r#"{"fault_message":"machine track_dirty_pages is not supported"}"#
        );
        assert_eq!(vmm.machine_config().vcpu_count(), 2);
        assert_eq!(vmm.machine_config().mem_size_mib(), 256);
        assert!(!vmm.machine_config().track_dirty_pages());

        let oversized_mem_size_mib = MAX_MEM_SIZE_MIB + 1;
        let oversized_body =
            format!(r#"{{"vcpu_count":4,"mem_size_mib":{oversized_mem_size_mib}}}"#);
        let oversized_request = format!(
            "PUT /machine-config HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{oversized_body}",
            oversized_body.len()
        );

        let response = handle_request_bytes(oversized_request.as_bytes(), &mut vmm);

        assert_eq!(
            response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );
        assert_eq!(
            response.body(),
            format!(
                r#"{{"fault_message":"machine mem_size_mib must be in 1..={MAX_MEM_SIZE_MIB}"}}"#
            )
        );
        assert_eq!(vmm.machine_config().vcpu_count(), 2);
        assert_eq!(vmm.machine_config().mem_size_mib(), 256);
        assert!(!vmm.machine_config().track_dirty_pages());

        let invalid_patch_body = r#"{"mem_size_mib":0}"#;
        let invalid_patch_request = format!(
            "PATCH /machine-config HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{invalid_patch_body}",
            invalid_patch_body.len()
        );

        let response = handle_request_bytes(invalid_patch_request.as_bytes(), &mut vmm);

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
        assert!(!vmm.machine_config().track_dirty_pages());

        let oversized_patch_body = format!(r#"{{"mem_size_mib":{oversized_mem_size_mib}}}"#);
        let oversized_patch_request = format!(
            "PATCH /machine-config HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{oversized_patch_body}",
            oversized_patch_body.len()
        );

        let response = handle_request_bytes(oversized_patch_request.as_bytes(), &mut vmm);

        assert_eq!(
            response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );
        assert_eq!(
            response.body(),
            format!(
                r#"{{"fault_message":"machine mem_size_mib must be in 1..={MAX_MEM_SIZE_MIB}"}}"#
            )
        );
        assert_eq!(vmm.machine_config().vcpu_count(), 2);
        assert_eq!(vmm.machine_config().mem_size_mib(), 256);
        assert!(!vmm.machine_config().track_dirty_pages());

        let unsupported_patch_body = r#"{"track_dirty_pages":true}"#;
        let unsupported_patch_request = format!(
            "PATCH /machine-config HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{unsupported_patch_body}",
            unsupported_patch_body.len()
        );

        let response = handle_request_bytes(unsupported_patch_request.as_bytes(), &mut vmm);

        assert_eq!(
            response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );
        assert_eq!(
            response.body(),
            r#"{"fault_message":"machine track_dirty_pages is not supported"}"#
        );
        assert_eq!(vmm.machine_config().vcpu_count(), 2);
        assert_eq!(vmm.machine_config().mem_size_mib(), 256);
        assert!(!vmm.machine_config().track_dirty_pages());
    }

    #[test]
    fn machine_cpu_template_faults_do_not_mutate_vmm_state_over_socket() {
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

        let unsupported_put_body = r#"{"vcpu_count":4,"mem_size_mib":512,"cpu_template":"V1N1"}"#;
        let unsupported_put_request = format!(
            "PUT /machine-config HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{unsupported_put_body}",
            unsupported_put_body.len()
        );

        let response = request_over_socket(&mut vmm, "mct-put", &unsupported_put_request);

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(
            response.contains(r#"{"fault_message":"machine cpu_template V1N1 is not supported"}"#)
        );
        assert_eq!(vmm.machine_config().vcpu_count(), 2);
        assert_eq!(vmm.machine_config().mem_size_mib(), 256);
        assert_eq!(vmm.machine_config().cpu_template(), None);

        let unsupported_patch_body = r#"{"mem_size_mib":512,"cpu_template":"T2A"}"#;
        let unsupported_patch_request = format!(
            "PATCH /machine-config HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{unsupported_patch_body}",
            unsupported_patch_body.len()
        );

        let response = request_over_socket(&mut vmm, "mct-patch", &unsupported_patch_request);

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(
            response.contains(r#"{"fault_message":"machine cpu_template T2A is not supported"}"#)
        );
        assert_eq!(vmm.machine_config().vcpu_count(), 2);
        assert_eq!(vmm.machine_config().mem_size_mib(), 256);
        assert_eq!(vmm.machine_config().cpu_template(), None);
    }

    #[test]
    fn machine_smt_and_huge_pages_faults_do_not_mutate_vmm_state_over_socket() {
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

        for (method, socket_name, body, expected_fault) in [
            (
                "PUT",
                "ms-put",
                r#"{"vcpu_count":4,"mem_size_mib":512,"smt":true}"#,
                r#"{"fault_message":"machine smt is not supported"}"#,
            ),
            (
                "PATCH",
                "ms-pat",
                r#"{"mem_size_mib":512,"smt":true}"#,
                r#"{"fault_message":"machine smt is not supported"}"#,
            ),
            (
                "PUT",
                "mh-put",
                r#"{"vcpu_count":4,"mem_size_mib":512,"huge_pages":"2M"}"#,
                r#"{"fault_message":"machine huge_pages is not supported"}"#,
            ),
            (
                "PATCH",
                "mh-pat",
                r#"{"mem_size_mib":512,"huge_pages":"2M"}"#,
                r#"{"fault_message":"machine huge_pages is not supported"}"#,
            ),
        ] {
            let request = format!(
                "{method} /machine-config HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );

            let response = request_over_socket(&mut vmm, socket_name, &request);

            assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
            assert!(response.contains(expected_fault), "{method} {body}");
            assert_eq!(vmm.machine_config().vcpu_count(), 2);
            assert_eq!(vmm.machine_config().mem_size_mib(), 256);
            assert!(!vmm.machine_config().smt());
            assert_eq!(
                vmm.machine_config().huge_pages(),
                RuntimeMachineConfigHugePages::None
            );
        }
    }

    #[test]
    fn running_state_rejects_machine_config_patch_without_mutating() {
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        let machine_body = r#"{"vcpu_count":2,"mem_size_mib":256}"#;
        let machine_request = format!(
            "PUT /machine-config HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{machine_body}",
            machine_body.len()
        );
        assert_eq!(
            handle_request_bytes(machine_request.as_bytes(), &mut vmm).status(),
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
        let start_response = put_action_over_socket(&mut vmm, "mc-patch", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));

        let patch_body = r#"{"mem_size_mib":512}"#;
        let patch_request = format!(
            "PATCH /machine-config HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{patch_body}",
            patch_body.len()
        );

        let response = handle_request_bytes(patch_request.as_bytes(), &mut vmm);

        assert_eq!(
            response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );
        assert_eq!(
            response.body(),
            r#"{"fault_message":"The requested operation is not supported in Running state: PatchMachineConfig"}"#
        );
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::Running
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
    fn configured_logger_records_actions_over_api_requests() {
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        let logger_path = unique_socket_path("logger-actions").with_extension("log");
        let logger_body = format!(
            r#"{{
                "log_path": "{}",
                "level": "Info",
                "show_level": true,
                "show_log_origin": true,
                "module": "bangbang_runtime"
            }}"#,
            logger_path.to_string_lossy()
        );
        let logger_request = format!(
            "PUT /logger HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{logger_body}",
            logger_body.len()
        );
        assert_eq!(
            handle_request_bytes(logger_request.as_bytes(), &mut vmm).status(),
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

        let start_response = put_action_over_socket(&mut vmm, "start-with-logger", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));
        let flush_response = put_action_over_socket(&mut vmm, "flush-with-logger", "FlushMetrics");
        assert!(flush_response.starts_with("HTTP/1.1 204 No Content\r\n"));

        let output = fs::read_to_string(&logger_path).expect("logger output should be readable");
        let mut lines = output.lines();
        assert_action_log_with_origin(lines.next(), "InstanceStart");
        assert_action_log_with_origin(lines.next(), "FlushMetrics");
        assert_eq!(lines.next(), None);

        fs::remove_file(logger_path).expect("fixture should clean up");
    }

    #[test]
    fn configured_logger_module_filter_suppresses_actions_over_api_requests() {
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        let logger_path = unique_socket_path("log-mod").with_extension("log");
        let logger_body = format!(
            r#"{{
                "log_path": "{}",
                "level": "Info",
                "module": "api_server"
            }}"#,
            logger_path.to_string_lossy()
        );
        let logger_request = format!(
            "PUT /logger HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{logger_body}",
            logger_body.len()
        );
        assert_eq!(
            handle_request_bytes(logger_request.as_bytes(), &mut vmm).status(),
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

        let start_response = put_action_over_socket(&mut vmm, "lm-start", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));
        let flush_response = put_action_over_socket(&mut vmm, "lm-flush", "FlushMetrics");
        assert!(flush_response.starts_with("HTTP/1.1 204 No Content\r\n"));

        assert_eq!(
            fs::read_to_string(&logger_path).expect("logger output should be readable"),
            ""
        );

        fs::remove_file(logger_path).expect("fixture should clean up");
    }

    fn assert_action_log_with_origin(line: Option<&str>, action: &str) {
        let line = line.expect("logger output should include action line");
        assert!(line.starts_with("level=Info origin="));
        assert!(line.ends_with(&format!(" action={action}")));

        let suffix = format!(" action={action}");
        let origin = line
            .strip_prefix("level=Info origin=")
            .expect("logger output should include origin prefix")
            .strip_suffix(&suffix)
            .expect("logger output should include action suffix");
        let (file, line_number) = origin
            .rsplit_once(':')
            .expect("logger origin should include file and line");

        assert!(
            file.ends_with("crates/runtime/src/lib.rs"),
            "unexpected origin file: {file}"
        );
        assert!(
            line_number.parse::<u32>().is_ok(),
            "unexpected origin line: {line_number}"
        );
    }

    #[test]
    fn api_logger_update_replaces_startup_logger_before_actions() {
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        let startup_logger_path = unique_socket_path("startup-logger").with_extension("log");
        let api_logger_path = unique_socket_path("api-logger").with_extension("log");
        vmm.handle_action(VmmAction::PutLogger(
            LoggerConfigInput::new()
                .with_log_path(&startup_logger_path)
                .with_show_level(true),
        ))
        .expect("startup logger config should apply");
        let logger_body = format!(
            r#"{{
                "log_path": "{}",
                "level": "Info",
                "show_level": false
            }}"#,
            api_logger_path.to_string_lossy()
        );
        let logger_request = format!(
            "PUT /logger HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{logger_body}",
            logger_body.len()
        );
        assert_eq!(
            handle_request_bytes(logger_request.as_bytes(), &mut vmm).status(),
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

        let start_response = put_action_over_socket(&mut vmm, "api-log", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));

        assert_eq!(
            fs::read_to_string(&startup_logger_path)
                .expect("startup logger output should be readable"),
            ""
        );
        assert_eq!(
            fs::read_to_string(&api_logger_path).expect("api logger output should be readable"),
            "action=InstanceStart\n"
        );

        fs::remove_file(startup_logger_path).expect("startup fixture should clean up");
        fs::remove_file(api_logger_path).expect("api fixture should clean up");
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
            "{\"put_api_requests\":{\"actions_count\":2,\"actions_fails\":0,\"balloon_count\":0,\"balloon_fails\":0,\"boot_source_count\":1,\"boot_source_fails\":0,\"cpu_cfg_count\":0,\"cpu_cfg_fails\":0,\"drive_count\":0,\"drive_fails\":0,\"hotplug_memory_count\":0,\"hotplug_memory_fails\":0,\"logger_count\":0,\"logger_fails\":0,\"machine_cfg_count\":0,\"machine_cfg_fails\":0,\"metrics_count\":1,\"metrics_fails\":0,\"mmds_count\":0,\"mmds_fails\":0,\"network_count\":0,\"network_fails\":0,\"pmem_count\":0,\"pmem_fails\":0,\"serial_count\":0,\"serial_fails\":0,\"vsock_count\":0,\"vsock_fails\":0},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(metrics_path).expect("fixture should clean up");
    }

    #[test]
    fn configured_metrics_counts_failed_action_requests() {
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        let metrics_path = unique_socket_path("metrics-actions").with_extension("metrics");
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
            put_action_over_socket(&mut vmm, "start-before-failure", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));

        let duplicate_start_response =
            put_action_over_socket(&mut vmm, "duplicate-start", "InstanceStart");
        assert!(duplicate_start_response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        let unsupported_action_response =
            put_action_over_socket(&mut vmm, "send-ctrl-alt-del", "SendCtrlAltDel");
        assert!(unsupported_action_response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(
            unsupported_action_response
                .contains(r#"{"fault_message":"SendCtrlAltDel is not supported on aarch64."}"#)
        );
        let flush_response =
            put_action_over_socket(&mut vmm, "flush-after-failure", "FlushMetrics");

        assert!(flush_response.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert_eq!(
            fs::read_to_string(&metrics_path).expect("metrics output should be readable"),
            "{\"put_api_requests\":{\"actions_count\":4,\"actions_fails\":1,\"balloon_count\":0,\"balloon_fails\":0,\"boot_source_count\":1,\"boot_source_fails\":0,\"cpu_cfg_count\":0,\"cpu_cfg_fails\":0,\"drive_count\":0,\"drive_fails\":0,\"hotplug_memory_count\":0,\"hotplug_memory_fails\":0,\"logger_count\":0,\"logger_fails\":0,\"machine_cfg_count\":0,\"machine_cfg_fails\":0,\"metrics_count\":1,\"metrics_fails\":0,\"mmds_count\":0,\"mmds_fails\":0,\"network_count\":0,\"network_fails\":0,\"pmem_count\":0,\"pmem_fails\":0,\"serial_count\":0,\"serial_fails\":0,\"vsock_count\":0,\"vsock_fails\":0},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(metrics_path).expect("fixture should clean up");
    }

    #[test]
    fn configured_metrics_counts_get_api_requests() {
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        let metrics_path = unique_socket_path("metrics-get").with_extension("metrics");
        let metrics_body = format!(r#"{{"metrics_path":"{}"}}"#, metrics_path.to_string_lossy());
        let metrics_request = format!(
            "PUT /metrics HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{metrics_body}",
            metrics_body.len()
        );
        assert_eq!(
            handle_request_bytes(metrics_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let instance_response =
            request_over_socket(&mut vmm, "g-i", "GET / HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert!(instance_response.starts_with("HTTP/1.1 200 OK\r\n"));
        let version_response = request_over_socket(
            &mut vmm,
            "g-v",
            "GET /version HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        assert!(version_response.starts_with("HTTP/1.1 200 OK\r\n"));
        let machine_response = request_over_socket(
            &mut vmm,
            "g-m",
            "GET /machine-config HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        assert!(machine_response.starts_with("HTTP/1.1 200 OK\r\n"));
        let mmds_response = request_over_socket(
            &mut vmm,
            "g-d",
            "GET /mmds HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        assert!(mmds_response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        let vm_config_response = request_over_socket(
            &mut vmm,
            "g-c",
            "GET /vm/config HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        assert!(vm_config_response.starts_with("HTTP/1.1 200 OK\r\n"));

        let boot_body = r#"{"kernel_image_path":"/tmp/original-vmlinux"}"#;
        let boot_request = format!(
            "PUT /boot-source HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{boot_body}",
            boot_body.len()
        );
        assert_eq!(
            handle_request_bytes(boot_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );
        let start_response = put_action_over_socket(&mut vmm, "g-s", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));
        let flush_response = put_action_over_socket(&mut vmm, "g-f", "FlushMetrics");

        assert!(flush_response.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert_eq!(
            fs::read_to_string(&metrics_path).expect("metrics output should be readable"),
            "{\"get_api_requests\":{\"balloon_count\":0,\"hotplug_memory_count\":0,\"instance_info_count\":1,\"machine_cfg_count\":1,\"mmds_count\":1,\"vmm_version_count\":1},\"put_api_requests\":{\"actions_count\":2,\"actions_fails\":0,\"balloon_count\":0,\"balloon_fails\":0,\"boot_source_count\":1,\"boot_source_fails\":0,\"cpu_cfg_count\":0,\"cpu_cfg_fails\":0,\"drive_count\":0,\"drive_fails\":0,\"hotplug_memory_count\":0,\"hotplug_memory_fails\":0,\"logger_count\":0,\"logger_fails\":0,\"machine_cfg_count\":0,\"machine_cfg_fails\":0,\"metrics_count\":1,\"metrics_fails\":0,\"mmds_count\":0,\"mmds_fails\":0,\"network_count\":0,\"network_fails\":0,\"pmem_count\":0,\"pmem_fails\":0,\"serial_count\":0,\"serial_fails\":0,\"vsock_count\":0,\"vsock_fails\":0},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(metrics_path).expect("fixture should clean up");
    }

    #[test]
    fn configured_metrics_counts_balloon_api_requests() {
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        let metrics_path = unique_socket_path("metrics-balloon").with_extension("metrics");
        let metrics_body = format!(r#"{{"metrics_path":"{}"}}"#, metrics_path.to_string_lossy());
        let metrics_request = request_with_body("PUT", "/metrics", &metrics_body);
        assert_eq!(
            handle_request_bytes(metrics_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        for (socket_name, request) in [
            (
                "bg",
                "GET /balloon HTTP/1.1\r\nHost: localhost\r\n\r\n".to_string(),
            ),
            (
                "bgs",
                "GET /balloon/statistics HTTP/1.1\r\nHost: localhost\r\n\r\n".to_string(),
            ),
            (
                "bgh",
                "GET /balloon/hinting/status HTTP/1.1\r\nHost: localhost\r\n\r\n".to_string(),
            ),
        ] {
            let response = request_over_socket(&mut vmm, socket_name, &request);
            assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        }

        let valid_put_response = request_over_socket(
            &mut vmm,
            "bp",
            &request_with_body(
                "PUT",
                "/balloon",
                r#"{"amount_mib":64,"deflate_on_oom":true}"#,
            ),
        );
        assert!(valid_put_response.starts_with("HTTP/1.1 400 Bad Request\r\n"));

        let malformed_put_response =
            request_over_socket(&mut vmm, "bpm", &request_with_body("PUT", "/balloon", "{}"));
        assert!(malformed_put_response.starts_with("HTTP/1.1 400 Bad Request\r\n"));

        for (socket_name, request) in [
            (
                "bpa",
                request_with_body("PATCH", "/balloon", r#"{"amount_mib":32}"#),
            ),
            (
                "bps",
                request_with_body(
                    "PATCH",
                    "/balloon/statistics",
                    r#"{"stats_polling_interval_s":1}"#,
                ),
            ),
            (
                "bphs",
                request_with_body(
                    "PATCH",
                    "/balloon/hinting/start",
                    r#"{"acknowledge_on_stop":false}"#,
                ),
            ),
            (
                "bphx",
                "PATCH /balloon/hinting/stop HTTP/1.1\r\nHost: localhost\r\n\r\n".to_string(),
            ),
        ] {
            let response = request_over_socket(&mut vmm, socket_name, &request);
            assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        }

        let boot_body = r#"{"kernel_image_path":"/tmp/original-vmlinux"}"#;
        let boot_request = request_with_body("PUT", "/boot-source", boot_body);
        assert_eq!(
            handle_request_bytes(boot_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );
        let start_response = put_action_over_socket(&mut vmm, "bs", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));
        let flush_response = put_action_over_socket(&mut vmm, "bf", "FlushMetrics");

        assert!(flush_response.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert_eq!(
            fs::read_to_string(&metrics_path).expect("metrics output should be readable"),
            "{\"get_api_requests\":{\"balloon_count\":3,\"hotplug_memory_count\":0,\"instance_info_count\":0,\"machine_cfg_count\":0,\"mmds_count\":0,\"vmm_version_count\":0},\"patch_api_requests\":{\"balloon_count\":4,\"balloon_fails\":4,\"drive_count\":0,\"drive_fails\":0,\"hotplug_memory_count\":0,\"hotplug_memory_fails\":0,\"machine_cfg_count\":0,\"machine_cfg_fails\":0,\"mmds_count\":0,\"mmds_fails\":0,\"network_count\":0,\"network_fails\":0,\"pmem_count\":0,\"pmem_fails\":0},\"put_api_requests\":{\"actions_count\":2,\"actions_fails\":0,\"balloon_count\":1,\"balloon_fails\":1,\"boot_source_count\":1,\"boot_source_fails\":0,\"cpu_cfg_count\":0,\"cpu_cfg_fails\":0,\"drive_count\":0,\"drive_fails\":0,\"hotplug_memory_count\":0,\"hotplug_memory_fails\":0,\"logger_count\":0,\"logger_fails\":0,\"machine_cfg_count\":0,\"machine_cfg_fails\":0,\"metrics_count\":1,\"metrics_fails\":0,\"mmds_count\":0,\"mmds_fails\":0,\"network_count\":0,\"network_fails\":0,\"pmem_count\":0,\"pmem_fails\":0,\"serial_count\":0,\"serial_fails\":0,\"vsock_count\":0,\"vsock_fails\":0},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(metrics_path).expect("fixture should clean up");
    }

    #[test]
    fn configured_metrics_counts_observability_put_api_requests() {
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        let metrics_path = unique_socket_path("metrics-observability").with_extension("metrics");
        let logger_path = unique_socket_path("logger-observability").with_extension("log");
        let rejected_logger_path =
            unique_socket_path("logger-observability-rejected").with_extension("log");
        let serial_path = unique_socket_path("serial-observability").with_extension("out");
        let rejected_serial_path =
            unique_socket_path("serial-observability-rejected").with_extension("out");

        let metrics_body = format!(r#"{{"metrics_path":"{}"}}"#, metrics_path.to_string_lossy());
        let metrics_request = format!(
            "PUT /metrics HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{metrics_body}",
            metrics_body.len()
        );
        let metrics_response = request_over_socket(&mut vmm, "o-m1", &metrics_request);
        assert!(metrics_response.starts_with("HTTP/1.1 204 No Content\r\n"));

        let duplicate_metrics_path =
            unique_socket_path("metrics-observability-duplicate").with_extension("metrics");
        let duplicate_metrics_body = format!(
            r#"{{"metrics_path":"{}"}}"#,
            duplicate_metrics_path.to_string_lossy()
        );
        let duplicate_metrics_request = format!(
            "PUT /metrics HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{duplicate_metrics_body}",
            duplicate_metrics_body.len()
        );
        let duplicate_metrics_response =
            request_over_socket(&mut vmm, "o-m2", &duplicate_metrics_request);
        assert!(duplicate_metrics_response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(!duplicate_metrics_path.exists());

        let logger_body = format!(r#"{{"log_path":"{}"}}"#, logger_path.to_string_lossy());
        let logger_request = format!(
            "PUT /logger HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{logger_body}",
            logger_body.len()
        );
        let logger_response = request_over_socket(&mut vmm, "o-l1", &logger_request);
        assert!(logger_response.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert!(logger_path.exists());

        let serial_body = format!(
            r#"{{"serial_out_path":"{}"}}"#,
            serial_path.to_string_lossy()
        );
        let serial_request = format!(
            "PUT /serial HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{serial_body}",
            serial_body.len()
        );
        let serial_response = request_over_socket(&mut vmm, "o-s1", &serial_request);
        assert!(serial_response.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert!(!serial_path.exists());

        let boot_body = r#"{"kernel_image_path":"/tmp/original-vmlinux"}"#;
        let boot_request = format!(
            "PUT /boot-source HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{boot_body}",
            boot_body.len()
        );
        assert_eq!(
            handle_request_bytes(boot_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );
        let start_response = put_action_over_socket(&mut vmm, "o-a1", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));

        let rejected_logger_body = format!(
            r#"{{"log_path":"{}"}}"#,
            rejected_logger_path.to_string_lossy()
        );
        let rejected_logger_request = format!(
            "PUT /logger HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{rejected_logger_body}",
            rejected_logger_body.len()
        );
        let rejected_logger_response =
            request_over_socket(&mut vmm, "o-l2", &rejected_logger_request);
        assert!(rejected_logger_response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(!rejected_logger_path.exists());

        let rejected_serial_body = format!(
            r#"{{"serial_out_path":"{}"}}"#,
            rejected_serial_path.to_string_lossy()
        );
        let rejected_serial_request = format!(
            "PUT /serial HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{rejected_serial_body}",
            rejected_serial_body.len()
        );
        let rejected_serial_response =
            request_over_socket(&mut vmm, "o-s2", &rejected_serial_request);
        assert!(rejected_serial_response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(!rejected_serial_path.exists());

        let flush_response = put_action_over_socket(&mut vmm, "o-a2", "FlushMetrics");
        assert!(flush_response.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert_eq!(
            fs::read_to_string(&metrics_path).expect("metrics output should be readable"),
            "{\"put_api_requests\":{\"actions_count\":2,\"actions_fails\":0,\"balloon_count\":0,\"balloon_fails\":0,\"boot_source_count\":1,\"boot_source_fails\":0,\"cpu_cfg_count\":0,\"cpu_cfg_fails\":0,\"drive_count\":0,\"drive_fails\":0,\"hotplug_memory_count\":0,\"hotplug_memory_fails\":0,\"logger_count\":2,\"logger_fails\":1,\"machine_cfg_count\":0,\"machine_cfg_fails\":0,\"metrics_count\":2,\"metrics_fails\":1,\"mmds_count\":0,\"mmds_fails\":0,\"network_count\":0,\"network_fails\":0,\"pmem_count\":0,\"pmem_fails\":0,\"serial_count\":2,\"serial_fails\":1,\"vsock_count\":0,\"vsock_fails\":0},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(metrics_path).expect("metrics fixture should clean up");
        fs::remove_file(logger_path).expect("logger fixture should clean up");
    }

    #[test]
    fn configured_metrics_counts_observability_parser_failures() {
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        let metrics_path =
            unique_socket_path("metrics-observability-parser").with_extension("metrics");
        let rejected_metrics_path =
            unique_socket_path("metrics-observability-parser-rejected").with_extension("metrics");
        let rejected_logger_path =
            unique_socket_path("logger-observability-parser-rejected").with_extension("log");
        let rejected_serial_path =
            unique_socket_path("serial-observability-parser-rejected").with_extension("out");

        let metrics_body = format!(r#"{{"metrics_path":"{}"}}"#, metrics_path.to_string_lossy());
        let metrics_response = request_over_socket(
            &mut vmm,
            "op-m0",
            &request_with_body("PUT", "/metrics", &metrics_body),
        );
        assert!(metrics_response.starts_with("HTTP/1.1 204 No Content\r\n"));

        let malformed_metrics_body = format!(
            r#"{{"metrics_path":"{}","unknown":true}}"#,
            rejected_metrics_path.to_string_lossy()
        );
        let malformed_metrics_response = request_over_socket(
            &mut vmm,
            "op-m1",
            &request_with_body("PUT", "/metrics", &malformed_metrics_body),
        );
        assert!(malformed_metrics_response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(
            malformed_metrics_response.contains(r#"{"fault_message":"Malformed HTTP request."}"#)
        );
        assert!(!rejected_metrics_path.exists());

        let malformed_logger_body = format!(
            r#"{{"log_path":"{}","unknown":true}}"#,
            rejected_logger_path.to_string_lossy()
        );
        let malformed_logger_response = request_over_socket(
            &mut vmm,
            "op-l1",
            &request_with_body("PUT", "/logger", &malformed_logger_body),
        );
        assert!(malformed_logger_response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(
            malformed_logger_response.contains(r#"{"fault_message":"Malformed HTTP request."}"#)
        );
        assert!(!rejected_logger_path.exists());

        let malformed_serial_body = format!(
            r#"{{"serial_out_path":"{}","unknown":true}}"#,
            rejected_serial_path.to_string_lossy()
        );
        let malformed_serial_response = request_over_socket(
            &mut vmm,
            "op-s1",
            &request_with_body("PUT", "/serial", &malformed_serial_body),
        );
        assert!(malformed_serial_response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(
            malformed_serial_response.contains(r#"{"fault_message":"Malformed HTTP request."}"#)
        );
        assert!(!rejected_serial_path.exists());
        assert_eq!(vmm.serial_config().serial_out_path(), None);

        let boot_body = r#"{"kernel_image_path":"/tmp/original-vmlinux"}"#;
        let boot_response = request_over_socket(
            &mut vmm,
            "op-boot",
            &request_with_body("PUT", "/boot-source", boot_body),
        );
        assert!(boot_response.starts_with("HTTP/1.1 204 No Content\r\n"));
        let start_response = put_action_over_socket(&mut vmm, "op-a1", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));
        let flush_response = put_action_over_socket(&mut vmm, "op-a2", "FlushMetrics");
        assert!(flush_response.starts_with("HTTP/1.1 204 No Content\r\n"));

        assert_eq!(
            fs::read_to_string(&metrics_path).expect("metrics output should be readable"),
            "{\"put_api_requests\":{\"actions_count\":2,\"actions_fails\":0,\"balloon_count\":0,\"balloon_fails\":0,\"boot_source_count\":1,\"boot_source_fails\":0,\"cpu_cfg_count\":0,\"cpu_cfg_fails\":0,\"drive_count\":0,\"drive_fails\":0,\"hotplug_memory_count\":0,\"hotplug_memory_fails\":0,\"logger_count\":1,\"logger_fails\":1,\"machine_cfg_count\":0,\"machine_cfg_fails\":0,\"metrics_count\":2,\"metrics_fails\":1,\"mmds_count\":0,\"mmds_fails\":0,\"network_count\":0,\"network_fails\":0,\"pmem_count\":0,\"pmem_fails\":0,\"serial_count\":1,\"serial_fails\":1,\"vsock_count\":0,\"vsock_fails\":0},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );
        assert!(!rejected_metrics_path.exists());

        fs::remove_file(metrics_path).expect("metrics fixture should clean up");
    }

    #[test]
    fn configured_metrics_counts_core_parser_failures() {
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        let metrics_path = unique_socket_path("metrics-core-parser").with_extension("metrics");

        let metrics_body = format!(r#"{{"metrics_path":"{}"}}"#, metrics_path.to_string_lossy());
        let metrics_response = handle_request_bytes(
            request_with_body("PUT", "/metrics", &metrics_body).as_bytes(),
            &mut vmm,
        );
        assert_eq!(
            metrics_response.status(),
            bangbang_api::http::StatusCode::NoContent
        );

        for (method, path, body) in [
            ("PUT", "/actions", "not-json"),
            ("PUT", "/boot-source", "{}"),
            ("PUT", "/cpu-config", "not-json"),
            ("PUT", "/drives/data", "not-json"),
            ("PUT", "/hotplug/memory", "not-json"),
            ("PUT", "/logger", "not-json"),
            ("PUT", "/machine-config", "not-json"),
            ("PUT", "/mmds", "not-json"),
            ("PUT", "/mmds/config", "not-json"),
            ("PUT", "/network-interfaces/eth0", "not-json"),
            ("PUT", "/pmem/pmem0", "not-json"),
            ("PUT", "/serial", "not-json"),
            ("PUT", "/vsock", "not-json"),
            ("PATCH", "/drives/data", "not-json"),
            ("PATCH", "/hotplug/memory", "not-json"),
            ("PATCH", "/machine-config", "not-json"),
            ("PATCH", "/mmds", "not-json"),
            ("PATCH", "/network-interfaces/eth0", "not-json"),
            ("PATCH", "/pmem/pmem0", "not-json"),
        ] {
            let response =
                handle_request_bytes(request_with_body(method, path, body).as_bytes(), &mut vmm);
            assert_eq!(
                response.status(),
                bangbang_api::http::StatusCode::BadRequest,
                "{method} {path}"
            );
            assert_eq!(
                response.body(),
                r#"{"fault_message":"Malformed HTTP request."}"#,
                "{method} {path}"
            );
        }

        for (method, path, body, fault_message) in [
            (
                "PUT",
                "/drives/data",
                r#"{"drive_id":"other","path_on_host":"/tmp/drive.img","is_root_device":false}"#,
                "path drive_id must match body drive_id.",
            ),
            (
                "PUT",
                "/network-interfaces/eth0",
                r#"{"iface_id":"eth1","host_dev_name":"vmnet:shared"}"#,
                "path iface_id must match body iface_id.",
            ),
            (
                "PUT",
                "/pmem/pmem0",
                r#"{"id":"other","path_on_host":"/tmp/pmem.img"}"#,
                "path pmem id must match body id.",
            ),
            (
                "PATCH",
                "/drives/data",
                r#"{"drive_id":"other","path_on_host":"/tmp/drive.img"}"#,
                "path drive_id must match body drive_id.",
            ),
            (
                "PATCH",
                "/network-interfaces/eth0",
                r#"{"iface_id":"eth1"}"#,
                "path iface_id must match body iface_id.",
            ),
            (
                "PATCH",
                "/pmem/pmem0",
                r#"{"id":"other"}"#,
                "path pmem id must match body id.",
            ),
        ] {
            let response =
                handle_request_bytes(request_with_body(method, path, body).as_bytes(), &mut vmm);
            assert_eq!(
                response.status(),
                bangbang_api::http::StatusCode::BadRequest,
                "{method} {path}"
            );
            assert_eq!(
                response.body(),
                format!(r#"{{"fault_message":"{fault_message}"}}"#),
                "{method} {path}"
            );
        }

        for (method, path, body) in [
            ("DELETE", "/drives/data", "{}"),
            ("PUT", "/balloon", "not-json"),
            ("PUT", "/entropy", "not-json"),
            ("PATCH", "/balloon", "not-json"),
            ("PATCH", "/vm", "not-json"),
        ] {
            let response =
                handle_request_bytes(request_with_body(method, path, body).as_bytes(), &mut vmm);
            assert_eq!(
                response.status(),
                bangbang_api::http::StatusCode::BadRequest,
                "{method} {path}"
            );
        }

        let oversized_boot_request = request_with_body("PUT", "/boot-source", "{}");
        assert_eq!(
            handle_request_bytes_with_limit(
                oversized_boot_request.as_bytes(),
                &mut vmm,
                oversized_boot_request.len() - 1,
            )
            .status(),
            bangbang_api::http::StatusCode::BadRequest
        );

        let boot_response = handle_request_bytes(
            request_with_body(
                "PUT",
                "/boot-source",
                r#"{"kernel_image_path":"/tmp/original-vmlinux"}"#,
            )
            .as_bytes(),
            &mut vmm,
        );
        assert_eq!(
            boot_response.status(),
            bangbang_api::http::StatusCode::NoContent
        );
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("instance should start");
        vmm.handle_action(VmmAction::FlushMetrics)
            .expect("metrics should flush");

        let metrics = read_metrics_json(&metrics_path);
        for (field, count, fails) in [
            ("actions", 1, 1),
            ("balloon", 0, 0),
            ("boot_source", 2, 1),
            ("cpu_cfg", 1, 1),
            ("drive", 2, 2),
            ("hotplug_memory", 1, 1),
            ("logger", 1, 1),
            ("machine_cfg", 1, 1),
            ("metrics", 1, 0),
            ("mmds", 2, 2),
            ("network", 2, 2),
            ("pmem", 2, 2),
            ("serial", 1, 1),
            ("vsock", 1, 1),
        ] {
            assert_metric(
                &metrics,
                "put_api_requests",
                &format!("{field}_count"),
                count,
            );
            assert_metric(
                &metrics,
                "put_api_requests",
                &format!("{field}_fails"),
                fails,
            );
        }
        for field in [
            "balloon",
            "drive",
            "hotplug_memory",
            "machine_cfg",
            "mmds",
            "network",
            "pmem",
        ] {
            let (count, fails) = match field {
                "balloon" => (0, 0),
                "drive" | "network" | "pmem" => (2, 2),
                _ => (1, 1),
            };
            assert_metric(
                &metrics,
                "patch_api_requests",
                &format!("{field}_count"),
                count,
            );
            assert_metric(
                &metrics,
                "patch_api_requests",
                &format!("{field}_fails"),
                fails,
            );
        }

        fs::remove_file(metrics_path).expect("metrics fixture should clean up");
    }

    #[test]
    fn configured_metrics_counts_core_config_put_api_requests() {
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        let metrics_path = unique_socket_path("metrics-core-config").with_extension("metrics");
        let drive_path = unique_socket_path("core-drive").with_extension("img");
        let replacement_drive_path =
            unique_socket_path("core-drive-rejected").with_extension("img");
        let vsock_path = unique_socket_path("core-vsock").with_extension("sock");
        let replacement_vsock_path =
            unique_socket_path("core-vsock-rejected").with_extension("sock");

        let metrics_body = format!(r#"{{"metrics_path":"{}"}}"#, metrics_path.to_string_lossy());
        let metrics_request = request_with_body("PUT", "/metrics", &metrics_body);
        assert_eq!(
            handle_request_bytes(metrics_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let machine_request = request_with_body(
            "PUT",
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        );
        assert_eq!(
            handle_request_bytes(machine_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let cpu_response = handle_request_bytes(
            request_with_body("PUT", "/cpu-config", "{}").as_bytes(),
            &mut vmm,
        );
        assert_eq!(
            cpu_response.status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let custom_cpu_response = handle_request_bytes(
            request_with_body("PUT", "/cpu-config", r#"{"kvm_capabilities":["1"]}"#).as_bytes(),
            &mut vmm,
        );
        assert_eq!(
            custom_cpu_response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );

        let boot_body = r#"{"kernel_image_path":"/tmp/original-vmlinux"}"#;
        let boot_request = request_with_body("PUT", "/boot-source", boot_body);
        assert_eq!(
            handle_request_bytes(boot_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let drive_body = format!(
            r#"{{"drive_id":"data","path_on_host":"{}","is_root_device":false,"is_read_only":false}}"#,
            drive_path.to_string_lossy()
        );
        let drive_request = request_with_body("PUT", "/drives/data", &drive_body);
        assert_eq!(
            handle_request_bytes(drive_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let network_body =
            r#"{"iface_id":"eth0","host_dev_name":"vmnet:shared","guest_mac":"12:34:56:78:9a:bc"}"#;
        let network_request = request_with_body("PUT", "/network-interfaces/eth0", network_body);
        assert_eq!(
            handle_request_bytes(network_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let vsock_body = format!(
            r#"{{"guest_cid":3,"uds_path":"{}"}}"#,
            vsock_path.to_string_lossy()
        );
        let vsock_request = request_with_body("PUT", "/vsock", &vsock_body);
        assert_eq!(
            handle_request_bytes(vsock_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let start_response = put_action_over_socket(&mut vmm, "core-a1", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));

        let machine_after_start_response = handle_request_bytes(
            request_with_body(
                "PUT",
                "/machine-config",
                r#"{"vcpu_count":2,"mem_size_mib":512}"#,
            )
            .as_bytes(),
            &mut vmm,
        );
        assert_eq!(
            machine_after_start_response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );

        let boot_after_start_response = handle_request_bytes(
            request_with_body(
                "PUT",
                "/boot-source",
                r#"{"kernel_image_path":"/tmp/replacement-vmlinux"}"#,
            )
            .as_bytes(),
            &mut vmm,
        );
        assert_eq!(
            boot_after_start_response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );

        let rejected_drive_body = format!(
            r#"{{"drive_id":"replacement","path_on_host":"{}","is_root_device":false,"is_read_only":false}}"#,
            replacement_drive_path.to_string_lossy()
        );
        let drive_after_start_response = handle_request_bytes(
            request_with_body("PUT", "/drives/replacement", &rejected_drive_body).as_bytes(),
            &mut vmm,
        );
        assert_eq!(
            drive_after_start_response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );
        assert!(!replacement_drive_path.exists());

        let network_after_start_response = handle_request_bytes(
            request_with_body(
                "PUT",
                "/network-interfaces/eth1",
                r#"{"iface_id":"eth1","host_dev_name":"vmnet:shared"}"#,
            )
            .as_bytes(),
            &mut vmm,
        );
        assert_eq!(
            network_after_start_response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );

        let rejected_vsock_body = format!(
            r#"{{"guest_cid":4,"uds_path":"{}"}}"#,
            replacement_vsock_path.to_string_lossy()
        );
        let vsock_after_start_response = handle_request_bytes(
            request_with_body("PUT", "/vsock", &rejected_vsock_body).as_bytes(),
            &mut vmm,
        );
        assert_eq!(
            vsock_after_start_response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );
        assert!(!replacement_vsock_path.exists());

        let flush_response = put_action_over_socket(&mut vmm, "core-a2", "FlushMetrics");
        assert!(flush_response.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert_eq!(
            fs::read_to_string(&metrics_path).expect("metrics output should be readable"),
            "{\"put_api_requests\":{\"actions_count\":2,\"actions_fails\":0,\"balloon_count\":0,\"balloon_fails\":0,\"boot_source_count\":2,\"boot_source_fails\":1,\"cpu_cfg_count\":2,\"cpu_cfg_fails\":1,\"drive_count\":2,\"drive_fails\":1,\"hotplug_memory_count\":0,\"hotplug_memory_fails\":0,\"logger_count\":0,\"logger_fails\":0,\"machine_cfg_count\":2,\"machine_cfg_fails\":1,\"metrics_count\":1,\"metrics_fails\":0,\"mmds_count\":0,\"mmds_fails\":0,\"network_count\":2,\"network_fails\":1,\"pmem_count\":0,\"pmem_fails\":0,\"serial_count\":0,\"serial_fails\":0,\"vsock_count\":2,\"vsock_fails\":1},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        assert!(!drive_path.exists());
        assert!(!vsock_path.exists());
        fs::remove_file(metrics_path).expect("metrics fixture should clean up");
    }

    #[test]
    fn configured_metrics_counts_mmds_put_api_requests() {
        let original_mmds_body = r#"{"latest":{"meta-data":{}}}"#;
        let mut vmm = test_controller_with_starter_and_mmds_data_store_limit(
            TestInstanceStarter::success(),
            original_mmds_body.len(),
        );
        let metrics_path = unique_socket_path("metrics-mmds").with_extension("metrics");

        let metrics_body = format!(r#"{{"metrics_path":"{}"}}"#, metrics_path.to_string_lossy());
        let metrics_request = request_with_body("PUT", "/metrics", &metrics_body);
        assert_eq!(
            handle_request_bytes(metrics_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let network_body = r#"{"iface_id":"eth0","host_dev_name":"vmnet:shared"}"#;
        let network_request = request_with_body("PUT", "/network-interfaces/eth0", network_body);
        assert_eq!(
            handle_request_bytes(network_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let mmds_config_body = r#"{"network_interfaces":["eth0"],"version":"V2"}"#;
        let mmds_config_request = request_with_body("PUT", "/mmds/config", mmds_config_body);
        assert_eq!(
            handle_request_bytes(mmds_config_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let mmds_request = request_with_body("PUT", "/mmds", original_mmds_body);
        assert_eq!(
            handle_request_bytes(mmds_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let oversized_mmds_body = r#"{"latest":{"meta-data":{"ami-id":"ami-oversized"}}}"#;
        let oversized_mmds_response = handle_request_bytes(
            request_with_body("PUT", "/mmds", oversized_mmds_body).as_bytes(),
            &mut vmm,
        );
        assert_eq!(
            oversized_mmds_response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );

        let boot_request = request_with_body(
            "PUT",
            "/boot-source",
            r#"{"kernel_image_path":"/tmp/vmlinux"}"#,
        );
        assert_eq!(
            handle_request_bytes(boot_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );
        let start_response = put_action_over_socket(&mut vmm, "mmds-a1", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));

        let mmds_config_after_start_response = handle_request_bytes(
            request_with_body("PUT", "/mmds/config", mmds_config_body).as_bytes(),
            &mut vmm,
        );
        assert_eq!(
            mmds_config_after_start_response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );

        let patch_response = handle_request_bytes(
            request_with_body("PATCH", "/mmds", r#"{"latest":{"meta-data":{}}}"#).as_bytes(),
            &mut vmm,
        );
        assert_eq!(
            patch_response.status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let flush_response = put_action_over_socket(&mut vmm, "mmds-a2", "FlushMetrics");
        assert!(flush_response.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert_eq!(
            fs::read_to_string(&metrics_path).expect("metrics output should be readable"),
            "{\"patch_api_requests\":{\"balloon_count\":0,\"balloon_fails\":0,\"drive_count\":0,\"drive_fails\":0,\"hotplug_memory_count\":0,\"hotplug_memory_fails\":0,\"machine_cfg_count\":0,\"machine_cfg_fails\":0,\"mmds_count\":1,\"mmds_fails\":0,\"network_count\":0,\"network_fails\":0,\"pmem_count\":0,\"pmem_fails\":0},\"put_api_requests\":{\"actions_count\":2,\"actions_fails\":0,\"balloon_count\":0,\"balloon_fails\":0,\"boot_source_count\":1,\"boot_source_fails\":0,\"cpu_cfg_count\":0,\"cpu_cfg_fails\":0,\"drive_count\":0,\"drive_fails\":0,\"hotplug_memory_count\":0,\"hotplug_memory_fails\":0,\"logger_count\":0,\"logger_fails\":0,\"machine_cfg_count\":0,\"machine_cfg_fails\":0,\"metrics_count\":1,\"metrics_fails\":0,\"mmds_count\":4,\"mmds_fails\":2,\"network_count\":1,\"network_fails\":0,\"pmem_count\":0,\"pmem_fails\":0,\"serial_count\":0,\"serial_fails\":0,\"vsock_count\":0,\"vsock_fails\":0},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(metrics_path).expect("metrics fixture should clean up");
    }

    #[test]
    fn configured_metrics_counts_pmem_api_requests() {
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        let metrics_path = unique_socket_path("metrics-pmem").with_extension("metrics");

        let metrics_body = format!(r#"{{"metrics_path":"{}"}}"#, metrics_path.to_string_lossy());
        let metrics_request = request_with_body("PUT", "/metrics", &metrics_body);
        assert_eq!(
            handle_request_bytes(metrics_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let valid_put_body = r#"{"id":"pmem0","path_on_host":"/private/tmp/pmem.img"}"#;
        let valid_put_response = request_over_socket(
            &mut vmm,
            "pm-put",
            &request_with_body("PUT", "/pmem/pmem0", valid_put_body),
        );
        assert!(valid_put_response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(
            valid_put_response.contains(r#"{"fault_message":"Pmem device is not supported."}"#)
        );

        let malformed_put_response = request_over_socket(
            &mut vmm,
            "pm-put-bad",
            &request_with_body("PUT", "/pmem/pmem0", r#"{"id":"pmem0"}"#),
        );
        assert!(malformed_put_response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(malformed_put_response.contains(r#"{"fault_message":"Malformed HTTP request."}"#));

        let valid_patch_response = request_over_socket(
            &mut vmm,
            "pm-pat",
            &request_with_body("PATCH", "/pmem/pmem0", r#"{"id":"pmem0"}"#),
        );
        assert!(valid_patch_response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(valid_patch_response.contains(
            r#"{"fault_message":"The requested operation is not supported in Not started state: PatchPmem"}"#
        ));

        let mismatched_patch_response = request_over_socket(
            &mut vmm,
            "pm-pat-mis",
            &request_with_body("PATCH", "/pmem/pmem0", r#"{"id":"other"}"#),
        );
        assert!(mismatched_patch_response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(
            mismatched_patch_response
                .contains(r#"{"fault_message":"path pmem id must match body id."}"#)
        );

        let boot_request = request_with_body(
            "PUT",
            "/boot-source",
            r#"{"kernel_image_path":"/tmp/vmlinux"}"#,
        );
        assert_eq!(
            handle_request_bytes(boot_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );
        let start_response = put_action_over_socket(&mut vmm, "pm-a1", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));

        let flush_response = put_action_over_socket(&mut vmm, "pm-a2", "FlushMetrics");
        assert!(flush_response.starts_with("HTTP/1.1 204 No Content\r\n"));
        let metrics_output =
            fs::read_to_string(&metrics_path).expect("metrics output should be readable");
        assert_eq!(
            metrics_output,
            "{\"patch_api_requests\":{\"balloon_count\":0,\"balloon_fails\":0,\"drive_count\":0,\"drive_fails\":0,\"hotplug_memory_count\":0,\"hotplug_memory_fails\":0,\"machine_cfg_count\":0,\"machine_cfg_fails\":0,\"mmds_count\":0,\"mmds_fails\":0,\"network_count\":0,\"network_fails\":0,\"pmem_count\":2,\"pmem_fails\":2},\"put_api_requests\":{\"actions_count\":2,\"actions_fails\":0,\"balloon_count\":0,\"balloon_fails\":0,\"boot_source_count\":1,\"boot_source_fails\":0,\"cpu_cfg_count\":0,\"cpu_cfg_fails\":0,\"drive_count\":0,\"drive_fails\":0,\"hotplug_memory_count\":0,\"hotplug_memory_fails\":0,\"logger_count\":0,\"logger_fails\":0,\"machine_cfg_count\":0,\"machine_cfg_fails\":0,\"metrics_count\":1,\"metrics_fails\":0,\"mmds_count\":0,\"mmds_fails\":0,\"network_count\":0,\"network_fails\":0,\"pmem_count\":2,\"pmem_fails\":2,\"serial_count\":0,\"serial_fails\":0,\"vsock_count\":0,\"vsock_fails\":0},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );
        assert!(!metrics_output.contains("pmem0"));
        assert!(!metrics_output.contains("/private/tmp/pmem.img"));

        fs::remove_file(metrics_path).expect("metrics fixture should clean up");
    }

    #[test]
    fn configured_metrics_counts_memory_hotplug_api_requests() {
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        let metrics_path = unique_socket_path("metrics-memory-hotplug").with_extension("metrics");

        let metrics_body = format!(r#"{{"metrics_path":"{}"}}"#, metrics_path.to_string_lossy());
        let metrics_request = request_with_body("PUT", "/metrics", &metrics_body);
        assert_eq!(
            handle_request_bytes(metrics_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let valid_get_response = request_over_socket(
            &mut vmm,
            "mh-m-get",
            "GET /hotplug/memory HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        assert!(valid_get_response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(valid_get_response.contains(
            r#"{"fault_message":"The requested operation is not supported in Not started state: GetMemoryHotplug"}"#
        ));

        let malformed_get_response = request_over_socket(
            &mut vmm,
            "mh-m-get-bad",
            &request_with_body("GET", "/hotplug/memory", "{}"),
        );
        assert!(malformed_get_response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(
            malformed_get_response
                .contains(r#"{"fault_message":"GET request cannot have a body."}"#)
        );

        let valid_put_body =
            r#"{"total_size_mib":222222222,"block_size_mib":2,"slot_size_mib":128}"#;
        let valid_put_response = request_over_socket(
            &mut vmm,
            "mh-m-put",
            &request_with_body("PUT", "/hotplug/memory", valid_put_body),
        );
        assert!(valid_put_response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(
            valid_put_response.contains(r#"{"fault_message":"Memory hotplug is not supported."}"#)
        );

        let malformed_put_response = request_over_socket(
            &mut vmm,
            "mh-m-put-bad",
            &request_with_body("PUT", "/hotplug/memory", r#"{"size_mib":2048}"#),
        );
        assert!(malformed_put_response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(malformed_put_response.contains(r#"{"fault_message":"Malformed HTTP request."}"#));

        let valid_patch_body = r#"{"requested_size_mib":333333333}"#;
        let valid_patch_response = request_over_socket(
            &mut vmm,
            "mh-m-pat",
            &request_with_body("PATCH", "/hotplug/memory", valid_patch_body),
        );
        assert!(valid_patch_response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(valid_patch_response.contains(
            r#"{"fault_message":"The requested operation is not supported in Not started state: PatchMemoryHotplug"}"#
        ));

        let malformed_patch_response = request_over_socket(
            &mut vmm,
            "mh-m-pat-bad",
            &request_with_body("PATCH", "/hotplug/memory", "not-json"),
        );
        assert!(malformed_patch_response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(
            malformed_patch_response.contains(r#"{"fault_message":"Malformed HTTP request."}"#)
        );

        let boot_request = request_with_body(
            "PUT",
            "/boot-source",
            r#"{"kernel_image_path":"/tmp/vmlinux"}"#,
        );
        assert_eq!(
            handle_request_bytes(boot_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );
        let start_response = put_action_over_socket(&mut vmm, "mh-m-a1", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));

        let flush_response = put_action_over_socket(&mut vmm, "mh-m-a2", "FlushMetrics");
        assert!(flush_response.starts_with("HTTP/1.1 204 No Content\r\n"));
        let metrics_output =
            fs::read_to_string(&metrics_path).expect("metrics output should be readable");
        assert_eq!(
            metrics_output,
            "{\"get_api_requests\":{\"balloon_count\":0,\"hotplug_memory_count\":1,\"instance_info_count\":0,\"machine_cfg_count\":0,\"mmds_count\":0,\"vmm_version_count\":0},\"patch_api_requests\":{\"balloon_count\":0,\"balloon_fails\":0,\"drive_count\":0,\"drive_fails\":0,\"hotplug_memory_count\":2,\"hotplug_memory_fails\":2,\"machine_cfg_count\":0,\"machine_cfg_fails\":0,\"mmds_count\":0,\"mmds_fails\":0,\"network_count\":0,\"network_fails\":0,\"pmem_count\":0,\"pmem_fails\":0},\"put_api_requests\":{\"actions_count\":2,\"actions_fails\":0,\"balloon_count\":0,\"balloon_fails\":0,\"boot_source_count\":1,\"boot_source_fails\":0,\"cpu_cfg_count\":0,\"cpu_cfg_fails\":0,\"drive_count\":0,\"drive_fails\":0,\"hotplug_memory_count\":2,\"hotplug_memory_fails\":2,\"logger_count\":0,\"logger_fails\":0,\"machine_cfg_count\":0,\"machine_cfg_fails\":0,\"metrics_count\":1,\"metrics_fails\":0,\"mmds_count\":0,\"mmds_fails\":0,\"network_count\":0,\"network_fails\":0,\"pmem_count\":0,\"pmem_fails\":0,\"serial_count\":0,\"serial_fails\":0,\"vsock_count\":0,\"vsock_fails\":0},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );
        assert!(!metrics_output.contains("222222222"));
        assert!(!metrics_output.contains("333333333"));

        fs::remove_file(metrics_path).expect("metrics fixture should clean up");
    }

    #[test]
    fn configured_metrics_counts_deprecated_api_requests() {
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        let metrics_path = unique_socket_path("metrics-deprecated-api").with_extension("metrics");

        let metrics_body = format!(r#"{{"metrics_path":"{}"}}"#, metrics_path.to_string_lossy());
        let metrics_request = request_with_body("PUT", "/metrics", &metrics_body);
        assert_eq!(
            handle_request_bytes(metrics_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let machine_request = request_with_body(
            "PUT",
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256,"cpu_template":null}"#,
        );
        assert_eq!(
            handle_request_bytes(machine_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let machine_patch_request = request_with_body(
            "PATCH",
            "/machine-config",
            r#"{"mem_size_mib":256,"cpu_template":null}"#,
        );
        assert_eq!(
            handle_request_bytes(machine_patch_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let mmds_config_response = handle_request_bytes(
            request_with_body("PUT", "/mmds/config", r#"{"network_interfaces":[]}"#).as_bytes(),
            &mut vmm,
        );
        assert_eq!(
            mmds_config_response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );

        let vsock_body = r#"{"vsock_id":"vsock-secret","guest_cid":2,"uds_path":"/private/tmp/deprecated-vsock-secret.sock"}"#;
        let vsock_response = handle_request_bytes(
            request_with_body("PUT", "/vsock", vsock_body).as_bytes(),
            &mut vmm,
        );
        assert_eq!(
            vsock_response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );

        let snapshot_load_body = r#"{"snapshot_path":"/private/tmp/deprecated-vmstate","mem_file_path":"/private/tmp/deprecated-memory"}"#;
        let snapshot_load_response = handle_request_bytes(
            request_with_body("PUT", "/snapshot/load", snapshot_load_body).as_bytes(),
            &mut vmm,
        );
        assert_eq!(
            snapshot_load_response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );

        let snapshot_load_false_body = r#"{"snapshot_path":"/private/tmp/deprecated-vmstate","mem_backend":{"backend_path":"/private/tmp/deprecated-memory-backend","backend_type":"File"},"enable_diff_snapshots":false}"#;
        let snapshot_load_false_response = handle_request_bytes(
            request_with_body("PUT", "/snapshot/load", snapshot_load_false_body).as_bytes(),
            &mut vmm,
        );
        assert_eq!(
            snapshot_load_false_response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );

        let malformed_machine_response = handle_request_bytes(
            request_with_body(
                "PUT",
                "/machine-config",
                r#"{"vcpu_count":1,"mem_size_mib":256,"cpu_template":"unknown-template"}"#,
            )
            .as_bytes(),
            &mut vmm,
        );
        assert_eq!(
            malformed_machine_response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );

        let malformed_snapshot_load_body = r#"{"snapshot_path":"vmstate","mem_file_path":"memory","mem_backend":{"backend_path":"memory","backend_type":"File"}}"#;
        let malformed_snapshot_load_response = handle_request_bytes(
            request_with_body("PUT", "/snapshot/load", malformed_snapshot_load_body).as_bytes(),
            &mut vmm,
        );
        assert_eq!(
            malformed_snapshot_load_response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );

        let boot_request = request_with_body(
            "PUT",
            "/boot-source",
            r#"{"kernel_image_path":"/tmp/vmlinux"}"#,
        );
        assert_eq!(
            handle_request_bytes(boot_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );
        let start_response = put_action_over_socket(&mut vmm, "deprecated-a1", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));

        let flush_response = put_action_over_socket(&mut vmm, "deprecated-a2", "FlushMetrics");
        assert!(flush_response.starts_with("HTTP/1.1 204 No Content\r\n"));
        let metrics_output =
            fs::read_to_string(&metrics_path).expect("metrics output should be readable");
        assert_eq!(
            metrics_output,
            "{\"deprecated_api\":{\"deprecated_http_api_calls\":3},\"patch_api_requests\":{\"balloon_count\":0,\"balloon_fails\":0,\"drive_count\":0,\"drive_fails\":0,\"hotplug_memory_count\":0,\"hotplug_memory_fails\":0,\"machine_cfg_count\":1,\"machine_cfg_fails\":0,\"mmds_count\":0,\"mmds_fails\":0,\"network_count\":0,\"network_fails\":0,\"pmem_count\":0,\"pmem_fails\":0},\"put_api_requests\":{\"actions_count\":2,\"actions_fails\":0,\"balloon_count\":0,\"balloon_fails\":0,\"boot_source_count\":1,\"boot_source_fails\":0,\"cpu_cfg_count\":0,\"cpu_cfg_fails\":0,\"drive_count\":0,\"drive_fails\":0,\"hotplug_memory_count\":0,\"hotplug_memory_fails\":0,\"logger_count\":0,\"logger_fails\":0,\"machine_cfg_count\":2,\"machine_cfg_fails\":1,\"metrics_count\":1,\"metrics_fails\":0,\"mmds_count\":1,\"mmds_fails\":1,\"network_count\":0,\"network_fails\":0,\"pmem_count\":0,\"pmem_fails\":0,\"serial_count\":0,\"serial_fails\":0,\"vsock_count\":1,\"vsock_fails\":1},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );
        assert!(!metrics_output.contains("vsock-secret"));
        assert!(!metrics_output.contains("deprecated-vsock-secret"));
        assert!(!metrics_output.contains("deprecated-vmstate"));
        assert!(!metrics_output.contains("deprecated-memory"));

        fs::remove_file(metrics_path).expect("metrics fixture should clean up");
    }

    #[test]
    fn configured_metrics_counts_patch_api_requests() {
        let original_mmds_body = r#"{"latest":{"meta-data":{}}}"#;
        let mut vmm = test_controller_with_starter_and_mmds_data_store_limit(
            TestInstanceStarter::success(),
            original_mmds_body.len(),
        );
        vmm.handle_action(VmmAction::PutDrive(DriveConfigInput::new(
            "rootfs",
            "rootfs",
            "/tmp/rootfs.ext4",
            true,
        )))
        .expect("initial drive should configure");
        vmm.handle_action(VmmAction::PutMmds(MmdsContentInput::new(
            serde_json::json!({"latest": {"meta-data": {}}}),
        )))
        .expect("MMDS data should configure");
        let metrics_path = unique_socket_path("metrics-patch").with_extension("metrics");

        let metrics_body = format!(r#"{{"metrics_path":"{}"}}"#, metrics_path.to_string_lossy());
        let metrics_request = request_with_body("PUT", "/metrics", &metrics_body);
        assert_eq!(
            handle_request_bytes(metrics_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let machine_patch_request =
            request_with_body("PATCH", "/machine-config", r#"{"vcpu_count":2}"#);
        assert_eq!(
            handle_request_bytes(machine_patch_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );
        let mmds_patch_request = request_with_body("PATCH", "/mmds", original_mmds_body);
        assert_eq!(
            handle_request_bytes(mmds_patch_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );
        let oversized_mmds_patch_body = r#"{"latest":{"meta-data":{"ami-id":"ami-oversized"}}}"#;
        assert_eq!(
            handle_request_bytes(
                request_with_body("PATCH", "/mmds", oversized_mmds_patch_body).as_bytes(),
                &mut vmm,
            )
            .status(),
            bangbang_api::http::StatusCode::BadRequest
        );

        let drive_patch_body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/replaced.ext4"
        }"#;
        assert_eq!(
            handle_request_bytes(
                request_with_body("PATCH", "/drives/rootfs", drive_patch_body).as_bytes(),
                &mut vmm,
            )
            .status(),
            bangbang_api::http::StatusCode::BadRequest
        );
        let network_patch_response = request_over_socket(
            &mut vmm,
            "pn",
            &request_with_body(
                "PATCH",
                "/network-interfaces/eth0",
                r#"{"iface_id":"eth0","rx_rate_limiter":{"bandwidth":{"size":123456,"one_time_burst":234567,"refill_time":345678}}}"#,
            ),
        );
        assert!(
            network_patch_response.starts_with("HTTP/1.1 400 Bad Request\r\n"),
            "network patch should fail through the API socket; response:\n{network_patch_response}"
        );
        assert!(
            network_patch_response.contains(
                r#"{"fault_message":"The requested operation is not supported in Not started state: UpdateNetworkInterface"}"#
            ),
            "preboot network patch should fail on lifecycle state; response:\n{network_patch_response}"
        );
        for private_value in ["123456", "234567", "345678"] {
            assert!(
                !network_patch_response.contains(private_value),
                "preboot network patch response must not echo {private_value}: {network_patch_response}"
            );
        }
        for (socket_name, body, fault_message) in [
            (
                "pn-mis",
                r#"{"iface_id":"eth1"}"#,
                "path iface_id must match body iface_id.",
            ),
            ("pn-bad", "not-json", "Malformed HTTP request."),
        ] {
            let response = request_over_socket(
                &mut vmm,
                socket_name,
                &request_with_body("PATCH", "/network-interfaces/eth0", body),
            );
            assert!(
                response.starts_with("HTTP/1.1 400 Bad Request\r\n"),
                "{socket_name} should fail through the API socket; response:\n{response}"
            );
            assert!(
                response.contains(&format!(r#"{{"fault_message":"{fault_message}"}}"#)),
                "{socket_name} should return the parser fault before VMM metrics; response:\n{response}"
            );
        }
        let vm_config_after_network_patch = handle_request_bytes(
            b"GET /vm/config HTTP/1.1\r\nHost: localhost\r\n\r\n",
            &mut vmm,
        );
        assert_eq!(
            vm_config_after_network_patch.status(),
            bangbang_api::http::StatusCode::Ok
        );
        assert!(
            vm_config_after_network_patch
                .body()
                .contains(r#""network-interfaces":[]"#),
            "rejected network PATCH must not add network interface config; response:\n{}",
            vm_config_after_network_patch.body()
        );
        assert_eq!(
            handle_request_bytes(
                request_with_body("PATCH", "/vm", r#"{"state":"Paused"}"#).as_bytes(),
                &mut vmm,
            )
            .status(),
            bangbang_api::http::StatusCode::BadRequest
        );

        let boot_request = request_with_body(
            "PUT",
            "/boot-source",
            r#"{"kernel_image_path":"/tmp/vmlinux"}"#,
        );
        assert_eq!(
            handle_request_bytes(boot_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );
        let start_response = put_action_over_socket(&mut vmm, "patch-a1", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));

        let runtime_network_patch_response = request_over_socket(
            &mut vmm,
            "pn-running",
            &request_with_body(
                "PATCH",
                "/network-interfaces/eth0",
                r#"{"iface_id":"eth0","rx_rate_limiter":{"bandwidth":{"size":223456,"one_time_burst":334567,"refill_time":445678}}}"#,
            ),
        );
        assert!(
            runtime_network_patch_response.starts_with("HTTP/1.1 400 Bad Request\r\n"),
            "running-state network patch should fail through the API socket; response:\n{runtime_network_patch_response}"
        );
        assert!(
            runtime_network_patch_response
                .contains(r#"{"fault_message":"Network interface updates are not supported."}"#),
            "running-state network patch should keep the existing unsupported fault body; response:\n{runtime_network_patch_response}"
        );
        for private_value in ["223456", "334567", "445678"] {
            assert!(
                !runtime_network_patch_response.contains(private_value),
                "running-state network patch response must not echo {private_value}: {runtime_network_patch_response}"
            );
        }

        assert_eq!(
            handle_request_bytes(machine_patch_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::BadRequest
        );

        let flush_response = put_action_over_socket(&mut vmm, "patch-a2", "FlushMetrics");
        assert!(flush_response.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert_eq!(
            fs::read_to_string(&metrics_path).expect("metrics output should be readable"),
            "{\"patch_api_requests\":{\"balloon_count\":0,\"balloon_fails\":0,\"drive_count\":1,\"drive_fails\":1,\"hotplug_memory_count\":0,\"hotplug_memory_fails\":0,\"machine_cfg_count\":2,\"machine_cfg_fails\":1,\"mmds_count\":2,\"mmds_fails\":1,\"network_count\":4,\"network_fails\":4,\"pmem_count\":0,\"pmem_fails\":0},\"put_api_requests\":{\"actions_count\":2,\"actions_fails\":0,\"balloon_count\":0,\"balloon_fails\":0,\"boot_source_count\":1,\"boot_source_fails\":0,\"cpu_cfg_count\":0,\"cpu_cfg_fails\":0,\"drive_count\":0,\"drive_fails\":0,\"hotplug_memory_count\":0,\"hotplug_memory_fails\":0,\"logger_count\":0,\"logger_fails\":0,\"machine_cfg_count\":0,\"machine_cfg_fails\":0,\"metrics_count\":1,\"metrics_fails\":0,\"mmds_count\":0,\"mmds_fails\":0,\"network_count\":0,\"network_fails\":0,\"pmem_count\":0,\"pmem_fails\":0,\"serial_count\":0,\"serial_fails\":0,\"vsock_count\":0,\"vsock_fails\":0},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(metrics_path).expect("metrics fixture should clean up");
    }

    #[test]
    fn configured_metrics_flush_writes_boot_run_loop_status_over_unix_socket() {
        let mut vmm =
            test_controller_with_starter(TestInstanceStarter::success_with_boot_run_loop_status(
                BootRunLoopMetricStatus::Running,
            ));
        let metrics_path = unique_socket_path("metrics-boot-loop").with_extension("metrics");
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
            put_action_over_socket(&mut vmm, "start-with-boot-loop", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));

        let flush_response =
            put_action_over_socket(&mut vmm, "flush-with-boot-loop", "FlushMetrics");

        assert!(flush_response.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert_eq!(
            fs::read_to_string(&metrics_path).expect("metrics output should be readable"),
            "{\"put_api_requests\":{\"actions_count\":2,\"actions_fails\":0,\"balloon_count\":0,\"balloon_fails\":0,\"boot_source_count\":1,\"boot_source_fails\":0,\"cpu_cfg_count\":0,\"cpu_cfg_fails\":0,\"drive_count\":0,\"drive_fails\":0,\"hotplug_memory_count\":0,\"hotplug_memory_fails\":0,\"logger_count\":0,\"logger_fails\":0,\"machine_cfg_count\":0,\"machine_cfg_fails\":0,\"metrics_count\":1,\"metrics_fails\":0,\"mmds_count\":0,\"mmds_fails\":0,\"network_count\":0,\"network_fails\":0,\"pmem_count\":0,\"pmem_fails\":0,\"serial_count\":0,\"serial_fails\":0,\"vsock_count\":0,\"vsock_fails\":0},\"vmm\":{\"boot_run_loop_status\":\"running\",\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(metrics_path).expect("fixture should clean up");
    }

    #[test]
    fn api_metrics_update_after_startup_metrics_preserves_startup_sink() {
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        let startup_metrics_path = unique_socket_path("startup-metrics").with_extension("metrics");
        let api_metrics_path = unique_socket_path("api-metrics").with_extension("metrics");
        vmm.handle_action(VmmAction::PutMetrics(MetricsConfigInput::new(
            &startup_metrics_path,
        )))
        .expect("startup metrics config should apply");
        let metrics_body = format!(
            r#"{{"metrics_path":"{}"}}"#,
            api_metrics_path.to_string_lossy()
        );
        let metrics_request = format!(
            "PUT /metrics HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{metrics_body}",
            metrics_body.len()
        );
        let metrics_response = handle_request_bytes(metrics_request.as_bytes(), &mut vmm);
        assert_eq!(
            metrics_response.status(),
            bangbang_api::http::StatusCode::BadRequest
        );
        assert_eq!(
            metrics_response.body(),
            r#"{"fault_message":"metrics system is already initialized"}"#
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
        let start_response = put_action_over_socket(&mut vmm, "met-start", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));
        let flush_response = put_action_over_socket(&mut vmm, "met-flush", "FlushMetrics");
        assert!(flush_response.starts_with("HTTP/1.1 204 No Content\r\n"));

        assert_eq!(
            fs::read_to_string(&startup_metrics_path)
                .expect("startup metrics output should be readable"),
            "{\"put_api_requests\":{\"actions_count\":2,\"actions_fails\":0,\"balloon_count\":0,\"balloon_fails\":0,\"boot_source_count\":1,\"boot_source_fails\":0,\"cpu_cfg_count\":0,\"cpu_cfg_fails\":0,\"drive_count\":0,\"drive_fails\":0,\"hotplug_memory_count\":0,\"hotplug_memory_fails\":0,\"logger_count\":0,\"logger_fails\":0,\"machine_cfg_count\":0,\"machine_cfg_fails\":0,\"metrics_count\":1,\"metrics_fails\":1,\"mmds_count\":0,\"mmds_fails\":0,\"network_count\":0,\"network_fails\":0,\"pmem_count\":0,\"pmem_fails\":0,\"serial_count\":0,\"serial_fails\":0,\"vsock_count\":0,\"vsock_fails\":0},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );
        assert!(!api_metrics_path.exists());

        fs::remove_file(startup_metrics_path).expect("startup fixture should clean up");
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
    fn bind_restricts_socket_path_permissions() {
        let path = unique_socket_path("mode");
        let _server = ApiServer::bind(&path).expect("server should bind");
        let metadata = socket_path_metadata(&path).expect("API socket path should exist");

        assert_eq!(metadata.mode() & 0o777, API_SOCKET_MODE);
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
                .with_guest_mac("12:34:56:78:9a:bc")
                .with_mtu(1500),
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
        assert!(response.contains(r#""mtu":1500"#));
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
    fn serves_put_mmds_above_default_with_configured_mmds_size_limit() {
        let path = unique_socket_path("mmds-large-limit");
        let body = format!(r#"{{"data":"{}"}}"#, "x".repeat(HTTP_MAX_PAYLOAD_SIZE));
        assert!(body.len() > HTTP_MAX_PAYLOAD_SIZE);
        let request = format!(
            "PUT /mmds HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let server = ApiServer::bind_with_max_payload_size(&path, request.len())
            .expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");
        let client_handle = thread::spawn(move || {
            client
                .write_all(request.as_bytes())
                .expect("client should write request");
            let mut response = String::new();
            client
                .read_to_string(&mut response)
                .expect("client should read response");
            response
        });
        let mut vmm = test_controller_with_mmds_data_store_limit(body.len());

        server
            .serve_next(&mut vmm)
            .expect("server should handle one request");

        let response = client_handle
            .join()
            .expect("client thread should not panic");
        assert!(response.starts_with("HTTP/1.1 204 No Content\r\n"));

        let get_response =
            handle_request_bytes(b"GET /mmds HTTP/1.1\r\nHost: localhost\r\n\r\n", &mut vmm);
        assert_eq!(get_response.status(), bangbang_api::http::StatusCode::Ok);
        assert_eq!(get_response.body(), body);
    }

    #[test]
    fn oversized_put_mmds_with_configured_size_limit_does_not_mutate_store() {
        let original_body = r#"{"latest":{"meta-data":{}}}"#.to_string();
        let mut vmm = test_controller_with_mmds_data_store_limit(original_body.len());
        let original_request = format!(
            "PUT /mmds HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{original_body}",
            original_body.len()
        );
        assert_eq!(
            handle_request_bytes(original_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        let path = unique_socket_path("mmds-small-limit");
        let server = ApiServer::bind_with_max_payload_size(&path, HTTP_MAX_PAYLOAD_SIZE)
            .expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");
        let oversized_body = format!(r#"{{"data":"{}"}}"#, "x".repeat(64));
        let oversized_request = format!(
            "PUT /mmds HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{oversized_body}",
            oversized_body.len()
        );

        client
            .write_all(oversized_request.as_bytes())
            .expect("client should write request");
        server
            .serve_next(&mut vmm)
            .expect("server should handle one request");

        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("client should read response");

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(response.contains("The MMDS data store size limit was exceeded"));

        let get_response =
            handle_request_bytes(b"GET /mmds HTTP/1.1\r\nHost: localhost\r\n\r\n", &mut vmm);
        assert_eq!(get_response.status(), bangbang_api::http::StatusCode::Ok);
        assert_eq!(get_response.body(), original_body);
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
    fn returns_fault_for_snapshot_endpoint() {
        let mut vmm = test_controller();
        for (socket_name, request, fault_message) in [
            (
                "snapshot-create",
                request_with_body(
                    "PUT",
                    "/snapshot/create",
                    r#"{"snapshot_path":"vmstate","mem_file_path":"memory"}"#,
                ),
                "The requested operation is not supported in Not started state: CreateSnapshot",
            ),
            (
                "snapshot-load",
                request_with_body(
                    "PUT",
                    "/snapshot/load",
                    r#"{"snapshot_path":"vmstate","mem_backend":{"backend_path":"memory","backend_type":"File"}}"#,
                ),
                "Snapshot and restore are not supported.",
            ),
            (
                "snapshot-create-bad",
                request_with_body("PUT", "/snapshot/create", "{}"),
                "Malformed HTTP request.",
            ),
            (
                "snapshot-load-bad",
                request_with_body("PUT", "/snapshot/load", r#"{"snapshot_path":"vmstate"}"#),
                "Malformed HTTP request.",
            ),
        ] {
            let response = request_over_socket(&mut vmm, socket_name, &request);

            assert!(
                response.starts_with("HTTP/1.1 400 Bad Request\r\n"),
                "{socket_name}: {response}"
            );
            assert!(
                response.contains(&format!(r#"{{"fault_message":"{fault_message}"}}"#)),
                "{socket_name}: {response}"
            );
        }
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::NotStarted
        );
    }

    #[test]
    fn returns_stateful_faults_for_running_snapshot_endpoints() {
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
        let start_response = put_action_over_socket(&mut vmm, "snap-start", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));

        for (socket_name, request, fault_message) in [
            (
                "snap-cr-run",
                request_with_body(
                    "PUT",
                    "/snapshot/create",
                    r#"{"snapshot_path":"vmstate","mem_file_path":"memory"}"#,
                ),
                "Snapshot and restore are not supported.",
            ),
            (
                "snap-ld-run",
                request_with_body(
                    "PUT",
                    "/snapshot/load",
                    r#"{"snapshot_path":"vmstate","mem_backend":{"backend_path":"memory","backend_type":"File"}}"#,
                ),
                "The requested operation is not supported in Running state: LoadSnapshot",
            ),
        ] {
            let response = request_over_socket(&mut vmm, socket_name, &request);

            assert!(
                response.starts_with("HTTP/1.1 400 Bad Request\r\n"),
                "{socket_name}: {response}"
            );
            assert!(
                response.contains(&format!(r#"{{"fault_message":"{fault_message}"}}"#)),
                "{socket_name}: {response}"
            );
            assert!(
                !response.contains("vmstate") && !response.contains("memory"),
                "{socket_name} must not echo snapshot paths: {response}"
            );
        }
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::Running
        );
    }

    #[test]
    fn not_started_state_accepts_noop_cpu_config_without_mutating() {
        let mut vmm = test_controller();
        let request = "PUT /cpu-config HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\n\r\n{}";

        let response = request_over_socket(&mut vmm, "cpu-cfg-ns", request);

        assert!(response.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::NotStarted
        );
    }

    #[test]
    fn not_started_state_accepts_empty_array_cpu_config_without_mutating() {
        let mut vmm = test_controller();
        let body = r#"{"kvm_capabilities":[],"reg_modifiers":[],"vcpu_features":[]}"#;
        let request = format!(
            "PUT /cpu-config HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        let response = request_over_socket(&mut vmm, "cpu-cfg-empty-arrays", &request);

        assert!(response.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::NotStarted
        );
    }

    #[test]
    fn not_started_state_rejects_custom_cpu_config_without_mutating() {
        let mut vmm = test_controller();
        let body = r#"{"kvm_capabilities":["1"]}"#;
        let request = format!(
            "PUT /cpu-config HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        let response = request_over_socket(&mut vmm, "cpu-cfg-custom", &request);

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(response.contains(
            r#"{"fault_message":"The requested operation is not supported: PutCpuConfig"}"#
        ));
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::NotStarted
        );
    }

    #[test]
    fn rejects_malformed_cpu_config_without_mutating() {
        let mut vmm = test_controller();
        let request =
            "PUT /cpu-config HTTP/1.1\r\nHost: localhost\r\nContent-Length: 8\r\n\r\nnot-json";

        let response = request_over_socket(&mut vmm, "cpu-cfg-bad", request);

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(response.contains(r#"{"fault_message":"Malformed HTTP request."}"#));
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::NotStarted
        );
    }

    #[test]
    fn running_state_rejects_cpu_config_without_mutating() {
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
        let start_response = put_action_over_socket(&mut vmm, "cpu-cfg-start", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));
        let request = "PUT /cpu-config HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\n\r\n{}";

        let response = request_over_socket(&mut vmm, "cpu-cfg-run", request);

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(response.contains(
            r#"{"fault_message":"The requested operation is not supported in Running state: PutCpuConfig"}"#
        ));
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::Running
        );
    }

    #[test]
    fn stores_entropy_endpoint_without_rate_limiter() {
        let mut vmm = test_controller();
        for (socket_name, body) in [
            ("ent-empty", "{}"),
            ("ent-null-rl", r#"{"rate_limiter":null}"#),
        ] {
            let request = format!(
                "PUT /entropy HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );

            let response = request_over_socket(&mut vmm, socket_name, &request);

            assert!(
                response.starts_with("HTTP/1.1 204 No Content\r\n"),
                "{socket_name}: {response}"
            );
            let data = vmm
                .handle_action(VmmAction::GetVmConfig)
                .expect("VM config should be returned");
            let VmmData::VmConfiguration(config) = data else {
                panic!("expected VM config");
            };
            assert!(config.entropy_config().is_some(), "{socket_name}");
            let vm_config_response = handle_request_bytes(
                b"GET /vm/config HTTP/1.1\r\nHost: localhost\r\n\r\n",
                &mut vmm,
            );
            assert_eq!(
                vm_config_response.status(),
                bangbang_api::http::StatusCode::Ok
            );
            assert!(
                vm_config_response.body().contains(r#""entropy":{}"#),
                "{socket_name}: {}",
                vm_config_response.body()
            );
        }
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::NotStarted
        );
    }

    #[test]
    fn returns_fault_for_invalid_entropy_endpoint_without_mutating() {
        let mut vmm = test_controller();
        vmm.handle_action(VmmAction::PutEntropy(EntropyConfigInput::new()))
            .expect("initial entropy config should store");
        for (socket_name, body, fault_message, private_values) in [
            (
                "ent-rl",
                r#"{"rate_limiter":{"bandwidth":{"size":123456789,"one_time_burst":987654321,"refill_time":777}}}"#,
                "entropy rate_limiter is not supported",
                &["123456789", "987654321", "777"][..],
            ),
            ("ent-bad", "not-json", "Malformed HTTP request.", &[][..]),
            (
                "ent-bad-rl",
                r#"{"rate_limiter":{"bandwidth":{"size":1}}}"#,
                "Malformed HTTP request.",
                &[][..],
            ),
        ] {
            let request = format!(
                "PUT /entropy HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );

            let response = request_over_socket(&mut vmm, socket_name, &request);

            assert!(
                response.starts_with("HTTP/1.1 400 Bad Request\r\n"),
                "{socket_name}: {response}"
            );
            assert!(
                response.contains(&format!(r#"{{"fault_message":"{fault_message}"}}"#)),
                "{socket_name}: {response}"
            );
            for private_value in private_values {
                assert!(
                    !response.contains(private_value),
                    "{socket_name} must not echo private entropy config value {private_value}: {response}"
                );
            }
            let data = vmm
                .handle_action(VmmAction::GetVmConfig)
                .expect("VM config should be returned");
            let VmmData::VmConfiguration(config) = data else {
                panic!("expected VM config");
            };
            assert!(config.entropy_config().is_some(), "{socket_name}");
        }
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::NotStarted
        );
    }

    #[test]
    fn running_state_rejects_entropy_endpoint_without_mutating() {
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
        vmm.handle_action(VmmAction::PutEntropy(EntropyConfigInput::new()))
            .expect("initial entropy config should store");
        let start_response = put_action_over_socket(&mut vmm, "ent-start", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));
        let body = r#"{"rate_limiter":{"ops":{"size":222222222,"one_time_burst":333333333,"refill_time":444}}}"#;
        let request = format!(
            "PUT /entropy HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        let response = request_over_socket(&mut vmm, "ent-run", &request);

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(response.contains(
            r#"{"fault_message":"The requested operation is not supported in Running state: PutEntropy"}"#
        ));
        for private_value in ["222222222", "333333333", "444"] {
            assert!(
                !response.contains(private_value),
                "running-state entropy response must not echo {private_value}: {response}"
            );
        }
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::Running
        );
        let data = vmm
            .handle_action(VmmAction::GetVmConfig)
            .expect("VM config should be returned");
        let VmmData::VmConfiguration(config) = data else {
            panic!("expected VM config");
        };
        assert!(config.entropy_config().is_some());
    }

    #[test]
    fn returns_fault_for_balloon_endpoints() {
        let mut vmm = test_controller();
        for (socket_name, request, fault_message) in [
            (
                "b-get",
                "GET /balloon HTTP/1.1\r\nHost: localhost\r\n\r\n".to_string(),
                "Balloon device is not supported.",
            ),
            (
                "b-stats-get",
                "GET /balloon/statistics HTTP/1.1\r\nHost: localhost\r\n\r\n".to_string(),
                "The requested operation is not supported in Not started state: GetBalloonStats",
            ),
            (
                "b-hint-status",
                "GET /balloon/hinting/status HTTP/1.1\r\nHost: localhost\r\n\r\n".to_string(),
                "The requested operation is not supported in Not started state: GetBalloonHintingStatus",
            ),
            (
                "b-put",
                request_with_body(
                    "PUT",
                    "/balloon",
                    r#"{"amount_mib":64,"deflate_on_oom":true}"#,
                ),
                "Balloon device is not supported.",
            ),
            (
                "b-put-bad",
                request_with_body("PUT", "/balloon", "{}"),
                "Malformed HTTP request.",
            ),
            (
                "b-patch",
                request_with_body("PATCH", "/balloon", r#"{"amount_mib":32}"#),
                "The requested operation is not supported in Not started state: PatchBalloon",
            ),
            (
                "b-stats-patch",
                request_with_body(
                    "PATCH",
                    "/balloon/statistics",
                    r#"{"stats_polling_interval_s":1}"#,
                ),
                "The requested operation is not supported in Not started state: PatchBalloonStats",
            ),
            (
                "b-hint-start",
                request_with_body(
                    "PATCH",
                    "/balloon/hinting/start",
                    r#"{"acknowledge_on_stop":false}"#,
                ),
                "The requested operation is not supported in Not started state: PatchBalloonHintingStart",
            ),
            (
                "b-hint-start-bad",
                request_with_body(
                    "PATCH",
                    "/balloon/hinting/start",
                    r#"{"acknowledge_on_stop":"false"}"#,
                ),
                "Malformed HTTP request.",
            ),
            (
                "b-hint-stop",
                request_with_body("PATCH", "/balloon/hinting/stop", "not-json"),
                "The requested operation is not supported in Not started state: PatchBalloonHintingStop",
            ),
        ] {
            let response = request_over_socket(&mut vmm, socket_name, &request);

            assert!(
                response.starts_with("HTTP/1.1 400 Bad Request\r\n"),
                "{socket_name}: {response}"
            );
            assert!(
                response.contains(&format!(r#"{{"fault_message":"{fault_message}"}}"#)),
                "{socket_name}: {response}"
            );
        }
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::NotStarted
        );
    }

    #[test]
    fn running_state_rejects_balloon_endpoints_without_echoing_request_fields() {
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        let boot_body = r#"{"kernel_image_path":"/tmp/original-vmlinux"}"#;
        let boot_request = request_with_body("PUT", "/boot-source", boot_body);
        assert_eq!(
            handle_request_bytes(boot_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );
        let start_response = put_action_over_socket(&mut vmm, "balloon-start", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));

        for (socket_name, request, fault_message) in [
            (
                "bgr",
                "GET /balloon HTTP/1.1\r\nHost: localhost\r\n\r\n".to_string(),
                "Balloon device is not supported.",
            ),
            (
                "bpr",
                request_with_body(
                    "PUT",
                    "/balloon",
                    r#"{"amount_mib":222222222,"deflate_on_oom":true,"stats_polling_interval_s":333}"#,
                ),
                "The requested operation is not supported in Running state: PutBalloon",
            ),
            (
                "bpar",
                request_with_body("PATCH", "/balloon", r#"{"amount_mib":222222222}"#),
                "Balloon device is not supported.",
            ),
            (
                "bsgr",
                "GET /balloon/statistics HTTP/1.1\r\nHost: localhost\r\n\r\n".to_string(),
                "Balloon device is not supported.",
            ),
            (
                "bspr",
                request_with_body(
                    "PATCH",
                    "/balloon/statistics",
                    r#"{"stats_polling_interval_s":333}"#,
                ),
                "Balloon device is not supported.",
            ),
            (
                "bhsr",
                "GET /balloon/hinting/status HTTP/1.1\r\nHost: localhost\r\n\r\n".to_string(),
                "Balloon device is not supported.",
            ),
            (
                "bhsar",
                request_with_body(
                    "PATCH",
                    "/balloon/hinting/start",
                    r#"{"acknowledge_on_stop":false}"#,
                ),
                "Balloon device is not supported.",
            ),
            (
                "bhsor",
                request_with_body("PATCH", "/balloon/hinting/stop", "not-json"),
                "Balloon device is not supported.",
            ),
        ] {
            let response = request_over_socket(&mut vmm, socket_name, &request);

            assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
            assert!(response.contains(&format!(r#"{{"fault_message":"{fault_message}"}}"#)));
            for private_value in ["222222222", "333"] {
                assert!(
                    !response.contains(private_value),
                    "running-state balloon response must not echo {private_value}: {response}"
                );
            }
        }
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::Running
        );
    }

    #[test]
    fn configures_serial_endpoint_over_socket() {
        let path = unique_socket_path("serial-config");
        let serial_path = unique_socket_path("serial-output").with_extension("out");
        let body = format!(
            r#"{{"serial_out_path":"{}"}}"#,
            serial_path.to_string_lossy()
        );
        let request = format!(
            "PUT /serial HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let server = ApiServer::bind(&path).expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");

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
        assert_eq!(
            vmm.serial_config().serial_out_path(),
            Some(serial_path.as_path())
        );
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::NotStarted
        );
    }

    #[test]
    fn rejects_invalid_serial_endpoint_without_mutating() {
        let mut vmm = test_controller();
        let serial_path = unique_socket_path("serial-original").with_extension("out");
        let initial_body = format!(
            r#"{{"serial_out_path":"{}"}}"#,
            serial_path.to_string_lossy()
        );
        let initial_request = format!(
            "PUT /serial HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{initial_body}",
            initial_body.len()
        );
        assert_eq!(
            handle_request_bytes(initial_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );

        for (name, body, message) in [
            (
                "serial-empty-path",
                r#"{"serial_out_path":""}"#,
                "serial output path must not be empty",
            ),
            (
                "serial-control-path",
                "{\"serial_out_path\":\"/tmp/bad\\npath\"}",
                "serial output path must not contain control characters",
            ),
            (
                "serial-rate-limiter",
                r#"{"rate_limiter":{"bandwidth":{"size":1,"refill_time":1}}}"#,
                "serial output rate limiting is not supported",
            ),
        ] {
            let request = format!(
                "PUT /serial HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );

            let response = request_over_socket(&mut vmm, name, &request);

            assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
            assert!(response.contains(&format!(r#"{{"fault_message":"{message}"}}"#)));
            assert_eq!(
                vmm.serial_config().serial_out_path(),
                Some(serial_path.as_path())
            );
        }
    }

    #[test]
    fn running_state_rejects_serial_endpoint_without_mutating() {
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        let serial_path = unique_socket_path("serial-running-original").with_extension("out");
        let serial_body = format!(
            r#"{{"serial_out_path":"{}"}}"#,
            serial_path.to_string_lossy()
        );
        let serial_request = format!(
            "PUT /serial HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{serial_body}",
            serial_body.len()
        );
        assert_eq!(
            handle_request_bytes(serial_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );
        let boot_body = r#"{"kernel_image_path":"/tmp/original-vmlinux"}"#;
        let boot_request = format!(
            "PUT /boot-source HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{boot_body}",
            boot_body.len()
        );
        assert_eq!(
            handle_request_bytes(boot_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );
        let start_response = put_action_over_socket(&mut vmm, "serial-start", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));

        let replacement_body = r#"{"serial_out_path":"/tmp/replacement.out"}"#;
        let replacement_request = format!(
            "PUT /serial HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{replacement_body}",
            replacement_body.len()
        );

        let response = request_over_socket(&mut vmm, "serial-running", &replacement_request);

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(response.contains(
            r#"{"fault_message":"The requested operation is not supported in Running state: PutSerial"}"#
        ));
        assert_eq!(
            vmm.serial_config().serial_out_path(),
            Some(serial_path.as_path())
        );
    }

    #[test]
    fn not_started_state_rejects_vm_state_update_without_mutating() {
        let mut vmm = test_controller();
        let request = "PATCH /vm HTTP/1.1\r\nHost: localhost\r\nContent-Length: 18\r\n\r\n{\"state\":\"Paused\"}";

        let response = request_over_socket(&mut vmm, "vm-state-not-started", request);

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(response.contains(
            r#"{"fault_message":"The requested operation is not supported in Not started state: Pause"}"#
        ));
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::NotStarted
        );
    }

    #[test]
    fn rejects_malformed_vm_state_update_without_mutating() {
        let mut vmm = test_controller();
        let request = "PATCH /vm HTTP/1.1\r\nHost: localhost\r\nContent-Length: 19\r\n\r\n{\"state\":\"Running\"}";

        let response = request_over_socket(&mut vmm, "vm-state-malformed", request);

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(response.contains(r#"{"fault_message":"Malformed HTTP request."}"#));
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::NotStarted
        );
    }

    #[test]
    fn running_state_rejects_vm_state_update_without_mutating() {
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
        let start_response = put_action_over_socket(&mut vmm, "vm-state-start", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));
        let request = "PATCH /vm HTTP/1.1\r\nHost: localhost\r\nContent-Length: 19\r\n\r\n{\"state\":\"Resumed\"}";

        let response = request_over_socket(&mut vmm, "vm-state-running", request);

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(
            response.contains(
                r#"{"fault_message":"The requested operation is not supported: Resume"}"#
            )
        );
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::Running
        );
    }

    #[test]
    fn returns_fault_for_memory_hotplug_endpoints() {
        let mut vmm = test_controller();
        for (socket_name, request, fault_message) in [
            (
                "mh-get",
                "GET /hotplug/memory HTTP/1.1\r\nHost: localhost\r\n\r\n".to_string(),
                "The requested operation is not supported in Not started state: GetMemoryHotplug",
            ),
            (
                "mh-put",
                request_with_body("PUT", "/hotplug/memory", r#"{"total_size_mib":2048}"#),
                "Memory hotplug is not supported.",
            ),
            (
                "mh-put-bad",
                request_with_body("PUT", "/hotplug/memory", r#"{"size_mib":2048}"#),
                "Malformed HTTP request.",
            ),
            (
                "mh-patch",
                request_with_body("PATCH", "/hotplug/memory", r#"{"requested_size_mib":256}"#),
                "The requested operation is not supported in Not started state: PatchMemoryHotplug",
            ),
            (
                "mh-patch-bad",
                request_with_body("PATCH", "/hotplug/memory", "not-json"),
                "Malformed HTTP request.",
            ),
        ] {
            let response = request_over_socket(&mut vmm, socket_name, &request);

            assert!(
                response.starts_with("HTTP/1.1 400 Bad Request\r\n"),
                "{socket_name}: {response}"
            );
            assert!(
                response.contains(&format!(r#"{{"fault_message":"{fault_message}"}}"#)),
                "{socket_name}: {response}"
            );
        }
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::NotStarted
        );
    }

    #[test]
    fn running_state_rejects_memory_hotplug_endpoints_without_echoing_request_fields() {
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        let boot_body = r#"{"kernel_image_path":"/tmp/original-vmlinux"}"#;
        let boot_request = request_with_body("PUT", "/boot-source", boot_body);
        assert_eq!(
            handle_request_bytes(boot_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );
        let start_response = put_action_over_socket(&mut vmm, "mh-start", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));

        for (socket_name, request, fault_message) in [
            (
                "mhgr",
                "GET /hotplug/memory HTTP/1.1\r\nHost: localhost\r\n\r\n".to_string(),
                "Memory hotplug is not supported.",
            ),
            (
                "mhpr",
                request_with_body(
                    "PUT",
                    "/hotplug/memory",
                    r#"{"total_size_mib":222222222,"block_size_mib":2,"slot_size_mib":128}"#,
                ),
                "The requested operation is not supported in Running state: PutMemoryHotplug",
            ),
            (
                "mhpar",
                request_with_body(
                    "PATCH",
                    "/hotplug/memory",
                    r#"{"requested_size_mib":222222222}"#,
                ),
                "Memory hotplug is not supported.",
            ),
        ] {
            let response = request_over_socket(&mut vmm, socket_name, &request);

            assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
            assert!(response.contains(&format!(r#"{{"fault_message":"{fault_message}"}}"#)));
            assert!(
                !response.contains("222222222"),
                "memory hotplug response must not echo request sizes: {response}"
            );
        }
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::Running
        );
    }

    #[test]
    fn returns_fault_for_pmem_endpoints() {
        let mut vmm = test_controller();
        for (socket_name, request, fault_message) in [
            (
                "p-put",
                request_with_body(
                    "PUT",
                    "/pmem/pmem0",
                    r#"{"id":"pmem0","path_on_host":"/tmp/pmem.img"}"#,
                ),
                "Pmem device is not supported.",
            ),
            (
                "p-put-bad",
                request_with_body("PUT", "/pmem/pmem0", r#"{"id":"pmem0"}"#),
                "Malformed HTTP request.",
            ),
            (
                "p-patch",
                request_with_body("PATCH", "/pmem/pmem0", r#"{"id":"pmem0"}"#),
                "The requested operation is not supported in Not started state: PatchPmem",
            ),
            (
                "p-patch-mis",
                request_with_body("PATCH", "/pmem/pmem0", r#"{"id":"other"}"#),
                "path pmem id must match body id.",
            ),
            (
                "p-del",
                "DELETE /pmem/pmem0 HTTP/1.1\r\nHost: localhost\r\n\r\n".to_string(),
                "The requested operation is not supported in Not started state: HotUnplugDevice",
            ),
        ] {
            let response = request_over_socket(&mut vmm, socket_name, &request);

            assert!(
                response.starts_with("HTTP/1.1 400 Bad Request\r\n"),
                "{socket_name}: {response}"
            );
            assert!(
                response.contains(&format!(r#"{{"fault_message":"{fault_message}"}}"#)),
                "{socket_name}: {response}"
            );
        }
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::NotStarted
        );
    }

    #[test]
    fn running_state_rejects_pmem_endpoints_without_echoing_request_fields() {
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        let boot_body = r#"{"kernel_image_path":"/tmp/original-vmlinux"}"#;
        let boot_request = request_with_body("PUT", "/boot-source", boot_body);
        assert_eq!(
            handle_request_bytes(boot_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );
        let start_response = put_action_over_socket(&mut vmm, "pmem-start", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));

        for (socket_name, request, fault_message) in [
            (
                "pmem-put-running",
                request_with_body(
                    "PUT",
                    "/pmem/pmem0",
                    r#"{"id":"pmem0","path_on_host":"/private/tmp/pmem.img","root_device":true,"read_only":false,"rate_limiter":{"bandwidth":{"size":123456,"one_time_burst":234567,"refill_time":345678}}}"#,
                ),
                "The requested operation is not supported in Running state: PutPmem",
            ),
            (
                "pmem-patch-running",
                request_with_body(
                    "PATCH",
                    "/pmem/pmem0",
                    r#"{"id":"pmem0","rate_limiter":{"ops":{"size":123456,"one_time_burst":234567,"refill_time":345678}}}"#,
                ),
                "Pmem device is not supported.",
            ),
        ] {
            let response = request_over_socket(&mut vmm, socket_name, &request);

            assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
            assert!(response.contains(&format!(r#"{{"fault_message":"{fault_message}"}}"#)));
            for private_value in ["/private/tmp/pmem.img", "123456", "234567", "345678"] {
                assert!(
                    !response.contains(private_value),
                    "running-state pmem response must not echo {private_value}: {response}"
                );
            }
        }
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::Running
        );
    }

    #[test]
    fn running_state_rejects_hot_unplug_endpoints_without_echoing_ids() {
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        let boot_body = r#"{"kernel_image_path":"/tmp/original-vmlinux"}"#;
        let boot_request = request_with_body("PUT", "/boot-source", boot_body);
        assert_eq!(
            handle_request_bytes(boot_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );
        let start_response = put_action_over_socket(&mut vmm, "hot-unplug-start", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));

        for (socket_name, request, fault_message, private_value) in [
            (
                "drive-delete-running",
                "DELETE /drives/rootfs HTTP/1.1\r\nHost: localhost\r\n\r\n",
                "Drive updates are not supported.",
                "rootfs",
            ),
            (
                "net-delete-running",
                "DELETE /network-interfaces/eth0 HTTP/1.1\r\nHost: localhost\r\n\r\n",
                "Network interface updates are not supported.",
                "eth0",
            ),
            (
                "pmem-delete-running",
                "DELETE /pmem/pmem0 HTTP/1.1\r\nHost: localhost\r\n\r\n",
                "Pmem device is not supported.",
                "pmem0",
            ),
        ] {
            let response = request_over_socket(&mut vmm, socket_name, request);

            assert!(
                response.starts_with("HTTP/1.1 400 Bad Request\r\n"),
                "{socket_name}: {response}"
            );
            assert!(
                response.contains(&format!(r#"{{"fault_message":"{fault_message}"}}"#)),
                "{socket_name}: {response}"
            );
            assert!(
                !response.contains(private_value),
                "running hot-unplug response must not echo {private_value}: {response}"
            );
        }
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::Running
        );
    }

    #[test]
    fn returns_fault_for_device_update_endpoints() {
        for (name, method, path, body, fault_message) in [
            (
                "drive-delete",
                "DELETE",
                "/drives/rootfs",
                "",
                r#"{"fault_message":"The requested operation is not supported in Not started state: HotUnplugDevice"}"#,
            ),
            (
                "drive-delete-body",
                "DELETE",
                "/drives/rootfs",
                "{}",
                r#"{"fault_message":"Malformed HTTP request."}"#,
            ),
            (
                "net",
                "PATCH",
                "/network-interfaces/eth0",
                r#"{"iface_id":"eth0"}"#,
                r#"{"fault_message":"The requested operation is not supported in Not started state: UpdateNetworkInterface"}"#,
            ),
            (
                "net-delete",
                "DELETE",
                "/network-interfaces/eth0",
                "",
                r#"{"fault_message":"The requested operation is not supported in Not started state: HotUnplugDevice"}"#,
            ),
            (
                "pmem-delete",
                "DELETE",
                "/pmem/pmem0",
                "",
                r#"{"fault_message":"The requested operation is not supported in Not started state: HotUnplugDevice"}"#,
            ),
        ] {
            let socket_name = format!("du-{name}");
            let socket_path = unique_socket_path(&socket_name);
            let server = ApiServer::bind(&socket_path).expect("server should bind");
            let mut client = UnixStream::connect(&socket_path).expect("client should connect");
            let request = format!(
                "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
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
            assert!(response.contains(fault_message));
            assert_eq!(
                vmm.instance_info().state,
                bangbang_runtime::InstanceState::NotStarted
            );
        }
    }

    #[test]
    fn returns_fault_for_send_ctrl_alt_del_action() {
        let path = unique_socket_path("cad-fault");
        let server = ApiServer::bind(&path).expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");
        let body = r#"{"action_type":"SendCtrlAltDel"}"#;
        let request = format!(
            "PUT /actions HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
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
            response.contains(r#"{"fault_message":"SendCtrlAltDel is not supported on aarch64."}"#)
        );
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::NotStarted
        );
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

        let mut client = UnixStream::connect(&path).expect("client should connect again");
        let patch_body = r#"{"mem_size_mib":512}"#;
        let patch_request = format!(
            "PATCH /machine-config HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{patch_body}",
            patch_body.len()
        );

        client
            .write_all(patch_request.as_bytes())
            .expect("client should write patch request");
        server
            .serve_next(&mut vmm)
            .expect("server should handle one patch request");

        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("client should read patch response");

        assert!(response.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert!(response.contains("Content-Length: 0\r\n"));
        assert!(response.ends_with("\r\n\r\n"));
        assert_eq!(vmm.machine_config().vcpu_count(), 2);
        assert_eq!(vmm.machine_config().mem_size_mib(), 512);

        let mut client =
            UnixStream::connect(&path).expect("client should connect for unsupported patch");
        let patch_body = r#"{"track_dirty_pages":true}"#;
        let patch_request = format!(
            "PATCH /machine-config HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{patch_body}",
            patch_body.len()
        );

        client
            .write_all(patch_request.as_bytes())
            .expect("client should write unsupported patch request");
        server
            .serve_next(&mut vmm)
            .expect("server should handle unsupported patch request");

        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("client should read unsupported patch response");

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(
            response.contains(r#"{"fault_message":"machine track_dirty_pages is not supported"}"#)
        );
        assert_eq!(vmm.machine_config().vcpu_count(), 2);
        assert_eq!(vmm.machine_config().mem_size_mib(), 512);
        assert!(!vmm.machine_config().track_dirty_pages());
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
    fn returns_fault_for_configured_drive_rate_limiter_without_storing() {
        let body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/rootfs.ext4",
            "is_root_device": true,
            "rate_limiter": {
                "bandwidth": {
                    "size": 1000,
                    "one_time_burst": 1000,
                    "refill_time": 100
                }
            }
        }"#;
        let request = format!(
            "PUT /drives/rootfs HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let mut vmm = test_controller();

        let response = request_over_socket(&mut vmm, "drive-rate-limiter", &request);

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(response.contains(r#"{"fault_message":"drive rate_limiter is not supported"}"#));
        assert!(vmm.drive_configs().is_empty());
    }

    #[test]
    fn returns_state_fault_for_preboot_drive_patch_without_mutating() {
        let mut vmm = test_controller();
        vmm.handle_action(VmmAction::PutDrive(
            DriveConfigInput::new("rootfs", "rootfs", "/tmp/rootfs.ext4", true)
                .with_is_read_only(true),
        ))
        .expect("initial drive should configure");
        let body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/replaced.ext4",
            "rate_limiter": null
        }"#;
        let request = format!(
            "PATCH /drives/rootfs HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        let response = request_over_socket(&mut vmm, "drive-patch-preboot", &request);

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(response.contains(
            r#"{"fault_message":"The requested operation is not supported in Not started state: UpdateBlockDevice"}"#
        ));
        assert_eq!(vmm.drive_configs().len(), 1);
        let config = &vmm.drive_configs()[0];
        assert_eq!(
            config.path_on_host(),
            std::path::Path::new("/tmp/rootfs.ext4")
        );
        assert!(config.is_read_only());
    }

    #[test]
    fn running_state_accepts_drive_patch_and_updates_stored_config() {
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        vmm.handle_action(VmmAction::PutDrive(
            DriveConfigInput::new("rootfs", "rootfs", "/tmp/rootfs.ext4", true)
                .with_is_read_only(true),
        ))
        .expect("initial drive should configure");
        let boot_body = r#"{"kernel_image_path":"/tmp/original-vmlinux"}"#;
        let boot_request = format!(
            "PUT /boot-source HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{boot_body}",
            boot_body.len()
        );
        assert_eq!(
            handle_request_bytes(boot_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );
        let start_response = put_action_over_socket(&mut vmm, "drive-patch-start", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));
        let body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/replaced.ext4"
        }"#;
        let request = format!(
            "PATCH /drives/rootfs HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        let response = request_over_socket(&mut vmm, "drive-patch-running", &request);

        assert!(response.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert!(response.contains("Content-Length: 0\r\n"));
        assert!(response.ends_with("\r\n\r\n"));
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::Running
        );
        assert_eq!(vmm.drive_configs().len(), 1);
        let config = &vmm.drive_configs()[0];
        assert_eq!(
            config.path_on_host(),
            std::path::Path::new("/tmp/replaced.ext4")
        );
        assert!(config.is_read_only());
    }

    #[test]
    fn running_state_rejects_drive_patch_rate_limiter_without_mutating() {
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        vmm.handle_action(VmmAction::PutDrive(
            DriveConfigInput::new("rootfs", "rootfs", "/tmp/rootfs.ext4", true)
                .with_is_read_only(true),
        ))
        .expect("initial drive should configure");
        let boot_body = r#"{"kernel_image_path":"/tmp/original-vmlinux"}"#;
        let boot_request = format!(
            "PUT /boot-source HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{boot_body}",
            boot_body.len()
        );
        assert_eq!(
            handle_request_bytes(boot_request.as_bytes(), &mut vmm).status(),
            bangbang_api::http::StatusCode::NoContent
        );
        let start_response = put_action_over_socket(&mut vmm, "dprl-start", "InstanceStart");
        assert!(start_response.starts_with("HTTP/1.1 204 No Content\r\n"));
        let body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/rejected.ext4",
            "rate_limiter": {
                "bandwidth": {
                    "size": 1000,
                    "one_time_burst": 1000,
                    "refill_time": 100
                }
            }
        }"#;
        let request = format!(
            "PATCH /drives/rootfs HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        let response = request_over_socket(&mut vmm, "dprl", &request);

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(response.contains(r#"{"fault_message":"drive rate_limiter is not supported"}"#));
        assert!(!response.contains("/tmp/rejected.ext4"));
        assert_eq!(
            vmm.instance_info().state,
            bangbang_runtime::InstanceState::Running
        );
        assert_eq!(vmm.drive_configs().len(), 1);
        let config = &vmm.drive_configs()[0];
        assert_eq!(
            config.path_on_host(),
            std::path::Path::new("/tmp/rootfs.ext4")
        );
        assert!(config.is_read_only());
    }

    #[test]
    fn stores_network_mtu() {
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

        assert_eq!(response.status(), bangbang_api::http::StatusCode::NoContent);

        let config_response = handle_request_bytes(
            b"GET /vm/config HTTP/1.1\r\nHost: localhost\r\n\r\n",
            &mut vmm,
        );
        assert_eq!(config_response.status(), bangbang_api::http::StatusCode::Ok);
        assert!(config_response.body().contains(r#""iface_id":"eth0""#));
        assert!(config_response.body().contains(r#""host_dev_name":"tap0""#));
        assert!(config_response.body().contains(r#""mtu":1500"#));
    }

    #[test]
    fn returns_fault_for_configured_network_rate_limiters_without_storing() {
        for (field, message, socket_name) in [
            (
                "rx_rate_limiter",
                "network rx_rate_limiter is not supported",
                "net-rx-rate-limiter",
            ),
            (
                "tx_rate_limiter",
                "network tx_rate_limiter is not supported",
                "net-tx-rate-limiter",
            ),
        ] {
            let body = format!(
                r#"{{
                    "iface_id": "eth0",
                    "host_dev_name": "tap0",
                    "{field}": {{
                        "ops": {{
                            "size": 1000,
                            "one_time_burst": 1000,
                            "refill_time": 100
                        }}
                    }}
                }}"#
            );
            let request = format!(
                "PUT /network-interfaces/eth0 HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let mut vmm = test_controller();

            let response = request_over_socket(&mut vmm, socket_name, &request);

            assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
            assert!(response.contains(&format!(r#"{{"fault_message":"{message}"}}"#)));
            let data = vmm
                .handle_action(VmmAction::GetVmConfig)
                .expect("VM config should be returned");
            let VmmData::VmConfiguration(config) = data else {
                panic!("expected VM config");
            };
            assert!(config.network_interface_configs().is_empty());
        }
    }

    #[test]
    fn returns_fault_for_invalid_network_mtu_without_storing() {
        let mut vmm = test_controller();
        vmm.handle_action(VmmAction::PutNetworkInterface(
            NetworkInterfaceConfigInput::new("eth0", "eth0", "tap0").with_mtu(1500),
        ))
        .expect("initial network config should be stored");
        let body = r#"{
            "iface_id": "eth0",
            "host_dev_name": "tap0",
            "mtu": 67
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
            r#"{"fault_message":"network mtu 67 is out of range [68, 65535]"}"#
        );

        let config_response = handle_request_bytes(
            b"GET /vm/config HTTP/1.1\r\nHost: localhost\r\n\r\n",
            &mut vmm,
        );
        assert_eq!(config_response.status(), bangbang_api::http::StatusCode::Ok);
        assert!(config_response.body().contains(r#""iface_id":"eth0""#));
        assert!(config_response.body().contains(r#""host_dev_name":"tap0""#));
        assert!(config_response.body().contains(r#""mtu":1500"#));
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
    fn configures_writeback_drive_over_unix_socket() {
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

        assert!(response.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert!(response.contains("Content-Length: 0\r\n"));
        assert!(response.ends_with("\r\n\r\n"));
        assert_eq!(vmm.drive_configs().len(), 1);
        let config = &vmm.drive_configs()[0];
        assert_eq!(config.drive_id(), "rootfs");
        assert_eq!(config.path_on_host(), PathBuf::from("/tmp/rootfs.ext4"));
        assert!(config.is_root_device());
        assert_eq!(config.cache_type(), DriveCacheType::Writeback);
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
    fn returns_fault_for_request_over_configured_payload_limit_without_mutation() {
        let path = unique_socket_path("small-limit");
        let logger_path = unique_socket_path("small-limit-output").with_extension("log");
        let server = ApiServer::bind_with_max_payload_size(&path, 64).expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");
        let body = format!(
            r#"{{"log_path":"{}","module":"{}"}}"#,
            logger_path.to_string_lossy(),
            "a".repeat(64)
        );
        let request = format!(
            "PUT /logger HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
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
            response.contains(
                r#"{"fault_message":"HTTP request payload exceeds the configured limit."}"#
            )
        );
        assert!(!logger_path.exists());
    }

    #[test]
    fn accepts_request_above_default_with_configured_payload_limit() {
        let path = unique_socket_path("large-limit");
        let module = "a".repeat(HTTP_MAX_PAYLOAD_SIZE);
        let body = format!(r#"{{"module":"{module}"}}"#);
        let request = format!(
            "PUT /logger HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        assert!(request.len() > HTTP_MAX_PAYLOAD_SIZE);
        let server = ApiServer::bind_with_max_payload_size(&path, request.len())
            .expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");
        let client_handle = thread::spawn(move || {
            client
                .write_all(request.as_bytes())
                .expect("client should write request");
            let mut response = String::new();
            client
                .read_to_string(&mut response)
                .expect("client should read response");
            response
        });
        let mut vmm = test_controller();
        server
            .serve_next(&mut vmm)
            .expect("server should handle one request");

        let response = client_handle
            .join()
            .expect("client thread should not panic");

        assert!(response.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert!(response.contains("Content-Length: 0\r\n"));
        assert!(response.ends_with("\r\n\r\n"));
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
    fn run_until_periodic_metrics_timeout_flushes_after_start() {
        let path = unique_socket_path("periodic-metrics");
        let metrics_path = unique_socket_path("periodic-metrics-output").with_extension("metrics");
        let server = ApiServer::bind(&path).expect("server should bind");
        let (mut shutdown_reader, shutdown_writer) =
            UnixStream::pair().expect("shutdown stream pair should be created");
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        vmm.handle_action(VmmAction::PutMetrics(MetricsConfigInput::new(
            &metrics_path,
        )))
        .expect("metrics should configure");
        vmm.handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            "/tmp/vmlinux",
        )))
        .expect("boot source should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("instance should start");
        let mut vmm = ShutdownAfterPeriodicFlush::new(vmm, &shutdown_writer);

        assert_eq!(
            server.run_until_with_periodic_metrics_scheduler(
                &mut vmm,
                &mut shutdown_reader,
                PeriodicMetricsScheduler::due_now(Instant::now()),
            ),
            Ok(())
        );

        assert_eq!(
            fs::read_to_string(&metrics_path).expect("metrics output should be readable"),
            "{\"vmm\":{\"metrics_flush_count\":1}}\n"
        );
        fs::remove_file(metrics_path).expect("fixture should clean up");
        drop(server);
        assert!(!path.exists());
    }

    #[test]
    fn run_until_periodic_metrics_timeout_before_start_does_not_write() {
        let path = unique_socket_path("periodic-pre-start");
        let metrics_path =
            unique_socket_path("periodic-pre-start-output").with_extension("metrics");
        let server = ApiServer::bind(&path).expect("server should bind");
        let (mut shutdown_reader, shutdown_writer) =
            UnixStream::pair().expect("shutdown stream pair should be created");
        let mut vmm = test_controller();
        vmm.handle_action(VmmAction::PutMetrics(MetricsConfigInput::new(
            &metrics_path,
        )))
        .expect("metrics should configure");
        let mut vmm = ShutdownAfterPeriodicFlush::new(vmm, &shutdown_writer);

        assert_eq!(
            server.run_until_with_periodic_metrics_scheduler(
                &mut vmm,
                &mut shutdown_reader,
                PeriodicMetricsScheduler::due_now(Instant::now()),
            ),
            Ok(())
        );

        assert_eq!(
            fs::read_to_string(&metrics_path).expect("metrics output should be readable"),
            ""
        );
        fs::remove_file(metrics_path).expect("fixture should clean up");
        drop(server);
        assert!(!path.exists());
    }

    #[test]
    fn run_until_periodic_metrics_timeout_without_configuration_is_noop() {
        let path = unique_socket_path("periodic-unconfigured");
        let server = ApiServer::bind(&path).expect("server should bind");
        let (mut shutdown_reader, shutdown_writer) =
            UnixStream::pair().expect("shutdown stream pair should be created");
        let mut vmm = test_controller_with_starter(TestInstanceStarter::success());
        vmm.handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            "/tmp/vmlinux",
        )))
        .expect("boot source should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("instance should start");
        let mut vmm = ShutdownAfterPeriodicFlush::new(vmm, &shutdown_writer);

        assert_eq!(
            server.run_until_with_periodic_metrics_scheduler(
                &mut vmm,
                &mut shutdown_reader,
                PeriodicMetricsScheduler::due_now(Instant::now()),
            ),
            Ok(())
        );
        drop(server);
        assert!(!path.exists());
    }

    #[test]
    fn run_until_cleans_idle_socket_after_guest_requested_stop() {
        let path = unique_socket_path("idle-guest-requested-stop");
        let server = ApiServer::bind(&path).expect("server should bind");
        let (mut shutdown_reader, _shutdown_writer) =
            UnixStream::pair().expect("shutdown stream pair should be created");
        let process_exit_signal = TestProcessExitSignal::new();
        let process_exit_trigger = process_exit_signal.clone();
        let mut vmm = test_controller_with_starter(
            TestInstanceStarter::success_with_process_exit_signal(process_exit_signal),
        );
        vmm.handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            "/tmp/vmlinux",
        )))
        .expect("boot source should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("instance should start");
        let handle = thread::spawn(move || server.run_until(&mut vmm, &mut shutdown_reader));

        process_exit_trigger.trigger(ProcessSessionExitStatus::GuestRequestedStop);

        assert_eq!(
            handle.join().expect("server thread should not panic"),
            Ok(())
        );
        assert!(!path.exists());
    }

    #[test]
    fn run_until_fails_and_cleans_idle_socket_after_process_terminal_status() {
        let path = unique_socket_path("idle-process-terminal");
        let server = ApiServer::bind(&path).expect("server should bind");
        let (mut shutdown_reader, _shutdown_writer) =
            UnixStream::pair().expect("shutdown stream pair should be created");
        let process_exit_signal = TestProcessExitSignal::new();
        let process_exit_trigger = process_exit_signal.clone();
        let mut vmm = test_controller_with_starter(
            TestInstanceStarter::success_with_process_exit_signal(process_exit_signal),
        );
        vmm.handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            "/tmp/vmlinux",
        )))
        .expect("boot source should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("instance should start");
        let handle = thread::spawn(move || server.run_until(&mut vmm, &mut shutdown_reader));

        process_exit_trigger.trigger(ProcessSessionExitStatus::Terminal);

        assert_eq!(
            handle.join().expect("server thread should not panic"),
            Err(ApiServerError::ProcessSessionTerminal)
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
