use std::fmt;

use serde::{Deserialize, Deserializer};

use crate::HTTP_MAX_PAYLOAD_SIZE;
use crate::route::Endpoint;

const MAX_HEADERS: usize = 32;
const RATE_LIMITER_BANDWIDTH_FIELD: &str = "bandwidth";
const RATE_LIMITER_OPS_FIELD: &str = "ops";
const TOKEN_BUCKET_SIZE_FIELD: &str = "size";
const TOKEN_BUCKET_ONE_TIME_BURST_FIELD: &str = "one_time_burst";
const TOKEN_BUCKET_REFILL_TIME_FIELD: &str = "refill_time";
const MAX_MACHINE_CONFIG_VCPUS: u8 = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiRequest {
    GetInstanceInfo,
    GetMachineConfig,
    GetVmConfig,
    GetVersion,
    PutAction(Box<ActionRequest>),
    PutBootSource(Box<BootSourceRequest>),
    PutDrive(Box<DriveConfigRequest>),
    PutLogger(Box<LoggerConfigRequest>),
    PutMachineConfig(Box<MachineConfigRequest>),
    PutMetrics(Box<MetricsConfigRequest>),
    PutNetworkInterface(Box<NetworkInterfaceConfigRequest>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestError {
    GetRequestBody,
    InvalidPathMethod,
    MismatchedDriveId,
    MismatchedInterfaceId,
    MalformedRequest,
    PayloadTooLarge,
    SendCtrlAltDelUnsupported,
}

impl RequestError {
    pub fn fault_message(&self) -> &'static str {
        match self {
            Self::GetRequestBody => "GET request cannot have a body.",
            Self::InvalidPathMethod => "Invalid request method and/or path.",
            Self::MismatchedDriveId => "path drive_id must match body drive_id.",
            Self::MismatchedInterfaceId => "path iface_id must match body iface_id.",
            Self::MalformedRequest => "Malformed HTTP request.",
            Self::PayloadTooLarge => "HTTP request payload exceeds the configured limit.",
            Self::SendCtrlAltDelUnsupported => "SendCtrlAltDel does not supported on aarch64.",
        }
    }
}

impl fmt::Display for RequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.fault_message())
    }
}

impl std::error::Error for RequestError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionRequest {
    action_type: ActionType,
}

impl ActionRequest {
    pub const fn action_type(&self) -> ActionType {
        self.action_type
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionType {
    FlushMetrics,
    InstanceStart,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionRequestBody {
    action_type: ActionTypeBody,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
enum ActionTypeBody {
    FlushMetrics,
    InstanceStart,
    SendCtrlAltDel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootSourceRequest {
    kernel_image_path: String,
    initrd_path: Option<String>,
    boot_args: Option<String>,
}

impl BootSourceRequest {
    pub fn kernel_image_path(&self) -> &str {
        &self.kernel_image_path
    }

    pub fn initrd_path(&self) -> Option<&str> {
        self.initrd_path.as_deref()
    }

    pub fn boot_args(&self) -> Option<&str> {
        self.boot_args.as_deref()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BootSourceRequestBody {
    kernel_image_path: String,
    initrd_path: Option<String>,
    boot_args: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggerConfigRequest {
    log_path: Option<String>,
    level: Option<LoggerLevel>,
    show_level: Option<bool>,
    show_log_origin: Option<bool>,
    module: Option<String>,
}

impl LoggerConfigRequest {
    pub fn log_path(&self) -> Option<&str> {
        self.log_path.as_deref()
    }

    pub const fn level(&self) -> Option<LoggerLevel> {
        self.level
    }

    pub const fn show_level(&self) -> Option<bool> {
        self.show_level
    }

    pub const fn show_log_origin(&self) -> Option<bool> {
        self.show_log_origin
    }

    pub fn module(&self) -> Option<&str> {
        self.module.as_deref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggerLevel {
    Off,
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl<'de> Deserialize<'de> for LoggerLevel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        match value.to_ascii_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "trace" => Ok(Self::Trace),
            "debug" => Ok(Self::Debug),
            "info" => Ok(Self::Info),
            "warn" | "warning" => Ok(Self::Warn),
            "error" => Ok(Self::Error),
            _ => Err(serde::de::Error::custom("invalid logger level")),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LoggerConfigRequestBody {
    #[serde(default)]
    log_path: Option<String>,
    #[serde(default)]
    level: Option<LoggerLevel>,
    #[serde(default)]
    show_level: Option<bool>,
    #[serde(default)]
    show_log_origin: Option<bool>,
    #[serde(default)]
    module: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineConfigRequest {
    vcpu_count: u8,
    mem_size_mib: u64,
    smt: bool,
    cpu_template: Option<MachineConfigCpuTemplate>,
    track_dirty_pages: bool,
    huge_pages: MachineConfigHugePages,
}

impl MachineConfigRequest {
    pub const fn vcpu_count(&self) -> u8 {
        self.vcpu_count
    }

    pub const fn mem_size_mib(&self) -> u64 {
        self.mem_size_mib
    }

    pub const fn smt(&self) -> bool {
        self.smt
    }

    pub const fn cpu_template(&self) -> Option<MachineConfigCpuTemplate> {
        self.cpu_template
    }

    pub const fn track_dirty_pages(&self) -> bool {
        self.track_dirty_pages
    }

    pub const fn huge_pages(&self) -> MachineConfigHugePages {
        self.huge_pages
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum MachineConfigCpuTemplate {
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
pub enum MachineConfigHugePages {
    #[default]
    None,
    #[serde(rename = "2M")]
    TwoM,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MachineConfigRequestBody {
    vcpu_count: u8,
    mem_size_mib: u64,
    #[serde(default)]
    smt: bool,
    #[serde(default)]
    cpu_template: Option<MachineConfigCpuTemplate>,
    #[serde(default)]
    track_dirty_pages: bool,
    #[serde(default)]
    huge_pages: MachineConfigHugePages,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricsConfigRequest {
    metrics_path: String,
}

impl MetricsConfigRequest {
    pub fn metrics_path(&self) -> &str {
        &self.metrics_path
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MetricsConfigRequestBody {
    metrics_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveConfigRequest {
    path_drive_id: String,
    body_drive_id: String,
    path_on_host: String,
    is_root_device: bool,
    is_read_only: Option<bool>,
    partuuid: Option<String>,
    cache_type: Option<DriveCacheType>,
    io_engine: Option<DriveIoEngine>,
    rate_limiter_configured: bool,
    socket: Option<String>,
}

impl DriveConfigRequest {
    pub fn path_drive_id(&self) -> &str {
        &self.path_drive_id
    }

    pub fn body_drive_id(&self) -> &str {
        &self.body_drive_id
    }

    pub fn path_on_host(&self) -> &str {
        &self.path_on_host
    }

    pub const fn is_root_device(&self) -> bool {
        self.is_root_device
    }

    pub const fn is_read_only(&self) -> Option<bool> {
        self.is_read_only
    }

    pub fn partuuid(&self) -> Option<&str> {
        self.partuuid.as_deref()
    }

    pub const fn cache_type(&self) -> Option<DriveCacheType> {
        self.cache_type
    }

    pub const fn io_engine(&self) -> Option<DriveIoEngine> {
        self.io_engine
    }

    pub const fn rate_limiter_configured(&self) -> bool {
        self.rate_limiter_configured
    }

    pub fn socket(&self) -> Option<&str> {
        self.socket.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkInterfaceConfigRequest {
    path_iface_id: String,
    body_iface_id: String,
    host_dev_name: String,
    guest_mac: Option<String>,
    mtu_configured: bool,
    rx_rate_limiter_configured: bool,
    tx_rate_limiter_configured: bool,
}

impl NetworkInterfaceConfigRequest {
    pub fn path_iface_id(&self) -> &str {
        &self.path_iface_id
    }

    pub fn body_iface_id(&self) -> &str {
        &self.body_iface_id
    }

    pub fn host_dev_name(&self) -> &str {
        &self.host_dev_name
    }

    pub fn guest_mac(&self) -> Option<&str> {
        self.guest_mac.as_deref()
    }

    pub const fn mtu_configured(&self) -> bool {
        self.mtu_configured
    }

    pub const fn rx_rate_limiter_configured(&self) -> bool {
        self.rx_rate_limiter_configured
    }

    pub const fn tx_rate_limiter_configured(&self) -> bool {
        self.tx_rate_limiter_configured
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum DriveCacheType {
    Unsafe,
    Writeback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum DriveIoEngine {
    Sync,
    Async,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineConfigResponse {
    vcpu_count: u8,
    mem_size_mib: u64,
    smt: bool,
    track_dirty_pages: bool,
    huge_pages: String,
}

impl MachineConfigResponse {
    pub fn new(
        vcpu_count: u8,
        mem_size_mib: u64,
        smt: bool,
        track_dirty_pages: bool,
        huge_pages: impl Into<String>,
    ) -> Self {
        Self {
            vcpu_count,
            mem_size_mib,
            smt,
            track_dirty_pages,
            huge_pages: huge_pages.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootSourceResponse {
    kernel_image_path: String,
    initrd_path: Option<String>,
    boot_args: Option<String>,
}

impl BootSourceResponse {
    pub fn new(kernel_image_path: impl Into<String>) -> Self {
        Self {
            kernel_image_path: kernel_image_path.into(),
            initrd_path: None,
            boot_args: None,
        }
    }

    pub fn with_initrd_path(mut self, initrd_path: impl Into<String>) -> Self {
        self.initrd_path = Some(initrd_path.into());
        self
    }

    pub fn with_boot_args(mut self, boot_args: impl Into<String>) -> Self {
        self.boot_args = Some(boot_args.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveConfigResponse {
    drive_id: String,
    path_on_host: String,
    is_root_device: bool,
    is_read_only: bool,
    partuuid: Option<String>,
    cache_type: String,
    io_engine: String,
}

impl DriveConfigResponse {
    pub fn new(
        drive_id: impl Into<String>,
        path_on_host: impl Into<String>,
        is_root_device: bool,
        is_read_only: bool,
        cache_type: impl Into<String>,
        io_engine: impl Into<String>,
    ) -> Self {
        Self {
            drive_id: drive_id.into(),
            path_on_host: path_on_host.into(),
            is_root_device,
            is_read_only,
            partuuid: None,
            cache_type: cache_type.into(),
            io_engine: io_engine.into(),
        }
    }

    pub fn with_partuuid(mut self, partuuid: impl Into<String>) -> Self {
        self.partuuid = Some(partuuid.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkInterfaceConfigResponse {
    iface_id: String,
    host_dev_name: String,
    guest_mac: Option<String>,
}

impl NetworkInterfaceConfigResponse {
    pub fn new(iface_id: impl Into<String>, host_dev_name: impl Into<String>) -> Self {
        Self {
            iface_id: iface_id.into(),
            host_dev_name: host_dev_name.into(),
            guest_mac: None,
        }
    }

    pub fn with_guest_mac(mut self, guest_mac: impl Into<String>) -> Self {
        self.guest_mac = Some(guest_mac.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmConfigResponse {
    machine_config: MachineConfigResponse,
    boot_source: Option<BootSourceResponse>,
    drives: Vec<DriveConfigResponse>,
    network_interfaces: Vec<NetworkInterfaceConfigResponse>,
}

impl VmConfigResponse {
    pub fn new(
        machine_config: MachineConfigResponse,
        boot_source: Option<BootSourceResponse>,
        drives: Vec<DriveConfigResponse>,
        network_interfaces: Vec<NetworkInterfaceConfigResponse>,
    ) -> Self {
        Self {
            machine_config,
            boot_source,
            drives,
            network_interfaces,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DriveConfigRequestBody {
    drive_id: String,
    path_on_host: String,
    is_root_device: bool,
    #[serde(default)]
    is_read_only: Option<bool>,
    #[serde(default)]
    partuuid: Option<String>,
    #[serde(default)]
    cache_type: Option<DriveCacheType>,
    #[serde(default, rename = "io_engine")]
    io_engine: Option<DriveIoEngine>,
    #[serde(default)]
    rate_limiter: Option<serde_json::Value>,
    #[serde(default)]
    socket: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NetworkInterfaceConfigRequestBody {
    iface_id: String,
    host_dev_name: String,
    #[serde(default)]
    guest_mac: Option<String>,
    #[serde(default)]
    mtu: Option<u16>,
    #[serde(default)]
    rx_rate_limiter: Option<serde_json::Value>,
    #[serde(default)]
    tx_rate_limiter: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusCode {
    Ok,
    NoContent,
    BadRequest,
}

impl StatusCode {
    pub const fn as_u16(self) -> u16 {
        match self {
            Self::Ok => 200,
            Self::NoContent => 204,
            Self::BadRequest => 400,
        }
    }

    const fn reason_phrase(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::NoContent => "No Content",
            Self::BadRequest => "Bad Request",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    status: StatusCode,
    body: String,
}

impl HttpResponse {
    pub fn instance_info(id: &str, state: &str, vmm_version: &str, app_name: &str) -> Self {
        let body = serde_json::json!({
            "app_name": app_name,
            "id": id,
            "state": state,
            "vmm_version": vmm_version,
        })
        .to_string();

        Self {
            status: StatusCode::Ok,
            body,
        }
    }

    pub fn version(version: &str) -> Self {
        let body = serde_json::json!({ "firecracker_version": version }).to_string();

        Self {
            status: StatusCode::Ok,
            body,
        }
    }

    pub fn machine_config(
        vcpu_count: u8,
        mem_size_mib: u64,
        smt: bool,
        track_dirty_pages: bool,
        huge_pages: &str,
    ) -> Self {
        let body = serde_json::json!({
            "huge_pages": huge_pages,
            "mem_size_mib": mem_size_mib,
            "smt": smt,
            "track_dirty_pages": track_dirty_pages,
            "vcpu_count": vcpu_count,
        })
        .to_string();

        Self {
            status: StatusCode::Ok,
            body,
        }
    }

    pub fn vm_config(config: &VmConfigResponse) -> Self {
        let mut body = serde_json::Map::new();
        if let Some(boot_source) = &config.boot_source {
            body.insert(
                "boot-source".to_string(),
                boot_source_response_value(boot_source),
            );
        }
        body.insert(
            "drives".to_string(),
            serde_json::Value::Array(
                config
                    .drives
                    .iter()
                    .map(drive_config_response_value)
                    .collect(),
            ),
        );
        body.insert(
            "machine-config".to_string(),
            machine_config_response_value(&config.machine_config),
        );
        body.insert(
            "network-interfaces".to_string(),
            serde_json::Value::Array(
                config
                    .network_interfaces
                    .iter()
                    .map(network_interface_config_response_value)
                    .collect(),
            ),
        );

        Self {
            status: StatusCode::Ok,
            body: serde_json::Value::Object(body).to_string(),
        }
    }

    pub fn fault(message: &str) -> Self {
        let body = serde_json::json!({ "fault_message": message }).to_string();

        Self {
            status: StatusCode::BadRequest,
            body,
        }
    }

    pub fn no_content() -> Self {
        Self {
            status: StatusCode::NoContent,
            body: String::new(),
        }
    }

    pub const fn status(&self) -> StatusCode {
        self.status
    }

    pub fn body(&self) -> &str {
        &self.body
    }

    pub fn to_http_bytes(&self) -> Vec<u8> {
        let content_type = if self.body.is_empty() {
            ""
        } else {
            "Content-Type: application/json\r\n"
        };

        format!(
            "HTTP/1.1 {} {}\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
            self.status.as_u16(),
            self.status.reason_phrase(),
            content_type,
            self.body.len(),
            self.body
        )
        .into_bytes()
    }
}

fn machine_config_response_value(config: &MachineConfigResponse) -> serde_json::Value {
    serde_json::json!({
        "huge_pages": config.huge_pages.as_str(),
        "mem_size_mib": config.mem_size_mib,
        "smt": config.smt,
        "track_dirty_pages": config.track_dirty_pages,
        "vcpu_count": config.vcpu_count,
    })
}

fn boot_source_response_value(boot_source: &BootSourceResponse) -> serde_json::Value {
    let mut body = serde_json::Map::new();
    body.insert(
        "kernel_image_path".to_string(),
        serde_json::Value::String(boot_source.kernel_image_path.clone()),
    );
    if let Some(initrd_path) = &boot_source.initrd_path {
        body.insert(
            "initrd_path".to_string(),
            serde_json::Value::String(initrd_path.clone()),
        );
    }
    if let Some(boot_args) = &boot_source.boot_args {
        body.insert(
            "boot_args".to_string(),
            serde_json::Value::String(boot_args.clone()),
        );
    }

    serde_json::Value::Object(body)
}

fn drive_config_response_value(drive: &DriveConfigResponse) -> serde_json::Value {
    let mut body = serde_json::Map::new();
    body.insert(
        "cache_type".to_string(),
        serde_json::Value::String(drive.cache_type.clone()),
    );
    body.insert(
        "drive_id".to_string(),
        serde_json::Value::String(drive.drive_id.clone()),
    );
    body.insert(
        "io_engine".to_string(),
        serde_json::Value::String(drive.io_engine.clone()),
    );
    body.insert(
        "is_read_only".to_string(),
        serde_json::Value::Bool(drive.is_read_only),
    );
    body.insert(
        "is_root_device".to_string(),
        serde_json::Value::Bool(drive.is_root_device),
    );
    if let Some(partuuid) = &drive.partuuid {
        body.insert(
            "partuuid".to_string(),
            serde_json::Value::String(partuuid.clone()),
        );
    }
    body.insert(
        "path_on_host".to_string(),
        serde_json::Value::String(drive.path_on_host.clone()),
    );

    serde_json::Value::Object(body)
}

fn network_interface_config_response_value(
    network_interface: &NetworkInterfaceConfigResponse,
) -> serde_json::Value {
    let mut body = serde_json::Map::new();
    if let Some(guest_mac) = &network_interface.guest_mac {
        body.insert(
            "guest_mac".to_string(),
            serde_json::Value::String(guest_mac.clone()),
        );
    }
    body.insert(
        "host_dev_name".to_string(),
        serde_json::Value::String(network_interface.host_dev_name.clone()),
    );
    body.insert(
        "iface_id".to_string(),
        serde_json::Value::String(network_interface.iface_id.clone()),
    );

    serde_json::Value::Object(body)
}

pub fn parse_request(bytes: &[u8]) -> Result<ApiRequest, RequestError> {
    if bytes.len() > HTTP_MAX_PAYLOAD_SIZE {
        return Err(RequestError::PayloadTooLarge);
    }

    let (method, path, header_len, request_body) = parse_request_head(bytes)?;
    let body = bytes
        .get(header_len..)
        .ok_or(RequestError::MalformedRequest)?;

    if request_body.has_unsupported_encoding() {
        return Err(RequestError::MalformedRequest);
    }

    checked_request_len(header_len, request_body.content_length())?;

    if body.len() != request_body.content_length() {
        return Err(RequestError::MalformedRequest);
    }

    if method == "GET" && request_body.has_content() {
        return Err(RequestError::GetRequestBody);
    }

    if method == "PUT"
        && let Some(path_drive_id) = drive_path_id(path)
    {
        return parse_drive_config_request(path_drive_id, body);
    }
    if method == "PUT"
        && let Some(path_iface_id) = network_interface_path_id(path)
    {
        return parse_network_interface_config_request(path_iface_id, body);
    }
    if method == "PUT" && path == "/actions" {
        return parse_action_request(body);
    }
    if method == "PUT" && path == "/boot-source" {
        return parse_boot_source_request(body);
    }
    if method == "PUT" && path == "/logger" {
        return parse_logger_config_request(body);
    }
    if method == "PUT" && path == "/machine-config" {
        return parse_machine_config_request(body);
    }
    if method == "PUT" && path == "/metrics" {
        return parse_metrics_config_request(body);
    }

    match (method, path) {
        ("GET", "/") => Ok(ApiRequest::GetInstanceInfo),
        ("GET", "/machine-config") => Ok(ApiRequest::GetMachineConfig),
        ("GET", "/vm/config") => Ok(ApiRequest::GetVmConfig),
        ("GET", "/version") => Ok(ApiRequest::GetVersion),
        _ => Err(RequestError::InvalidPathMethod),
    }
}

fn drive_path_id(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/drives/")?;
    if rest.is_empty()
        || rest.contains('/')
        || !rest
            .chars()
            .all(|character| character == '_' || character.is_alphanumeric())
    {
        return None;
    }

    Some(rest)
}

fn network_interface_path_id(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/network-interfaces/")?;
    if rest.is_empty()
        || rest.contains('/')
        || !rest
            .chars()
            .all(|character| character == '_' || character.is_alphanumeric())
    {
        return None;
    }

    Some(rest)
}

fn parse_action_request(body: &[u8]) -> Result<ApiRequest, RequestError> {
    let body = serde_json::from_slice::<ActionRequestBody>(body)
        .map_err(|_| RequestError::MalformedRequest)?;

    let action_type = match body.action_type {
        ActionTypeBody::FlushMetrics => ActionType::FlushMetrics,
        ActionTypeBody::InstanceStart => ActionType::InstanceStart,
        ActionTypeBody::SendCtrlAltDel => return Err(RequestError::SendCtrlAltDelUnsupported),
    };

    Ok(ApiRequest::PutAction(Box::new(ActionRequest {
        action_type,
    })))
}

fn parse_boot_source_request(body: &[u8]) -> Result<ApiRequest, RequestError> {
    let body = serde_json::from_slice::<BootSourceRequestBody>(body)
        .map_err(|_| RequestError::MalformedRequest)?;

    Ok(ApiRequest::PutBootSource(Box::new(BootSourceRequest {
        kernel_image_path: body.kernel_image_path,
        initrd_path: body.initrd_path,
        boot_args: body.boot_args,
    })))
}

fn parse_logger_config_request(body: &[u8]) -> Result<ApiRequest, RequestError> {
    let body = serde_json::from_slice::<LoggerConfigRequestBody>(body)
        .map_err(|_| RequestError::MalformedRequest)?;

    Ok(ApiRequest::PutLogger(Box::new(LoggerConfigRequest {
        log_path: body.log_path,
        level: body.level,
        show_level: body.show_level,
        show_log_origin: body.show_log_origin,
        module: body.module,
    })))
}

fn parse_drive_config_request(
    path_drive_id: &str,
    body: &[u8],
) -> Result<ApiRequest, RequestError> {
    let body = serde_json::from_slice::<DriveConfigRequestBody>(body)
        .map_err(|_| RequestError::MalformedRequest)?;
    if path_drive_id != body.drive_id {
        return Err(RequestError::MismatchedDriveId);
    }
    let rate_limiter_configured = match &body.rate_limiter {
        Some(rate_limiter) => {
            validate_rate_limiter_config(rate_limiter)?;
            true
        }
        None => false,
    };

    Ok(ApiRequest::PutDrive(Box::new(DriveConfigRequest {
        path_drive_id: path_drive_id.to_string(),
        body_drive_id: body.drive_id,
        path_on_host: body.path_on_host,
        is_root_device: body.is_root_device,
        is_read_only: body.is_read_only,
        partuuid: body.partuuid,
        cache_type: body.cache_type,
        io_engine: body.io_engine,
        rate_limiter_configured,
        socket: body.socket,
    })))
}

fn parse_network_interface_config_request(
    path_iface_id: &str,
    body: &[u8],
) -> Result<ApiRequest, RequestError> {
    let body = serde_json::from_slice::<NetworkInterfaceConfigRequestBody>(body)
        .map_err(|_| RequestError::MalformedRequest)?;
    if path_iface_id != body.iface_id {
        return Err(RequestError::MismatchedInterfaceId);
    }
    let rx_rate_limiter_configured = match &body.rx_rate_limiter {
        Some(rate_limiter) => {
            validate_rate_limiter_config(rate_limiter)?;
            true
        }
        None => false,
    };
    let tx_rate_limiter_configured = match &body.tx_rate_limiter {
        Some(rate_limiter) => {
            validate_rate_limiter_config(rate_limiter)?;
            true
        }
        None => false,
    };

    Ok(ApiRequest::PutNetworkInterface(Box::new(
        NetworkInterfaceConfigRequest {
            path_iface_id: path_iface_id.to_string(),
            body_iface_id: body.iface_id,
            host_dev_name: body.host_dev_name,
            guest_mac: body.guest_mac,
            mtu_configured: body.mtu.is_some(),
            rx_rate_limiter_configured,
            tx_rate_limiter_configured,
        },
    )))
}

fn parse_machine_config_request(body: &[u8]) -> Result<ApiRequest, RequestError> {
    let body = serde_json::from_slice::<MachineConfigRequestBody>(body)
        .map_err(|_| RequestError::MalformedRequest)?;

    validate_machine_config_request(&body)?;

    Ok(ApiRequest::PutMachineConfig(Box::new(
        MachineConfigRequest {
            vcpu_count: body.vcpu_count,
            mem_size_mib: body.mem_size_mib,
            smt: body.smt,
            cpu_template: body.cpu_template,
            track_dirty_pages: body.track_dirty_pages,
            huge_pages: body.huge_pages,
        },
    )))
}

fn parse_metrics_config_request(body: &[u8]) -> Result<ApiRequest, RequestError> {
    let body = serde_json::from_slice::<MetricsConfigRequestBody>(body)
        .map_err(|_| RequestError::MalformedRequest)?;

    Ok(ApiRequest::PutMetrics(Box::new(MetricsConfigRequest {
        metrics_path: body.metrics_path,
    })))
}

fn validate_machine_config_request(body: &MachineConfigRequestBody) -> Result<(), RequestError> {
    if body.vcpu_count == 0 || body.vcpu_count > MAX_MACHINE_CONFIG_VCPUS {
        return Err(RequestError::MalformedRequest);
    }
    if body.mem_size_mib == 0 {
        return Err(RequestError::MalformedRequest);
    }
    if body.smt || body.track_dirty_pages || body.huge_pages != MachineConfigHugePages::None {
        return Err(RequestError::MalformedRequest);
    }

    Ok(())
}

fn validate_rate_limiter_config(value: &serde_json::Value) -> Result<(), RequestError> {
    let rate_limiter = value.as_object().ok_or(RequestError::MalformedRequest)?;
    for key in rate_limiter.keys() {
        if key != RATE_LIMITER_BANDWIDTH_FIELD && key != RATE_LIMITER_OPS_FIELD {
            return Err(RequestError::MalformedRequest);
        }
    }

    if let Some(bucket) = rate_limiter.get(RATE_LIMITER_BANDWIDTH_FIELD) {
        validate_token_bucket(bucket)?;
    }
    if let Some(bucket) = rate_limiter.get(RATE_LIMITER_OPS_FIELD) {
        validate_token_bucket(bucket)?;
    }

    Ok(())
}

fn validate_token_bucket(value: &serde_json::Value) -> Result<(), RequestError> {
    if value.is_null() {
        return Ok(());
    }

    let bucket = value.as_object().ok_or(RequestError::MalformedRequest)?;
    for key in bucket.keys() {
        if key != TOKEN_BUCKET_SIZE_FIELD
            && key != TOKEN_BUCKET_ONE_TIME_BURST_FIELD
            && key != TOKEN_BUCKET_REFILL_TIME_FIELD
        {
            return Err(RequestError::MalformedRequest);
        }
    }

    require_u64_field(bucket, TOKEN_BUCKET_SIZE_FIELD)?;
    require_u64_field(bucket, TOKEN_BUCKET_REFILL_TIME_FIELD)?;
    if let Some(value) = bucket.get(TOKEN_BUCKET_ONE_TIME_BURST_FIELD)
        && !value.is_null()
        && value.as_u64().is_none()
    {
        return Err(RequestError::MalformedRequest);
    }

    Ok(())
}

fn require_u64_field(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<(), RequestError> {
    object
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .map(|_| ())
        .ok_or(RequestError::MalformedRequest)
}

pub fn request_total_len(bytes: &[u8]) -> Result<Option<usize>, RequestError> {
    if bytes.len() > HTTP_MAX_PAYLOAD_SIZE {
        return Err(RequestError::PayloadTooLarge);
    }

    let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut request = httparse::Request::new(&mut headers);
    let status = request
        .parse(bytes)
        .map_err(|_| RequestError::MalformedRequest)?;
    let header_len = match status {
        httparse::Status::Complete(header_len) => header_len,
        httparse::Status::Partial => return Ok(None),
    };
    let body = request_body(request.headers)?;

    if body.has_unsupported_encoding() {
        return Err(RequestError::MalformedRequest);
    }

    Ok(Some(checked_request_len(
        header_len,
        body.content_length(),
    )?))
}

fn parse_request_head(bytes: &[u8]) -> Result<(&str, &str, usize, RequestBody), RequestError> {
    let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut request = httparse::Request::new(&mut headers);

    let status = request
        .parse(bytes)
        .map_err(|_| RequestError::MalformedRequest)?;
    let header_len = match status {
        httparse::Status::Complete(header_len) => header_len,
        httparse::Status::Partial => return Err(RequestError::MalformedRequest),
    };

    let method = request.method.ok_or(RequestError::MalformedRequest)?;
    let path = request.path.ok_or(RequestError::MalformedRequest)?;
    let body = request_body(request.headers)?;

    Ok((method, path, header_len, body))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RequestBody {
    content_length: usize,
    transfer_encoding: bool,
}

impl RequestBody {
    const fn content_length(self) -> usize {
        self.content_length
    }

    const fn has_unsupported_encoding(self) -> bool {
        self.transfer_encoding
    }

    const fn has_content(self) -> bool {
        self.content_length > 0
    }
}

fn request_body(headers: &[httparse::Header<'_>]) -> Result<RequestBody, RequestError> {
    let mut content_length = None;
    let mut transfer_encoding = false;

    for header in headers {
        if header.name.eq_ignore_ascii_case("Content-Length") {
            if content_length.is_some() {
                return Err(RequestError::MalformedRequest);
            }

            content_length = Some(parse_content_length(header.value)?);
        } else if header.name.eq_ignore_ascii_case("Transfer-Encoding") {
            transfer_encoding = true;
        }
    }

    Ok(RequestBody {
        content_length: content_length.unwrap_or(0),
        transfer_encoding,
    })
}

fn parse_content_length(value: &[u8]) -> Result<usize, RequestError> {
    let value = trim_http_optional_whitespace(value);
    if value.is_empty() {
        return Err(RequestError::MalformedRequest);
    }

    let mut parsed = 0usize;
    for byte in value {
        if !byte.is_ascii_digit() {
            return Err(RequestError::MalformedRequest);
        }

        parsed = parsed
            .checked_mul(10)
            .and_then(|parsed| parsed.checked_add(usize::from(byte - b'0')))
            .ok_or(RequestError::PayloadTooLarge)?;
    }

    Ok(parsed)
}

fn trim_http_optional_whitespace(value: &[u8]) -> &[u8] {
    let mut value = value;

    while let Some((&byte, rest)) = value.split_first() {
        if !is_http_optional_whitespace(byte) {
            break;
        }
        value = rest;
    }

    while let Some((&byte, rest)) = value.split_last() {
        if !is_http_optional_whitespace(byte) {
            break;
        }
        value = rest;
    }

    value
}

const fn is_http_optional_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t')
}

fn checked_request_len(header_len: usize, content_length: usize) -> Result<usize, RequestError> {
    let total_len = header_len
        .checked_add(content_length)
        .ok_or(RequestError::PayloadTooLarge)?;

    if total_len > HTTP_MAX_PAYLOAD_SIZE {
        return Err(RequestError::PayloadTooLarge);
    }

    Ok(total_len)
}

impl From<ApiRequest> for Endpoint {
    fn from(request: ApiRequest) -> Self {
        match request {
            ApiRequest::GetInstanceInfo => Self::DescribeInstance,
            ApiRequest::GetMachineConfig => Self::MachineConfig,
            ApiRequest::GetVmConfig => Self::VmConfig,
            ApiRequest::GetVersion => Self::Version,
            ApiRequest::PutAction(_) => Self::Actions,
            ApiRequest::PutBootSource(_) => Self::BootSource,
            ApiRequest::PutDrive(_) => Self::Drive,
            ApiRequest::PutLogger(_) => Self::Logger,
            ApiRequest::PutMachineConfig(_) => Self::MachineConfig,
            ApiRequest::PutMetrics(_) => Self::Metrics,
            ApiRequest::PutNetworkInterface(_) => Self::NetworkInterface,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VERSION: &str = "0.1.0";

    fn request_with_body(method: &str, path: &str, body: &str) -> Vec<u8> {
        format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    #[test]
    fn parses_get_instance_info() {
        let request = b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n";

        assert_eq!(parse_request(request), Ok(ApiRequest::GetInstanceInfo));
        assert_eq!(request_total_len(request), Ok(Some(request.len())));
    }

    #[test]
    fn rejects_get_instance_info_with_body() {
        let request =
            b"GET / HTTP/1.1\r\nContent-Length: 2\r\nContent-Type: application/json\r\n\r\n{}";

        assert_eq!(parse_request(request), Err(RequestError::GetRequestBody));
    }

    #[test]
    fn parses_get_instance_info_with_zero_content_length() {
        let request = b"GET / HTTP/1.1\r\nContent-Length:\t0 \r\n\r\n";

        assert_eq!(parse_request(request), Ok(ApiRequest::GetInstanceInfo));
        assert_eq!(request_total_len(request), Ok(Some(request.len())));
    }

    #[test]
    fn parses_get_version() {
        let request = b"GET /version HTTP/1.1\r\nHost: localhost\r\n\r\n";

        assert_eq!(parse_request(request), Ok(ApiRequest::GetVersion));
        assert_eq!(request_total_len(request), Ok(Some(request.len())));
    }

    #[test]
    fn rejects_get_version_with_body() {
        let request =
            b"GET /version HTTP/1.1\r\nContent-Length: 2\r\nContent-Type: application/json\r\n\r\n{}";

        assert_eq!(parse_request(request), Err(RequestError::GetRequestBody));
    }

    #[test]
    fn parses_get_version_with_zero_content_length() {
        let request = b"GET /version HTTP/1.1\r\nContent-Length:\t0 \r\n\r\n";

        assert_eq!(parse_request(request), Ok(ApiRequest::GetVersion));
        assert_eq!(request_total_len(request), Ok(Some(request.len())));
    }

    #[test]
    fn parses_get_machine_config() {
        let request = b"GET /machine-config HTTP/1.1\r\nHost: localhost\r\n\r\n";

        assert_eq!(parse_request(request), Ok(ApiRequest::GetMachineConfig));
        assert_eq!(request_total_len(request), Ok(Some(request.len())));
    }

    #[test]
    fn rejects_get_machine_config_with_body() {
        let request = request_with_body("GET", "/machine-config", "{}");

        assert_eq!(parse_request(&request), Err(RequestError::GetRequestBody));
    }

    #[test]
    fn parses_get_vm_config() {
        let request = b"GET /vm/config HTTP/1.1\r\nHost: localhost\r\n\r\n";

        assert_eq!(parse_request(request), Ok(ApiRequest::GetVmConfig));
        assert_eq!(request_total_len(request), Ok(Some(request.len())));
    }

    #[test]
    fn parses_get_vm_config_with_zero_content_length() {
        let request = b"GET /vm/config HTTP/1.1\r\nContent-Length:\t0 \r\n\r\n";

        assert_eq!(parse_request(request), Ok(ApiRequest::GetVmConfig));
        assert_eq!(request_total_len(request), Ok(Some(request.len())));
    }

    #[test]
    fn rejects_get_vm_config_with_body() {
        let request = request_with_body("GET", "/vm/config", "{}");

        assert_eq!(parse_request(&request), Err(RequestError::GetRequestBody));
    }

    #[test]
    fn parses_put_actions_instance_start() {
        let body = r#"{"action_type":"InstanceStart"}"#;
        let request = request_with_body("PUT", "/actions", body);

        let parsed = parse_request(&request).expect("actions request should parse");

        let ApiRequest::PutAction(action) = parsed else {
            panic!("expected actions request");
        };
        assert_eq!(action.action_type(), ActionType::InstanceStart);
        assert_eq!(request_total_len(&request), Ok(Some(request.len())));
    }

    #[test]
    fn parses_put_actions_flush_metrics() {
        let body = r#"{"action_type":"FlushMetrics"}"#;
        let request = request_with_body("PUT", "/actions", body);

        let parsed = parse_request(&request).expect("actions request should parse");

        let ApiRequest::PutAction(action) = parsed else {
            panic!("expected actions request");
        };
        assert_eq!(action.action_type(), ActionType::FlushMetrics);
    }

    #[test]
    fn rejects_put_actions_send_ctrl_alt_del() {
        let body = r#"{"action_type":"SendCtrlAltDel"}"#;
        let request = request_with_body("PUT", "/actions", body);

        let err = parse_request(&request).expect_err("SendCtrlAltDel should be unsupported");

        assert_eq!(err, RequestError::SendCtrlAltDelUnsupported);
        assert_eq!(
            err.fault_message(),
            "SendCtrlAltDel does not supported on aarch64."
        );
    }

    #[test]
    fn rejects_put_actions_missing_action_type() {
        let request = request_with_body("PUT", "/actions", "{}");

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_put_actions_unknown_field() {
        let body = r#"{"action_type":"InstanceStart","unknown":true}"#;
        let request = request_with_body("PUT", "/actions", body);

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_put_actions_invalid_field_types() {
        for body in [
            r#"{"action_type":1}"#,
            r#"{"action_type":null}"#,
            r#"{"action_type":["InstanceStart"]}"#,
        ] {
            assert_eq!(
                parse_request(&request_with_body("PUT", "/actions", body)),
                Err(RequestError::MalformedRequest)
            );
        }
    }

    #[test]
    fn rejects_put_actions_unknown_action_type() {
        let body = r#"{"action_type":"Pause"}"#;
        let request = request_with_body("PUT", "/actions", body);

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_put_actions_empty_body() {
        let request = request_with_body("PUT", "/actions", "");

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_unsupported_actions_method_or_path() {
        assert_eq!(
            parse_request(b"GET /actions HTTP/1.1\r\nHost: localhost\r\n\r\n"),
            Err(RequestError::InvalidPathMethod)
        );
        assert_eq!(
            parse_request(&request_with_body("GET", "/actions", "{}")),
            Err(RequestError::GetRequestBody)
        );
        assert_eq!(
            parse_request(&request_with_body(
                "PUT",
                "/actions/extra",
                r#"{"action_type":"InstanceStart"}"#,
            )),
            Err(RequestError::InvalidPathMethod)
        );
    }

    #[test]
    fn parses_put_boot_source_with_minimal_body() {
        let body = r#"{"kernel_image_path":"/tmp/vmlinux"}"#;
        let request = request_with_body("PUT", "/boot-source", body);

        let parsed = parse_request(&request).expect("boot-source request should parse");

        let ApiRequest::PutBootSource(config) = parsed else {
            panic!("expected boot-source request");
        };
        assert_eq!(config.kernel_image_path(), "/tmp/vmlinux");
        assert_eq!(config.initrd_path(), None);
        assert_eq!(config.boot_args(), None);
        assert_eq!(request_total_len(&request), Ok(Some(request.len())));
    }

    #[test]
    fn parses_put_boot_source_with_complete_body() {
        let body = r#"{
            "kernel_image_path": "/tmp/vmlinux",
            "initrd_path": "/tmp/initrd.img",
            "boot_args": "console=ttyS0 reboot=k panic=1"
        }"#;
        let request = request_with_body("PUT", "/boot-source", body);

        let parsed = parse_request(&request).expect("complete boot-source request should parse");

        let ApiRequest::PutBootSource(config) = parsed else {
            panic!("expected boot-source request");
        };
        assert_eq!(config.kernel_image_path(), "/tmp/vmlinux");
        assert_eq!(config.initrd_path(), Some("/tmp/initrd.img"));
        assert_eq!(config.boot_args(), Some("console=ttyS0 reboot=k panic=1"));
    }

    #[test]
    fn parses_put_boot_source_with_null_optional_fields() {
        let body = r#"{
            "kernel_image_path": "/tmp/vmlinux",
            "initrd_path": null,
            "boot_args": null
        }"#;
        let request = request_with_body("PUT", "/boot-source", body);

        let parsed = parse_request(&request).expect("nullable boot-source fields should parse");

        let ApiRequest::PutBootSource(config) = parsed else {
            panic!("expected boot-source request");
        };
        assert_eq!(config.kernel_image_path(), "/tmp/vmlinux");
        assert_eq!(config.initrd_path(), None);
        assert_eq!(config.boot_args(), None);
    }

    #[test]
    fn rejects_put_boot_source_missing_kernel_image_path() {
        let request = request_with_body("PUT", "/boot-source", r#"{"boot_args":"console=ttyS0"}"#);

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_put_boot_source_unknown_field() {
        let request = request_with_body(
            "PUT",
            "/boot-source",
            r#"{"kernel_image_path":"/tmp/vmlinux","unknown":true}"#,
        );

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_put_boot_source_invalid_field_types() {
        for body in [
            r#"{"kernel_image_path":1}"#,
            r#"{"kernel_image_path":"/tmp/vmlinux","initrd_path":false}"#,
            r#"{"kernel_image_path":"/tmp/vmlinux","boot_args":["console=ttyS0"]}"#,
        ] {
            assert_eq!(
                parse_request(&request_with_body("PUT", "/boot-source", body)),
                Err(RequestError::MalformedRequest)
            );
        }
    }

    #[test]
    fn rejects_put_boot_source_empty_body() {
        let request = request_with_body("PUT", "/boot-source", "");

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_unsupported_boot_source_method_or_path() {
        assert_eq!(
            parse_request(b"GET /boot-source HTTP/1.1\r\nHost: localhost\r\n\r\n"),
            Err(RequestError::InvalidPathMethod)
        );
        assert_eq!(
            parse_request(&request_with_body(
                "PUT",
                "/boot-source/extra",
                r#"{"kernel_image_path":"/tmp/vmlinux"}"#,
            )),
            Err(RequestError::InvalidPathMethod)
        );
    }

    #[test]
    fn parses_put_logger_with_minimal_body() {
        let request = request_with_body("PUT", "/logger", "{}");

        let parsed = parse_request(&request).expect("logger request should parse");

        let ApiRequest::PutLogger(config) = parsed else {
            panic!("expected logger request");
        };
        assert_eq!(config.log_path(), None);
        assert_eq!(config.level(), None);
        assert_eq!(config.show_level(), None);
        assert_eq!(config.show_log_origin(), None);
        assert_eq!(config.module(), None);
        assert_eq!(request_total_len(&request), Ok(Some(request.len())));
    }

    #[test]
    fn parses_put_logger_with_complete_body() {
        let body = r#"{
            "log_path": "/tmp/logger",
            "level": "Warning",
            "show_level": true,
            "show_log_origin": true,
            "module": "api_server::request"
        }"#;
        let request = request_with_body("PUT", "/logger", body);

        let parsed = parse_request(&request).expect("logger request should parse");

        let ApiRequest::PutLogger(config) = parsed else {
            panic!("expected logger request");
        };
        assert_eq!(config.log_path(), Some("/tmp/logger"));
        assert_eq!(config.level(), Some(LoggerLevel::Warn));
        assert_eq!(config.show_level(), Some(true));
        assert_eq!(config.show_log_origin(), Some(true));
        assert_eq!(config.module(), Some("api_server::request"));
    }

    #[test]
    fn parses_put_logger_case_insensitive_levels_and_nulls() {
        for (level, expected) in [
            ("off", LoggerLevel::Off),
            ("TRACE", LoggerLevel::Trace),
            ("Debug", LoggerLevel::Debug),
            ("info", LoggerLevel::Info),
            ("warn", LoggerLevel::Warn),
            ("ERROR", LoggerLevel::Error),
        ] {
            let body = format!(r#"{{"level":"{level}"}}"#);
            let parsed = parse_request(&request_with_body("PUT", "/logger", &body))
                .expect("logger request should parse");
            let ApiRequest::PutLogger(config) = parsed else {
                panic!("expected logger request");
            };
            assert_eq!(config.level(), Some(expected));
        }

        let body = r#"{
            "log_path": null,
            "level": null,
            "show_level": null,
            "show_log_origin": null,
            "module": null
        }"#;
        let parsed = parse_request(&request_with_body("PUT", "/logger", body))
            .expect("logger request should parse");
        let ApiRequest::PutLogger(config) = parsed else {
            panic!("expected logger request");
        };
        assert_eq!(config.log_path(), None);
        assert_eq!(config.level(), None);
        assert_eq!(config.show_level(), None);
        assert_eq!(config.show_log_origin(), None);
        assert_eq!(config.module(), None);
    }

    #[test]
    fn rejects_put_logger_unknown_field() {
        let request = request_with_body(
            "PUT",
            "/logger",
            r#"{"log_path":"/tmp/log","unknown":true}"#,
        );

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_put_logger_invalid_field_types() {
        for body in [
            r#"{"log_path":1}"#,
            r#"{"level":1}"#,
            r#"{"level":"Verbose"}"#,
            r#"{"show_level":"true"}"#,
            r#"{"show_log_origin":"true"}"#,
            r#"{"module":false}"#,
        ] {
            assert_eq!(
                parse_request(&request_with_body("PUT", "/logger", body)),
                Err(RequestError::MalformedRequest)
            );
        }
    }

    #[test]
    fn rejects_put_logger_empty_body() {
        let request = request_with_body("PUT", "/logger", "");

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_unsupported_logger_method_or_path() {
        assert_eq!(
            parse_request(b"GET /logger HTTP/1.1\r\nHost: localhost\r\n\r\n"),
            Err(RequestError::InvalidPathMethod)
        );
        assert_eq!(
            parse_request(&request_with_body("PUT", "/logger/extra", "{}")),
            Err(RequestError::InvalidPathMethod)
        );
    }

    #[test]
    fn parses_put_machine_config_with_minimal_body() {
        let body = r#"{
            "vcpu_count": 1,
            "mem_size_mib": 128
        }"#;
        let request = request_with_body("PUT", "/machine-config", body);

        let parsed = parse_request(&request).expect("machine-config request should parse");

        let ApiRequest::PutMachineConfig(config) = parsed else {
            panic!("expected machine-config request");
        };
        assert_eq!(config.vcpu_count(), 1);
        assert_eq!(config.mem_size_mib(), 128);
        assert!(!config.smt());
        assert_eq!(config.cpu_template(), None);
        assert!(!config.track_dirty_pages());
        assert_eq!(config.huge_pages(), MachineConfigHugePages::None);
        assert_eq!(request_total_len(&request), Ok(Some(request.len())));
    }

    #[test]
    fn parses_put_machine_config_with_accepted_default_values() {
        let body = r#"{
            "vcpu_count": 32,
            "mem_size_mib": 1024,
            "smt": false,
            "cpu_template": "None",
            "track_dirty_pages": false,
            "huge_pages": "None"
        }"#;
        let request = request_with_body("PUT", "/machine-config", body);

        let parsed = parse_request(&request).expect("machine-config defaults should parse");

        let ApiRequest::PutMachineConfig(config) = parsed else {
            panic!("expected machine-config request");
        };
        assert_eq!(config.vcpu_count(), 32);
        assert_eq!(config.mem_size_mib(), 1024);
        assert_eq!(config.cpu_template(), Some(MachineConfigCpuTemplate::None));
        assert_eq!(config.huge_pages(), MachineConfigHugePages::None);
    }

    #[test]
    fn parses_put_machine_config_with_null_cpu_template() {
        let body = r#"{
            "vcpu_count": 2,
            "mem_size_mib": 256,
            "cpu_template": null
        }"#;
        let request = request_with_body("PUT", "/machine-config", body);

        let parsed = parse_request(&request).expect("null CPU template should parse");

        let ApiRequest::PutMachineConfig(config) = parsed else {
            panic!("expected machine-config request");
        };
        assert_eq!(config.cpu_template(), None);
    }

    #[test]
    fn rejects_put_machine_config_missing_required_fields() {
        assert_eq!(
            parse_request(&request_with_body(
                "PUT",
                "/machine-config",
                r#"{"mem_size_mib":128}"#,
            )),
            Err(RequestError::MalformedRequest)
        );
        assert_eq!(
            parse_request(&request_with_body(
                "PUT",
                "/machine-config",
                r#"{"vcpu_count":1}"#,
            )),
            Err(RequestError::MalformedRequest)
        );
    }

    #[test]
    fn rejects_put_machine_config_unknown_field() {
        let body = r#"{
            "vcpu_count": 1,
            "mem_size_mib": 128,
            "unknown": true
        }"#;
        let request = request_with_body("PUT", "/machine-config", body);

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_put_machine_config_invalid_field_type() {
        let body = r#"{
            "vcpu_count": "1",
            "mem_size_mib": 128
        }"#;
        let request = request_with_body("PUT", "/machine-config", body);

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_put_machine_config_invalid_numeric_bounds() {
        for body in [
            r#"{"vcpu_count":0,"mem_size_mib":128}"#,
            r#"{"vcpu_count":33,"mem_size_mib":128}"#,
            r#"{"vcpu_count":1,"mem_size_mib":0}"#,
        ] {
            assert_eq!(
                parse_request(&request_with_body("PUT", "/machine-config", body)),
                Err(RequestError::MalformedRequest)
            );
        }
    }

    #[test]
    fn rejects_put_machine_config_unsupported_values() {
        for body in [
            r#"{"vcpu_count":1,"mem_size_mib":128,"smt":true}"#,
            r#"{"vcpu_count":1,"mem_size_mib":128,"track_dirty_pages":true}"#,
            r#"{"vcpu_count":1,"mem_size_mib":128,"cpu_template":"V1N1"}"#,
            r#"{"vcpu_count":1,"mem_size_mib":128,"huge_pages":"2M"}"#,
        ] {
            assert_eq!(
                parse_request(&request_with_body("PUT", "/machine-config", body)),
                Err(RequestError::MalformedRequest)
            );
        }
    }

    #[test]
    fn rejects_put_machine_config_null_for_non_nullable_default_fields() {
        for body in [
            r#"{"vcpu_count":1,"mem_size_mib":128,"smt":null}"#,
            r#"{"vcpu_count":1,"mem_size_mib":128,"track_dirty_pages":null}"#,
            r#"{"vcpu_count":1,"mem_size_mib":128,"huge_pages":null}"#,
        ] {
            assert_eq!(
                parse_request(&request_with_body("PUT", "/machine-config", body)),
                Err(RequestError::MalformedRequest)
            );
        }
    }

    #[test]
    fn parses_put_metrics() {
        let body = r#"{"metrics_path":"/tmp/metrics"}"#;
        let request = request_with_body("PUT", "/metrics", body);

        let parsed = parse_request(&request).expect("metrics request should parse");

        let ApiRequest::PutMetrics(config) = parsed else {
            panic!("expected metrics request");
        };
        assert_eq!(config.metrics_path(), "/tmp/metrics");
        assert_eq!(request_total_len(&request), Ok(Some(request.len())));
    }

    #[test]
    fn rejects_put_metrics_missing_metrics_path() {
        let request = request_with_body("PUT", "/metrics", "{}");

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_put_metrics_unknown_field() {
        let request = request_with_body(
            "PUT",
            "/metrics",
            r#"{"metrics_path":"/tmp/metrics","unknown":true}"#,
        );

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_put_metrics_invalid_field_type() {
        for body in [
            r#"{"metrics_path":1}"#,
            r#"{"metrics_path":null}"#,
            r#"{"metrics_path":["/tmp/metrics"]}"#,
        ] {
            assert_eq!(
                parse_request(&request_with_body("PUT", "/metrics", body)),
                Err(RequestError::MalformedRequest)
            );
        }
    }

    #[test]
    fn rejects_put_metrics_empty_body() {
        let request = request_with_body("PUT", "/metrics", "");

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_unsupported_metrics_method_or_path() {
        assert_eq!(
            parse_request(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n"),
            Err(RequestError::InvalidPathMethod)
        );
        assert_eq!(
            parse_request(&request_with_body("PUT", "/metrics/extra", "{}")),
            Err(RequestError::InvalidPathMethod)
        );
    }

    #[test]
    fn parses_put_drive_with_minimal_body() {
        let body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/rootfs.ext4",
            "is_root_device": true
        }"#;
        let request = request_with_body("PUT", "/drives/rootfs", body);

        let parsed = parse_request(&request).expect("drive request should parse");

        let ApiRequest::PutDrive(config) = parsed else {
            panic!("expected drive request");
        };
        assert_eq!(config.path_drive_id(), "rootfs");
        assert_eq!(config.body_drive_id(), "rootfs");
        assert_eq!(config.path_on_host(), "/tmp/rootfs.ext4");
        assert!(config.is_root_device());
        assert_eq!(config.is_read_only(), None);
        assert_eq!(config.partuuid(), None);
        assert_eq!(config.cache_type(), None);
        assert_eq!(config.io_engine(), None);
        assert!(!config.rate_limiter_configured());
        assert_eq!(config.socket(), None);
        assert_eq!(request_total_len(&request), Ok(Some(request.len())));
    }

    #[test]
    fn parses_put_drive_with_complete_body() {
        let body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/rootfs.ext4",
            "is_root_device": true,
            "is_read_only": true,
            "partuuid": "0eaa91a0-01",
            "cache_type": "Unsafe",
            "io_engine": "Sync",
            "rate_limiter": {
                "bandwidth": {
                    "size": 0,
                    "one_time_burst": 0,
                    "refill_time": 0
                }
            },
            "socket": "/tmp/vhost.sock"
        }"#;
        let request = request_with_body("PUT", "/drives/rootfs", body);

        let parsed = parse_request(&request).expect("drive request should parse");

        let ApiRequest::PutDrive(config) = parsed else {
            panic!("expected drive request");
        };
        assert_eq!(config.path_drive_id(), "rootfs");
        assert_eq!(config.body_drive_id(), "rootfs");
        assert_eq!(config.path_on_host(), "/tmp/rootfs.ext4");
        assert!(config.is_root_device());
        assert_eq!(config.is_read_only(), Some(true));
        assert_eq!(config.partuuid(), Some("0eaa91a0-01"));
        assert_eq!(config.cache_type(), Some(DriveCacheType::Unsafe));
        assert_eq!(config.io_engine(), Some(DriveIoEngine::Sync));
        assert!(config.rate_limiter_configured());
        assert_eq!(config.socket(), Some("/tmp/vhost.sock"));
    }

    #[test]
    fn parses_put_drive_with_deferred_field_nulls() {
        let body = r#"{
            "drive_id": "data",
            "path_on_host": "/tmp/data.ext4",
            "is_root_device": false,
            "rate_limiter": null,
            "socket": null
        }"#;
        let request = request_with_body("PUT", "/drives/data", body);

        let parsed = parse_request(&request).expect("drive request should parse");

        let ApiRequest::PutDrive(config) = parsed else {
            panic!("expected drive request");
        };
        assert!(!config.rate_limiter_configured());
        assert_eq!(config.socket(), None);
    }

    #[test]
    fn parses_put_drive_with_deferred_cache_and_io_values() {
        let body = r#"{
            "drive_id": "data",
            "path_on_host": "/tmp/data.ext4",
            "is_root_device": false,
            "cache_type": "Writeback",
            "io_engine": "Async"
        }"#;
        let request = request_with_body("PUT", "/drives/data", body);

        let parsed = parse_request(&request).expect("drive request should parse");

        let ApiRequest::PutDrive(config) = parsed else {
            panic!("expected drive request");
        };
        assert_eq!(config.cache_type(), Some(DriveCacheType::Writeback));
        assert_eq!(config.io_engine(), Some(DriveIoEngine::Async));
    }

    #[test]
    fn parses_put_drive_with_firecracker_id_character_set() {
        let body = r#"{
            "drive_id": "root_é1",
            "path_on_host": "/tmp/rootfs.ext4",
            "is_root_device": true
        }"#;
        let request = request_with_body("PUT", "/drives/root_é1", body);

        let parsed = parse_request(&request).expect("drive request should parse");

        let ApiRequest::PutDrive(config) = parsed else {
            panic!("expected drive request");
        };
        assert_eq!(config.path_drive_id(), "root_é1");
        assert_eq!(config.body_drive_id(), "root_é1");
    }

    #[test]
    fn rejects_put_drive_mismatched_body_id() {
        let body = r#"{
            "drive_id": "scratch",
            "path_on_host": "/tmp/rootfs.ext4",
            "is_root_device": true
        }"#;
        let request = request_with_body("PUT", "/drives/rootfs", body);

        assert_eq!(
            parse_request(&request),
            Err(RequestError::MismatchedDriveId)
        );
    }

    #[test]
    fn rejects_put_drive_without_path_id() {
        let body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/rootfs.ext4",
            "is_root_device": true
        }"#;

        assert_eq!(
            parse_request(&request_with_body("PUT", "/drives", body)),
            Err(RequestError::InvalidPathMethod)
        );
        assert_eq!(
            parse_request(&request_with_body("PUT", "/drives/", body)),
            Err(RequestError::InvalidPathMethod)
        );
    }

    #[test]
    fn rejects_put_drive_extra_path_segment() {
        let body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/rootfs.ext4",
            "is_root_device": true
        }"#;

        assert_eq!(
            parse_request(&request_with_body("PUT", "/drives/rootfs/extra", body)),
            Err(RequestError::InvalidPathMethod)
        );
    }

    #[test]
    fn rejects_put_drive_invalid_path_id() {
        let body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/rootfs.ext4",
            "is_root_device": true
        }"#;

        assert_eq!(
            parse_request(&request_with_body("PUT", "/drives/root-fs", body)),
            Err(RequestError::InvalidPathMethod)
        );
        assert_eq!(
            parse_request(&request_with_body("PUT", "/drives/rootfs?debug=true", body)),
            Err(RequestError::InvalidPathMethod)
        );
    }

    #[test]
    fn rejects_put_drive_with_empty_body() {
        let request = b"PUT /drives/rootfs HTTP/1.1\r\nContent-Length: 0\r\n\r\n";

        assert_eq!(parse_request(request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_put_drive_with_malformed_json() {
        let request = request_with_body("PUT", "/drives/rootfs", "{");

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_put_drive_missing_required_field() {
        let body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/rootfs.ext4"
        }"#;
        let request = request_with_body("PUT", "/drives/rootfs", body);

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_put_drive_invalid_field_type() {
        let body = r#"{
            "drive_id": 1000,
            "path_on_host": "/tmp/rootfs.ext4",
            "is_root_device": true
        }"#;
        let request = request_with_body("PUT", "/drives/rootfs", body);

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_put_drive_unknown_field() {
        let body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/rootfs.ext4",
            "is_root_device": true,
            "unknown": true
        }"#;
        let request = request_with_body("PUT", "/drives/rootfs", body);

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_put_drive_unknown_cache_value() {
        let body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/rootfs.ext4",
            "is_root_device": true,
            "cache_type": "Unknown"
        }"#;
        let request = request_with_body("PUT", "/drives/rootfs", body);

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_put_drive_invalid_rate_limiter_type() {
        let body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/rootfs.ext4",
            "is_root_device": true,
            "rate_limiter": "unsupported"
        }"#;
        let request = request_with_body("PUT", "/drives/rootfs", body);

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn parses_put_drive_with_null_rate_limiter_buckets() {
        let body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/rootfs.ext4",
            "is_root_device": true,
            "rate_limiter": {
                "bandwidth": null,
                "ops": {
                    "size": 100,
                    "one_time_burst": null,
                    "refill_time": 1000
                }
            }
        }"#;
        let request = request_with_body("PUT", "/drives/rootfs", body);

        let parsed = parse_request(&request).expect("drive request should parse");

        let ApiRequest::PutDrive(config) = parsed else {
            panic!("expected drive request");
        };
        assert!(config.rate_limiter_configured());
    }

    #[test]
    fn rejects_put_drive_invalid_rate_limiter_bucket() {
        let body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/rootfs.ext4",
            "is_root_device": true,
            "rate_limiter": {
                "ops": {
                    "size": 100
                }
            }
        }"#;
        let request = request_with_body("PUT", "/drives/rootfs", body);

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_get_drive_with_body() {
        let request = request_with_body("GET", "/drives/rootfs", "{}");

        assert_eq!(parse_request(&request), Err(RequestError::GetRequestBody));
    }

    #[test]
    fn parses_put_network_interface_with_minimal_body() {
        let body = r#"{
            "iface_id": "eth0",
            "host_dev_name": "tap0"
        }"#;
        let request = request_with_body("PUT", "/network-interfaces/eth0", body);

        let parsed = parse_request(&request).expect("network request should parse");

        let ApiRequest::PutNetworkInterface(config) = parsed else {
            panic!("expected network interface request");
        };
        assert_eq!(config.path_iface_id(), "eth0");
        assert_eq!(config.body_iface_id(), "eth0");
        assert_eq!(config.host_dev_name(), "tap0");
        assert_eq!(config.guest_mac(), None);
        assert!(!config.mtu_configured());
        assert!(!config.rx_rate_limiter_configured());
        assert!(!config.tx_rate_limiter_configured());
    }

    #[test]
    fn parses_put_network_interface_with_complete_body() {
        let body = r#"{
            "iface_id": "eth0",
            "host_dev_name": "tap0",
            "guest_mac": "12:34:56:78:9a:bc",
            "mtu": 1500,
            "rx_rate_limiter": {
                "bandwidth": {
                    "size": 1024,
                    "one_time_burst": 2048,
                    "refill_time": 1000
                }
            },
            "tx_rate_limiter": {
                "ops": {
                    "size": 100,
                    "one_time_burst": null,
                    "refill_time": 1000
                }
            }
        }"#;
        let request = request_with_body("PUT", "/network-interfaces/eth0", body);

        let parsed = parse_request(&request).expect("network request should parse");

        let ApiRequest::PutNetworkInterface(config) = parsed else {
            panic!("expected network interface request");
        };
        assert_eq!(config.path_iface_id(), "eth0");
        assert_eq!(config.body_iface_id(), "eth0");
        assert_eq!(config.host_dev_name(), "tap0");
        assert_eq!(config.guest_mac(), Some("12:34:56:78:9a:bc"));
        assert!(config.mtu_configured());
        assert!(config.rx_rate_limiter_configured());
        assert!(config.tx_rate_limiter_configured());
    }

    #[test]
    fn parses_put_network_interface_with_deferred_field_nulls() {
        let body = r#"{
            "iface_id": "eth0",
            "host_dev_name": "tap0",
            "guest_mac": null,
            "mtu": null,
            "rx_rate_limiter": null,
            "tx_rate_limiter": null
        }"#;
        let request = request_with_body("PUT", "/network-interfaces/eth0", body);

        let parsed = parse_request(&request).expect("network request should parse");

        let ApiRequest::PutNetworkInterface(config) = parsed else {
            panic!("expected network interface request");
        };
        assert_eq!(config.guest_mac(), None);
        assert!(!config.mtu_configured());
        assert!(!config.rx_rate_limiter_configured());
        assert!(!config.tx_rate_limiter_configured());
    }

    #[test]
    fn parses_put_network_interface_with_firecracker_id_character_set() {
        let body = r#"{
            "iface_id": "net_é1",
            "host_dev_name": "tap0"
        }"#;
        let request = request_with_body("PUT", "/network-interfaces/net_é1", body);

        let parsed = parse_request(&request).expect("network request should parse");

        let ApiRequest::PutNetworkInterface(config) = parsed else {
            panic!("expected network interface request");
        };
        assert_eq!(config.path_iface_id(), "net_é1");
        assert_eq!(config.body_iface_id(), "net_é1");
    }

    #[test]
    fn rejects_put_network_interface_mismatched_body_id() {
        let body = r#"{
            "iface_id": "eth1",
            "host_dev_name": "tap0"
        }"#;
        let request = request_with_body("PUT", "/network-interfaces/eth0", body);

        assert_eq!(
            parse_request(&request),
            Err(RequestError::MismatchedInterfaceId)
        );
    }

    #[test]
    fn rejects_put_network_interface_without_path_id() {
        let body = r#"{
            "iface_id": "eth0",
            "host_dev_name": "tap0"
        }"#;

        assert_eq!(
            parse_request(&request_with_body("PUT", "/network-interfaces", body)),
            Err(RequestError::InvalidPathMethod)
        );
        assert_eq!(
            parse_request(&request_with_body("PUT", "/network-interfaces/", body)),
            Err(RequestError::InvalidPathMethod)
        );
    }

    #[test]
    fn rejects_put_network_interface_extra_path_segment() {
        let body = r#"{
            "iface_id": "eth0",
            "host_dev_name": "tap0"
        }"#;

        assert_eq!(
            parse_request(&request_with_body(
                "PUT",
                "/network-interfaces/eth0/extra",
                body,
            )),
            Err(RequestError::InvalidPathMethod)
        );
    }

    #[test]
    fn rejects_put_network_interface_invalid_path_id() {
        let body = r#"{
            "iface_id": "eth0",
            "host_dev_name": "tap0"
        }"#;

        assert_eq!(
            parse_request(&request_with_body("PUT", "/network-interfaces/eth-0", body)),
            Err(RequestError::InvalidPathMethod)
        );
        assert_eq!(
            parse_request(&request_with_body(
                "PUT",
                "/network-interfaces/eth0?debug=true",
                body,
            )),
            Err(RequestError::InvalidPathMethod)
        );
    }

    #[test]
    fn rejects_put_network_interface_with_empty_body() {
        let request = b"PUT /network-interfaces/eth0 HTTP/1.1\r\nContent-Length: 0\r\n\r\n";

        assert_eq!(parse_request(request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_put_network_interface_missing_required_field() {
        let body = r#"{
            "iface_id": "eth0"
        }"#;
        let request = request_with_body("PUT", "/network-interfaces/eth0", body);

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_put_network_interface_unknown_field() {
        let body = r#"{
            "iface_id": "eth0",
            "host_dev_name": "tap0",
            "unknown": true
        }"#;
        let request = request_with_body("PUT", "/network-interfaces/eth0", body);

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_put_network_interface_invalid_rate_limiter_bucket() {
        let body = r#"{
            "iface_id": "eth0",
            "host_dev_name": "tap0",
            "rx_rate_limiter": {
                "ops": {
                    "size": 100
                }
            }
        }"#;
        let request = request_with_body("PUT", "/network-interfaces/eth0", body);

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_get_network_interface_with_body() {
        let request = request_with_body("GET", "/network-interfaces/eth0", "{}");

        assert_eq!(parse_request(&request), Err(RequestError::GetRequestBody));
    }

    #[test]
    fn rejects_get_version_with_transfer_encoding_body() {
        let request = b"GET /version HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n";

        assert_eq!(parse_request(request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn total_len_rejects_unsupported_transfer_encoding() {
        let request = b"GET /version HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n";

        assert_eq!(
            request_total_len(request),
            Err(RequestError::MalformedRequest)
        );
    }

    #[test]
    fn rejects_unsupported_method() {
        let request = b"PUT /version HTTP/1.1\r\nContent-Length: 0\r\n\r\n";

        assert_eq!(parse_request(request), Err(RequestError::InvalidPathMethod));
    }

    #[test]
    fn rejects_unsupported_path() {
        let request = b"GET /unknown HTTP/1.1\r\n\r\n";

        assert_eq!(parse_request(request), Err(RequestError::InvalidPathMethod));
    }

    #[test]
    fn rejects_malformed_request() {
        let request = b"not-http\r\n\r\n";

        assert_eq!(parse_request(request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_incomplete_body() {
        let request = b"GET /version HTTP/1.1\r\nContent-Length: 2\r\n\r\n{";

        assert_eq!(parse_request(request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_non_digit_content_length() {
        let request = b"GET /version HTTP/1.1\r\nContent-Length: +0\r\n\r\n";

        assert_eq!(parse_request(request), Err(RequestError::MalformedRequest));
        assert_eq!(
            request_total_len(request),
            Err(RequestError::MalformedRequest)
        );
    }

    #[test]
    fn rejects_duplicate_content_length() {
        let request = b"GET /version HTTP/1.1\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n";

        assert_eq!(parse_request(request), Err(RequestError::MalformedRequest));
        assert_eq!(
            request_total_len(request),
            Err(RequestError::MalformedRequest)
        );
    }

    #[test]
    fn rejects_declared_content_length_over_payload_limit() {
        let request = format!(
            "GET /version HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            HTTP_MAX_PAYLOAD_SIZE + 1
        );

        assert_eq!(
            parse_request(request.as_bytes()),
            Err(RequestError::PayloadTooLarge)
        );
        assert_eq!(
            request_total_len(request.as_bytes()),
            Err(RequestError::PayloadTooLarge)
        );
    }

    #[test]
    fn rejects_declared_content_length_over_usize() {
        let request =
            b"GET /version HTTP/1.1\r\nContent-Length: 999999999999999999999999999999\r\n\r\n";

        assert_eq!(parse_request(request), Err(RequestError::PayloadTooLarge));
        assert_eq!(
            request_total_len(request),
            Err(RequestError::PayloadTooLarge)
        );
    }

    #[test]
    fn rejects_request_over_payload_limit() {
        let request = vec![b'a'; HTTP_MAX_PAYLOAD_SIZE + 1];

        assert_eq!(parse_request(&request), Err(RequestError::PayloadTooLarge));
    }

    #[test]
    fn response_body_contains_firecracker_version() {
        let response = HttpResponse::version(VERSION);

        assert_eq!(response.status(), StatusCode::Ok);
        assert_eq!(response.body(), r#"{"firecracker_version":"0.1.0"}"#);
    }

    #[test]
    fn response_body_contains_instance_info() {
        let response = HttpResponse::instance_info("demo-1", "Not started", VERSION, "bangbang");
        let body: serde_json::Value =
            serde_json::from_str(response.body()).expect("body should be JSON");

        assert_eq!(response.status(), StatusCode::Ok);
        assert_eq!(
            body,
            serde_json::json!({
                "app_name": "bangbang",
                "id": "demo-1",
                "state": "Not started",
                "vmm_version": "0.1.0",
            })
        );
    }

    #[test]
    fn response_body_contains_machine_config() {
        let response = HttpResponse::machine_config(2, 256, false, false, "None");
        let body: serde_json::Value =
            serde_json::from_str(response.body()).expect("body should be JSON");

        assert_eq!(response.status(), StatusCode::Ok);
        assert_eq!(
            body,
            serde_json::json!({
                "huge_pages": "None",
                "mem_size_mib": 256,
                "smt": false,
                "track_dirty_pages": false,
                "vcpu_count": 2,
            })
        );
        assert_eq!(body.get("cpu_template"), None);
    }

    #[test]
    fn response_body_contains_default_vm_config() {
        let response = HttpResponse::vm_config(&VmConfigResponse::new(
            MachineConfigResponse::new(1, 128, false, false, "None"),
            None,
            Vec::new(),
            Vec::new(),
        ));
        let body: serde_json::Value =
            serde_json::from_str(response.body()).expect("body should be JSON");

        assert_eq!(response.status(), StatusCode::Ok);
        assert_eq!(
            body,
            serde_json::json!({
                "drives": [],
                "machine-config": {
                    "huge_pages": "None",
                    "mem_size_mib": 128,
                    "smt": false,
                    "track_dirty_pages": false,
                    "vcpu_count": 1,
                },
                "network-interfaces": [],
            })
        );
        assert_eq!(body.get("boot-source"), None);
        assert_eq!(body.get("logger"), None);
    }

    #[test]
    fn response_body_contains_configured_vm_config() {
        let boot_source = BootSourceResponse::new("/tmp/vmlinux")
            .with_initrd_path("/tmp/initrd.img")
            .with_boot_args("console=hvc0 reboot=k panic=1");
        let drive =
            DriveConfigResponse::new("rootfs", "/tmp/rootfs.ext4", true, true, "Unsafe", "Sync")
                .with_partuuid("0eaa91a0-01");
        let network_interface =
            NetworkInterfaceConfigResponse::new("eth0", "tap0").with_guest_mac("12:34:56:78:9a:bc");
        let response = HttpResponse::vm_config(&VmConfigResponse::new(
            MachineConfigResponse::new(2, 256, false, false, "None"),
            Some(boot_source),
            vec![drive],
            vec![network_interface],
        ));
        let body: serde_json::Value =
            serde_json::from_str(response.body()).expect("body should be JSON");

        assert_eq!(response.status(), StatusCode::Ok);
        assert_eq!(
            body,
            serde_json::json!({
                "boot-source": {
                    "boot_args": "console=hvc0 reboot=k panic=1",
                    "initrd_path": "/tmp/initrd.img",
                    "kernel_image_path": "/tmp/vmlinux",
                },
                "drives": [
                    {
                        "cache_type": "Unsafe",
                        "drive_id": "rootfs",
                        "io_engine": "Sync",
                        "is_read_only": true,
                        "is_root_device": true,
                        "partuuid": "0eaa91a0-01",
                        "path_on_host": "/tmp/rootfs.ext4",
                    },
                ],
                "machine-config": {
                    "huge_pages": "None",
                    "mem_size_mib": 256,
                    "smt": false,
                    "track_dirty_pages": false,
                    "vcpu_count": 2,
                },
                "network-interfaces": [
                    {
                        "guest_mac": "12:34:56:78:9a:bc",
                        "host_dev_name": "tap0",
                        "iface_id": "eth0",
                    },
                ],
            })
        );
        assert_eq!(body.get("metrics"), None);
    }

    #[test]
    fn response_body_omits_absent_optional_vm_config_fields() {
        let response = HttpResponse::vm_config(&VmConfigResponse::new(
            MachineConfigResponse::new(1, 128, false, false, "None"),
            Some(BootSourceResponse::new("/tmp/vmlinux")),
            vec![DriveConfigResponse::new(
                "data",
                "/tmp/data.ext4",
                false,
                false,
                "Unsafe",
                "Sync",
            )],
            vec![NetworkInterfaceConfigResponse::new("eth0", "tap0")],
        ));
        let body: serde_json::Value =
            serde_json::from_str(response.body()).expect("body should be JSON");

        assert_eq!(
            body.get("boot-source"),
            Some(&serde_json::json!({
                "kernel_image_path": "/tmp/vmlinux",
            }))
        );
        let drives = body
            .get("drives")
            .and_then(serde_json::Value::as_array)
            .expect("drives should be an array");
        let drive = drives.first().expect("one drive should be returned");
        assert_eq!(drive.get("partuuid"), None);
        assert_eq!(drive.get("rate_limiter"), None);
        let network_interfaces = body
            .get("network-interfaces")
            .and_then(serde_json::Value::as_array)
            .expect("network interfaces should be an array");
        let network_interface = network_interfaces
            .first()
            .expect("one network interface should be returned");
        assert_eq!(network_interface.get("guest_mac"), None);
        assert_eq!(network_interface.get("rx_rate_limiter"), None);
    }

    #[test]
    fn fault_body_contains_fault_message() {
        let response = HttpResponse::fault("message");

        assert_eq!(response.status(), StatusCode::BadRequest);
        assert_eq!(response.body(), r#"{"fault_message":"message"}"#);
    }

    #[test]
    fn no_content_response_has_empty_body() {
        let response = HttpResponse::no_content();

        assert_eq!(response.status(), StatusCode::NoContent);
        assert_eq!(response.body(), "");
    }

    #[test]
    fn response_bytes_include_http_headers() {
        let response = HttpResponse::machine_config(1, 128, false, false, "None");
        let bytes = response.to_http_bytes();
        let text = std::str::from_utf8(&bytes).expect("response should be utf-8");

        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("Content-Type: application/json\r\n"));
        assert!(text.contains(&format!("Content-Length: {}\r\n", response.body().len())));
        assert!(text.ends_with(response.body()));
    }

    #[test]
    fn no_content_response_bytes_have_zero_length_and_no_json_body() {
        let response = HttpResponse::no_content();
        let bytes = response.to_http_bytes();
        let text = std::str::from_utf8(&bytes).expect("response should be utf-8");

        assert!(text.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert!(!text.contains("Content-Type: application/json\r\n"));
        assert!(text.contains("Content-Length: 0\r\n"));
        assert!(text.ends_with("\r\n\r\n"));
    }

    #[test]
    fn api_request_converts_to_endpoint() {
        assert_eq!(
            Endpoint::from(ApiRequest::GetInstanceInfo),
            Endpoint::DescribeInstance
        );
        assert_eq!(Endpoint::from(ApiRequest::GetVersion), Endpoint::Version);
        assert_eq!(
            Endpoint::from(ApiRequest::GetMachineConfig),
            Endpoint::MachineConfig
        );
        assert_eq!(Endpoint::from(ApiRequest::GetVmConfig), Endpoint::VmConfig);
        let request = parse_request(&request_with_body(
            "PUT",
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        ))
        .expect("actions request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::Actions);

        let request = parse_request(&request_with_body(
            "PUT",
            "/boot-source",
            r#"{"kernel_image_path":"/tmp/vmlinux"}"#,
        ))
        .expect("boot-source request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::BootSource);

        let request = parse_request(&request_with_body("PUT", "/logger", "{}"))
            .expect("logger request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::Logger);

        let request = parse_request(&request_with_body(
            "PUT",
            "/drives/rootfs",
            r#"{
                "drive_id": "rootfs",
                "path_on_host": "/tmp/rootfs.ext4",
                "is_root_device": true
            }"#,
        ))
        .expect("drive request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::Drive);

        let request = parse_request(&request_with_body(
            "PUT",
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":128}"#,
        ))
        .expect("machine-config request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::MachineConfig);

        let request = parse_request(&request_with_body(
            "PUT",
            "/metrics",
            r#"{"metrics_path":"/tmp/metrics"}"#,
        ))
        .expect("metrics request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::Metrics);

        let request = parse_request(&request_with_body(
            "PUT",
            "/network-interfaces/eth0",
            r#"{
                "iface_id": "eth0",
                "host_dev_name": "tap0"
            }"#,
        ))
        .expect("network interface request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::NetworkInterface);
    }
}
