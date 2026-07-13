use std::fmt;
use std::net::Ipv4Addr;

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

use crate::route::Endpoint;
use crate::{HTTP_MAX_PAYLOAD_SIZE, HTTP_MAX_REQUEST_HEAD_SIZE};

const MAX_HEADERS: usize = 32;
const RATE_LIMITER_BANDWIDTH_FIELD: &str = "bandwidth";
const RATE_LIMITER_OPS_FIELD: &str = "ops";
const TOKEN_BUCKET_SIZE_FIELD: &str = "size";
const TOKEN_BUCKET_ONE_TIME_BURST_FIELD: &str = "one_time_burst";
const TOKEN_BUCKET_REFILL_TIME_FIELD: &str = "refill_time";
const MAX_MACHINE_CONFIG_VCPUS: u8 = 32;
const ARM64_KVM_REG_SIZE_MASK: u64 = 0x00f0_0000_0000_0000;
const ARM64_KVM_REG_SIZE_SHIFT: u32 = 52;
const SNAPSHOT_VALUE_REDACTED: &str = "<redacted>";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiRequest {
    GetInstanceInfo,
    GetBalloon,
    GetBalloonStats,
    GetBalloonHintingStatus,
    GetMemoryHotplug,
    GetMachineConfig,
    GetMmds,
    GetVmConfig,
    GetVersion,
    PutAction(Box<ActionRequest>),
    PutBalloon(Box<BalloonConfigRequest>),
    PutBootSource(Box<BootSourceRequest>),
    PutCpuConfig(Box<CpuConfigRequest>),
    PutDrive(Box<DriveConfigRequest>),
    PutEntropy(Box<EntropyConfigRequest>),
    PutMemoryHotplug(MemoryHotplugConfigRequest),
    PatchBalloon(Box<BalloonUpdateRequest>),
    PatchBalloonStats(Box<BalloonStatsUpdateRequest>),
    PatchBalloonHintingStart(BalloonHintingStartRequest),
    PatchBalloonHintingStop,
    PatchMemoryHotplug(MemoryHotplugSizeUpdateRequest),
    PatchDrive(Box<DrivePatchRequest>),
    PatchVmState(Box<VmStateUpdateRequest>),
    PutLogger(Box<LoggerConfigRequest>),
    PutMachineConfig(Box<MachineConfigRequest>),
    PatchMachineConfig(Box<MachineConfigPatchRequest>),
    PutMetrics(Box<MetricsConfigRequest>),
    PutMmds(Box<MmdsContentRequest>),
    PutMmdsConfig(Box<MmdsConfigRequest>),
    PutNetworkInterface(Box<NetworkInterfaceConfigRequest>),
    PatchNetworkInterface(Box<NetworkInterfacePatchRequest>),
    HotUnplugDevice(Box<HotUnplugDeviceRequest>),
    PutPmem(Box<PmemConfigRequest>),
    PatchPmem(Box<PmemPatchRequest>),
    PutSerial(Box<SerialConfigRequest>),
    PutSnapshotCreate(SnapshotCreateRequest),
    PutSnapshotLoad(SnapshotLoadRequest),
    PutVsock(Box<VsockConfigRequest>),
    PatchMmds(Box<MmdsContentRequest>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestError {
    BalloonUnsupported,
    EmptyDeleteRequest,
    EmptyPatchRequest,
    EmptyPutRequest,
    GetRequestBody,
    InvalidPathMethod,
    MismatchedDriveId,
    MismatchedInterfaceId,
    MismatchedPmemId,
    MalformedRequest,
    NetworkInterfaceUpdateUnsupported,
    PayloadTooLarge,
    PmemUnsupported,
    SendCtrlAltDelUnsupported,
}

impl RequestError {
    pub fn fault_message(&self) -> &'static str {
        match self {
            Self::BalloonUnsupported => "Balloon device is not supported.",
            Self::EmptyDeleteRequest => "Empty Delete request.",
            Self::EmptyPatchRequest => "Empty PATCH request.",
            Self::EmptyPutRequest => "Empty PUT request.",
            Self::GetRequestBody => "GET request cannot have a body.",
            Self::InvalidPathMethod => "Invalid request method and/or path.",
            Self::MismatchedDriveId => "path drive_id must match body drive_id.",
            Self::MismatchedInterfaceId => "path iface_id must match body iface_id.",
            Self::MismatchedPmemId => "path pmem id must match body id.",
            Self::MalformedRequest => "Malformed HTTP request.",
            Self::NetworkInterfaceUpdateUnsupported => {
                "Network interface updates are not supported."
            }
            Self::PayloadTooLarge => "HTTP request payload exceeds the configured limit.",
            Self::PmemUnsupported => "Pmem device is not supported.",
            Self::SendCtrlAltDelUnsupported => "SendCtrlAltDel is not supported on aarch64.",
        }
    }
}

impl fmt::Display for RequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.fault_message())
    }
}

impl std::error::Error for RequestError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiRequestMetricEndpoint {
    Patch(ApiRequestMetricPatchEndpoint),
    Put(ApiRequestMetricPutEndpoint),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiRequestMetricPutEndpoint {
    Actions,
    Balloon,
    BootSource,
    CpuConfig,
    Drive,
    HotplugMemory,
    Logger,
    MachineConfig,
    Metrics,
    Mmds,
    Network,
    Pmem,
    Serial,
    Vsock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiRequestMetricPatchEndpoint {
    Balloon,
    Drive,
    HotplugMemory,
    MachineConfig,
    Mmds,
    Network,
    Pmem,
}

pub fn api_request_metric_endpoint(bytes: &[u8]) -> Option<ApiRequestMetricEndpoint> {
    let (method, path, _, _) = parse_request_head(bytes).ok()?;

    match method {
        "PATCH" => patch_api_request_metric_endpoint(path).map(ApiRequestMetricEndpoint::Patch),
        "PUT" => put_api_request_metric_endpoint(path).map(ApiRequestMetricEndpoint::Put),
        _ => None,
    }
}

fn put_api_request_metric_endpoint(path: &str) -> Option<ApiRequestMetricPutEndpoint> {
    if drive_path_id(path).is_some() {
        return Some(ApiRequestMetricPutEndpoint::Drive);
    }
    if network_interface_path_id(path).is_some() {
        return Some(ApiRequestMetricPutEndpoint::Network);
    }
    if pmem_path_id(path).is_some() {
        return Some(ApiRequestMetricPutEndpoint::Pmem);
    }

    match path {
        "/actions" => Some(ApiRequestMetricPutEndpoint::Actions),
        "/balloon" => Some(ApiRequestMetricPutEndpoint::Balloon),
        "/boot-source" => Some(ApiRequestMetricPutEndpoint::BootSource),
        "/cpu-config" => Some(ApiRequestMetricPutEndpoint::CpuConfig),
        "/hotplug/memory" => Some(ApiRequestMetricPutEndpoint::HotplugMemory),
        "/logger" => Some(ApiRequestMetricPutEndpoint::Logger),
        "/machine-config" => Some(ApiRequestMetricPutEndpoint::MachineConfig),
        "/metrics" => Some(ApiRequestMetricPutEndpoint::Metrics),
        "/mmds" | "/mmds/config" => Some(ApiRequestMetricPutEndpoint::Mmds),
        "/serial" => Some(ApiRequestMetricPutEndpoint::Serial),
        "/vsock" => Some(ApiRequestMetricPutEndpoint::Vsock),
        _ => None,
    }
}

fn patch_api_request_metric_endpoint(path: &str) -> Option<ApiRequestMetricPatchEndpoint> {
    if drive_path_id(path).is_some() {
        return Some(ApiRequestMetricPatchEndpoint::Drive);
    }
    if network_interface_path_id(path).is_some() {
        return Some(ApiRequestMetricPatchEndpoint::Network);
    }
    if pmem_path_id(path).is_some() {
        return Some(ApiRequestMetricPatchEndpoint::Pmem);
    }

    match path {
        "/balloon" | "/balloon/statistics" | "/balloon/hinting/start" | "/balloon/hinting/stop" => {
            Some(ApiRequestMetricPatchEndpoint::Balloon)
        }
        "/hotplug/memory" => Some(ApiRequestMetricPatchEndpoint::HotplugMemory),
        "/machine-config" => Some(ApiRequestMetricPatchEndpoint::MachineConfig),
        "/mmds" => Some(ApiRequestMetricPatchEndpoint::Mmds),
        _ => None,
    }
}

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
pub enum HotUnplugDeviceKind {
    Drive,
    NetworkInterface,
    Pmem,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotUnplugDeviceRequest {
    kind: HotUnplugDeviceKind,
    id: String,
}

impl HotUnplugDeviceRequest {
    pub fn new(kind: HotUnplugDeviceKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }

    pub const fn kind(&self) -> HotUnplugDeviceKind {
        self.kind
    }

    pub fn id(&self) -> &str {
        &self.id
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuConfigRequest {
    category: Option<CpuConfigTemplateCategory>,
}

impl CpuConfigRequest {
    pub const fn category(&self) -> Option<CpuConfigTemplateCategory> {
        self.category
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuConfigTemplateCategory {
    KvmCapabilities,
    VcpuFeatures,
    ArmRegisterModifiers,
    Mixed,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CpuConfigRequestBody {
    #[serde(default)]
    kvm_capabilities: Vec<CpuConfigKvmCapability>,
    #[serde(default)]
    reg_modifiers: Vec<CpuConfigArmRegisterModifier>,
    #[serde(default)]
    vcpu_features: Vec<CpuConfigVcpuFeature>,
}

impl CpuConfigRequestBody {
    fn validate(&self) -> Result<(), ()> {
        for capability in &self.kvm_capabilities {
            capability.validate()?;
        }
        for modifier in &self.reg_modifiers {
            modifier.validate()?;
        }
        for feature in &self.vcpu_features {
            feature.validate()?;
        }
        Ok(())
    }

    fn category(&self) -> Option<CpuConfigTemplateCategory> {
        match (
            self.kvm_capabilities.is_empty(),
            self.reg_modifiers.is_empty(),
            self.vcpu_features.is_empty(),
        ) {
            (true, true, true) => None,
            (false, true, true) => Some(CpuConfigTemplateCategory::KvmCapabilities),
            (true, false, true) => Some(CpuConfigTemplateCategory::ArmRegisterModifiers),
            (true, true, false) => Some(CpuConfigTemplateCategory::VcpuFeatures),
            _ => Some(CpuConfigTemplateCategory::Mixed),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
struct CpuConfigKvmCapability(String);

impl CpuConfigKvmCapability {
    fn validate(&self) -> Result<(), ()> {
        let capability = self.0.strip_prefix('!').unwrap_or(&self.0);

        if capability.is_empty() {
            return Err(());
        }

        capability.parse::<u32>().map(|_| ()).map_err(|_| ())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CpuConfigArmRegisterModifier {
    addr: String,
    bitmap: String,
}

impl CpuConfigArmRegisterModifier {
    fn validate(&self) -> Result<(), ()> {
        let register_id = validate_prefixed_u64(&self.addr)?;
        let register_bits = validate_arm64_register_bits(register_id)?;
        let bitmap = validate_bitmap(&self.bitmap, u128::BITS)?;

        if let Some(limit) = register_bitmap_limit(register_bits)
            && (bitmap.value > limit || bitmap.filter > limit)
        {
            return Err(());
        }

        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CpuConfigVcpuFeature {
    index: u32,
    bitmap: String,
}

impl CpuConfigVcpuFeature {
    fn validate(&self) -> Result<(), ()> {
        let _ = self.index;
        validate_bitmap(&self.bitmap, u32::BITS).map(|_| ())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmStateUpdateRequest {
    state: VmStateUpdate,
}

impl VmStateUpdateRequest {
    pub const fn state(&self) -> VmStateUpdate {
        self.state
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmStateUpdate {
    Paused,
    Resumed,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VmStateUpdateRequestBody {
    state: VmStateUpdateBody,
}

#[derive(Debug, Clone, Copy, Deserialize)]
enum VmStateUpdateBody {
    Paused,
    Resumed,
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
pub struct SerialConfigRequest {
    serial_out_path: Option<String>,
    rate_limiter: Option<TokenBucketRequest>,
}

impl SerialConfigRequest {
    pub fn serial_out_path(&self) -> Option<&str> {
        self.serial_out_path.as_deref()
    }

    pub const fn rate_limiter(&self) -> Option<TokenBucketRequest> {
        self.rate_limiter
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenBucketRequest {
    size: u64,
    one_time_burst: Option<u64>,
    refill_time: u64,
}

pub type SerialRateLimiterRequest = TokenBucketRequest;

impl TokenBucketRequest {
    pub const fn new(size: u64, one_time_burst: Option<u64>, refill_time: u64) -> Self {
        Self {
            size,
            one_time_burst,
            refill_time,
        }
    }

    pub const fn size(self) -> u64 {
        self.size
    }

    pub const fn one_time_burst(self) -> Option<u64> {
        self.one_time_burst
    }

    pub const fn refill_time(self) -> u64 {
        self.refill_time
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntropyConfigRequest {
    rate_limiter: Option<EntropyRateLimiterRequest>,
}

impl EntropyConfigRequest {
    pub const fn rate_limiter(&self) -> Option<EntropyRateLimiterRequest> {
        self.rate_limiter
    }

    pub const fn rate_limiter_configured(&self) -> bool {
        self.rate_limiter.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntropyRateLimiterRequest {
    bandwidth: Option<TokenBucketRequest>,
    ops: Option<TokenBucketRequest>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriveRateLimiterRequest {
    bandwidth: Option<TokenBucketRequest>,
    ops: Option<TokenBucketRequest>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkRateLimiterRequest {
    bandwidth: Option<TokenBucketRequest>,
    ops: Option<TokenBucketRequest>,
}

type RateLimiterBuckets = (Option<TokenBucketRequest>, Option<TokenBucketRequest>);

impl DriveRateLimiterRequest {
    pub const fn new(
        bandwidth: Option<TokenBucketRequest>,
        ops: Option<TokenBucketRequest>,
    ) -> Self {
        Self { bandwidth, ops }
    }

    pub const fn bandwidth(self) -> Option<TokenBucketRequest> {
        self.bandwidth
    }

    pub const fn ops(self) -> Option<TokenBucketRequest> {
        self.ops
    }
}

impl EntropyRateLimiterRequest {
    pub const fn new(
        bandwidth: Option<TokenBucketRequest>,
        ops: Option<TokenBucketRequest>,
    ) -> Self {
        Self { bandwidth, ops }
    }

    pub const fn bandwidth(self) -> Option<TokenBucketRequest> {
        self.bandwidth
    }

    pub const fn ops(self) -> Option<TokenBucketRequest> {
        self.ops
    }
}

impl NetworkRateLimiterRequest {
    pub const fn new(
        bandwidth: Option<TokenBucketRequest>,
        ops: Option<TokenBucketRequest>,
    ) -> Self {
        Self { bandwidth, ops }
    }

    pub const fn bandwidth(self) -> Option<TokenBucketRequest> {
        self.bandwidth
    }

    pub const fn ops(self) -> Option<TokenBucketRequest> {
        self.ops
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalloonConfigRequest {
    amount_mib: u32,
    deflate_on_oom: bool,
    stats_polling_interval_s: u16,
    free_page_hinting: bool,
    free_page_reporting: bool,
}

impl BalloonConfigRequest {
    pub const fn amount_mib(self) -> u32 {
        self.amount_mib
    }

    pub const fn deflate_on_oom(self) -> bool {
        self.deflate_on_oom
    }

    pub const fn stats_polling_interval_s(self) -> u16 {
        self.stats_polling_interval_s
    }

    pub const fn free_page_hinting(self) -> bool {
        self.free_page_hinting
    }

    pub const fn free_page_reporting(self) -> bool {
        self.free_page_reporting
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalloonUpdateRequest {
    amount_mib: u32,
}

impl BalloonUpdateRequest {
    pub const fn amount_mib(self) -> u32 {
        self.amount_mib
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalloonStatsUpdateRequest {
    stats_polling_interval_s: u16,
}

impl BalloonStatsUpdateRequest {
    pub const fn stats_polling_interval_s(self) -> u16 {
        self.stats_polling_interval_s
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SerialConfigRequestBody {
    #[serde(default)]
    serial_out_path: Option<String>,
    #[serde(default)]
    rate_limiter: Option<JsonValueWithoutDuplicateObjectKeys>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntropyDeviceConfigRequestBody {
    #[serde(default)]
    rate_limiter: Option<JsonValueWithoutDuplicateObjectKeys>,
}

#[derive(Debug)]
struct JsonValueWithoutDuplicateObjectKeys(serde_json::Value);

impl JsonValueWithoutDuplicateObjectKeys {
    const fn as_value(&self) -> &serde_json::Value {
        &self.0
    }
}

impl<'de> Deserialize<'de> for JsonValueWithoutDuplicateObjectKeys {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer
            .deserialize_any(JsonValueWithoutDuplicateObjectKeysVisitor)
            .map(Self)
    }
}

#[derive(Debug)]
struct JsonValueWithoutDuplicateObjectKeysVisitor;

impl<'de> Visitor<'de> for JsonValueWithoutDuplicateObjectKeysVisitor {
    type Value = serde_json::Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Number(value.into()))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Number(value.into()))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        serde_json::Number::from_f64(value)
            .map(serde_json::Value::Number)
            .ok_or_else(|| E::custom("invalid JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(serde_json::Value::String(value.to_string()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(serde_json::Value::String(value))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Null)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        Deserialize::deserialize(deserializer)
            .map(|JsonValueWithoutDuplicateObjectKeys(value)| value)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or(0));

        while let Some(JsonValueWithoutDuplicateObjectKeys(value)) = sequence.next_element()? {
            values.push(value);
        }

        Ok(serde_json::Value::Array(values))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut object = serde_json::Map::new();

        while let Some(key) = map.next_key::<String>()? {
            if object.contains_key(&key) {
                return Err(de::Error::custom("duplicate object key"));
            }

            let JsonValueWithoutDuplicateObjectKeys(value) = map.next_value()?;
            object.insert(key, value);
        }

        Ok(serde_json::Value::Object(object))
    }
}

const fn default_memory_hotplug_block_size_mib() -> u64 {
    2
}

const fn default_memory_hotplug_slot_size_mib() -> u64 {
    128
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryHotplugConfigRequestBody {
    total_size_mib: u64,
    #[serde(default = "default_memory_hotplug_block_size_mib")]
    block_size_mib: u64,
    #[serde(default = "default_memory_hotplug_slot_size_mib")]
    slot_size_mib: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryHotplugSizeUpdateRequestBody {
    requested_size_mib: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryHotplugConfigRequest {
    total_size_mib: u64,
    block_size_mib: u64,
    slot_size_mib: u64,
}

impl MemoryHotplugConfigRequest {
    pub const fn new(total_size_mib: u64, block_size_mib: u64, slot_size_mib: u64) -> Self {
        Self {
            total_size_mib,
            block_size_mib,
            slot_size_mib,
        }
    }

    pub const fn total_size_mib(self) -> u64 {
        self.total_size_mib
    }

    pub const fn block_size_mib(self) -> u64 {
        self.block_size_mib
    }

    pub const fn slot_size_mib(self) -> u64 {
        self.slot_size_mib
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryHotplugSizeUpdateRequest {
    requested_size_mib: u64,
}

impl MemoryHotplugSizeUpdateRequest {
    pub const fn new(requested_size_mib: u64) -> Self {
        Self { requested_size_mib }
    }

    pub const fn requested_size_mib(self) -> u64 {
        self.requested_size_mib
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineConfigPatchRequest {
    vcpu_count: Option<u8>,
    mem_size_mib: Option<u64>,
    smt: Option<bool>,
    cpu_template: Option<MachineConfigCpuTemplate>,
    track_dirty_pages: Option<bool>,
    huge_pages: Option<MachineConfigHugePages>,
}

impl MachineConfigPatchRequest {
    pub const fn vcpu_count(&self) -> Option<u8> {
        self.vcpu_count
    }

    pub const fn mem_size_mib(&self) -> Option<u64> {
        self.mem_size_mib
    }

    pub const fn smt(&self) -> Option<bool> {
        self.smt
    }

    pub const fn cpu_template(&self) -> Option<MachineConfigCpuTemplate> {
        self.cpu_template
    }

    pub const fn track_dirty_pages(&self) -> Option<bool> {
        self.track_dirty_pages
    }

    pub const fn huge_pages(&self) -> Option<MachineConfigHugePages> {
        self.huge_pages
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum MachineConfigCpuTemplate {
    C3,
    T2,
    T2S,
    T2CL,
    T2A,
    V1N1,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MachineConfigPatchRequestBody {
    #[serde(default)]
    vcpu_count: Option<u8>,
    #[serde(default)]
    mem_size_mib: Option<u64>,
    #[serde(default)]
    smt: Option<bool>,
    #[serde(default)]
    cpu_template: Option<MachineConfigCpuTemplate>,
    #[serde(default)]
    track_dirty_pages: Option<bool>,
    #[serde(default)]
    huge_pages: Option<MachineConfigHugePages>,
}

impl MachineConfigPatchRequestBody {
    const fn is_empty(&self) -> bool {
        self.vcpu_count.is_none()
            && self.mem_size_mib.is_none()
            && self.smt.is_none()
            && self.cpu_template.is_none()
            && self.track_dirty_pages.is_none()
            && self.huge_pages.is_none()
    }
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
pub struct MmdsContentRequest {
    value: serde_json::Value,
}

impl MmdsContentRequest {
    pub fn value(&self) -> &serde_json::Value {
        &self.value
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmdsConfigRequest {
    network_interfaces: Vec<String>,
    version: MmdsVersion,
    ipv4_address: Option<Ipv4Addr>,
    imds_compat: bool,
}

impl MmdsConfigRequest {
    pub fn network_interfaces(&self) -> &[String] {
        &self.network_interfaces
    }

    pub const fn version(&self) -> MmdsVersion {
        self.version
    }

    pub const fn ipv4_address(&self) -> Option<Ipv4Addr> {
        self.ipv4_address
    }

    pub const fn imds_compat(&self) -> bool {
        self.imds_compat
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub enum MmdsVersion {
    #[default]
    V1,
    V2,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MmdsConfigRequestBody {
    network_interfaces: Vec<String>,
    #[serde(default)]
    version: MmdsVersion,
    #[serde(default)]
    ipv4_address: Option<Ipv4Addr>,
    #[serde(default)]
    imds_compat: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveConfigRequest {
    path_drive_id: String,
    body_drive_id: String,
    path_on_host: Option<String>,
    is_root_device: bool,
    is_read_only: Option<bool>,
    partuuid: Option<String>,
    cache_type: Option<DriveCacheType>,
    io_engine: Option<DriveIoEngine>,
    rate_limiter: Option<DriveRateLimiterRequest>,
    socket: Option<String>,
}

impl DriveConfigRequest {
    pub fn path_drive_id(&self) -> &str {
        &self.path_drive_id
    }

    pub fn body_drive_id(&self) -> &str {
        &self.body_drive_id
    }

    pub fn path_on_host(&self) -> Option<&str> {
        self.path_on_host.as_deref()
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

    pub const fn rate_limiter(&self) -> Option<DriveRateLimiterRequest> {
        self.rate_limiter
    }

    pub const fn rate_limiter_configured(&self) -> bool {
        self.rate_limiter.is_some()
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
    mtu: Option<u16>,
    rx_rate_limiter: Option<NetworkRateLimiterRequest>,
    tx_rate_limiter: Option<NetworkRateLimiterRequest>,
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

    pub const fn mtu(&self) -> Option<u16> {
        self.mtu
    }

    pub const fn rx_rate_limiter(&self) -> Option<NetworkRateLimiterRequest> {
        self.rx_rate_limiter
    }

    pub const fn tx_rate_limiter(&self) -> Option<NetworkRateLimiterRequest> {
        self.tx_rate_limiter
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkInterfacePatchRequest {
    path_iface_id: String,
    body_iface_id: String,
    rx_rate_limiter_configured: bool,
    tx_rate_limiter_configured: bool,
}

impl NetworkInterfacePatchRequest {
    pub fn path_iface_id(&self) -> &str {
        &self.path_iface_id
    }

    pub fn body_iface_id(&self) -> &str {
        &self.body_iface_id
    }

    pub const fn rx_rate_limiter_configured(&self) -> bool {
        self.rx_rate_limiter_configured
    }

    pub const fn tx_rate_limiter_configured(&self) -> bool {
        self.tx_rate_limiter_configured
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PmemConfigRequest {
    path_pmem_id: String,
    body_pmem_id: String,
    path_on_host: String,
    root_device: bool,
    read_only: bool,
    rate_limiter_configured: bool,
}

impl PmemConfigRequest {
    pub fn path_pmem_id(&self) -> &str {
        &self.path_pmem_id
    }

    pub fn body_pmem_id(&self) -> &str {
        &self.body_pmem_id
    }

    pub fn path_on_host(&self) -> &str {
        &self.path_on_host
    }

    pub const fn root_device(&self) -> bool {
        self.root_device
    }

    pub const fn read_only(&self) -> bool {
        self.read_only
    }

    pub const fn rate_limiter_configured(&self) -> bool {
        self.rate_limiter_configured
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PmemPatchRequest {
    path_pmem_id: String,
    body_pmem_id: String,
    rate_limiter_configured: bool,
}

impl PmemPatchRequest {
    pub fn path_pmem_id(&self) -> &str {
        &self.path_pmem_id
    }

    pub fn body_pmem_id(&self) -> &str {
        &self.body_pmem_id
    }

    pub const fn rate_limiter_configured(&self) -> bool {
        self.rate_limiter_configured
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VsockConfigRequest {
    vsock_id: Option<String>,
    guest_cid: u32,
    uds_path: String,
}

impl VsockConfigRequest {
    pub fn vsock_id(&self) -> Option<&str> {
        self.vsock_id.as_deref()
    }

    pub const fn guest_cid(&self) -> u32 {
        self.guest_cid
    }

    pub fn uds_path(&self) -> &str {
        &self.uds_path
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
    rate_limiter: Option<DriveRateLimiterRequest>,
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
            rate_limiter: None,
        }
    }

    pub fn with_partuuid(mut self, partuuid: impl Into<String>) -> Self {
        self.partuuid = Some(partuuid.into());
        self
    }

    pub const fn with_rate_limiter(mut self, rate_limiter: DriveRateLimiterRequest) -> Self {
        self.rate_limiter = Some(rate_limiter);
        self
    }

    pub const fn rate_limiter(&self) -> Option<DriveRateLimiterRequest> {
        self.rate_limiter
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkInterfaceConfigResponse {
    iface_id: String,
    host_dev_name: String,
    guest_mac: Option<String>,
    mtu: Option<u16>,
    rx_rate_limiter: Option<NetworkRateLimiterRequest>,
    tx_rate_limiter: Option<NetworkRateLimiterRequest>,
}

impl NetworkInterfaceConfigResponse {
    pub fn new(iface_id: impl Into<String>, host_dev_name: impl Into<String>) -> Self {
        Self {
            iface_id: iface_id.into(),
            host_dev_name: host_dev_name.into(),
            guest_mac: None,
            mtu: None,
            rx_rate_limiter: None,
            tx_rate_limiter: None,
        }
    }

    pub fn with_guest_mac(mut self, guest_mac: impl Into<String>) -> Self {
        self.guest_mac = Some(guest_mac.into());
        self
    }

    pub const fn with_mtu(mut self, mtu: u16) -> Self {
        self.mtu = Some(mtu);
        self
    }

    pub const fn with_rx_rate_limiter(mut self, rate_limiter: NetworkRateLimiterRequest) -> Self {
        self.rx_rate_limiter = Some(rate_limiter);
        self
    }

    pub const fn with_tx_rate_limiter(mut self, rate_limiter: NetworkRateLimiterRequest) -> Self {
        self.tx_rate_limiter = Some(rate_limiter);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PmemConfigResponse {
    id: String,
    path_on_host: String,
    root_device: bool,
    read_only: bool,
}

impl PmemConfigResponse {
    pub fn new(
        id: impl Into<String>,
        path_on_host: impl Into<String>,
        root_device: bool,
        read_only: bool,
    ) -> Self {
        Self {
            id: id.into(),
            path_on_host: path_on_host.into(),
            root_device,
            read_only,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryHotplugConfigResponse {
    total_size_mib: u64,
    block_size_mib: u64,
    slot_size_mib: u64,
}

impl MemoryHotplugConfigResponse {
    pub const fn new(total_size_mib: u64, block_size_mib: u64, slot_size_mib: u64) -> Self {
        Self {
            total_size_mib,
            block_size_mib,
            slot_size_mib,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryHotplugStatusResponse {
    total_size_mib: u64,
    block_size_mib: u64,
    slot_size_mib: u64,
    plugged_size_mib: u64,
    requested_size_mib: u64,
}

impl MemoryHotplugStatusResponse {
    pub const fn new(
        total_size_mib: u64,
        block_size_mib: u64,
        slot_size_mib: u64,
        plugged_size_mib: u64,
        requested_size_mib: u64,
    ) -> Self {
        Self {
            total_size_mib,
            block_size_mib,
            slot_size_mib,
            plugged_size_mib,
            requested_size_mib,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmConfigResponse {
    machine_config: MachineConfigResponse,
    boot_source: Option<BootSourceResponse>,
    drives: Vec<DriveConfigResponse>,
    network_interfaces: Vec<NetworkInterfaceConfigResponse>,
    mmds_config: Option<MmdsConfigResponse>,
    vsock: Option<VsockConfigResponse>,
    entropy: Option<EntropyConfigResponse>,
    memory_hotplug: Option<MemoryHotplugConfigResponse>,
    balloon: Option<BalloonConfigResponse>,
    pmem: Vec<PmemConfigResponse>,
}

impl VmConfigResponse {
    pub fn new(
        machine_config: MachineConfigResponse,
        boot_source: Option<BootSourceResponse>,
        drives: Vec<DriveConfigResponse>,
        network_interfaces: Vec<NetworkInterfaceConfigResponse>,
        mmds_config: Option<MmdsConfigResponse>,
        vsock: Option<VsockConfigResponse>,
        entropy: Option<EntropyConfigResponse>,
    ) -> Self {
        Self {
            machine_config,
            boot_source,
            drives,
            network_interfaces,
            mmds_config,
            vsock,
            entropy,
            memory_hotplug: None,
            balloon: None,
            pmem: Vec::new(),
        }
    }

    pub const fn with_memory_hotplug(
        mut self,
        memory_hotplug: Option<MemoryHotplugConfigResponse>,
    ) -> Self {
        self.memory_hotplug = memory_hotplug;
        self
    }

    pub const fn with_balloon(mut self, balloon: Option<BalloonConfigResponse>) -> Self {
        self.balloon = balloon;
        self
    }

    pub fn with_pmem(mut self, pmem: Vec<PmemConfigResponse>) -> Self {
        self.pmem = pmem;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalloonConfigResponse {
    amount_mib: u32,
    deflate_on_oom: bool,
    stats_polling_interval_s: u16,
    free_page_hinting: bool,
    free_page_reporting: bool,
}

impl BalloonConfigResponse {
    pub const fn new(
        amount_mib: u32,
        deflate_on_oom: bool,
        stats_polling_interval_s: u16,
        free_page_hinting: bool,
        free_page_reporting: bool,
    ) -> Self {
        Self {
            amount_mib,
            deflate_on_oom,
            stats_polling_interval_s,
            free_page_hinting,
            free_page_reporting,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalloonStatsResponse {
    target_pages: u32,
    actual_pages: u32,
    target_mib: u32,
    actual_mib: u32,
    swap_in: Option<u64>,
    swap_out: Option<u64>,
    major_faults: Option<u64>,
    minor_faults: Option<u64>,
    free_memory: Option<u64>,
    total_memory: Option<u64>,
    available_memory: Option<u64>,
    disk_caches: Option<u64>,
    hugetlb_allocations: Option<u64>,
    hugetlb_failures: Option<u64>,
    oom_kill: Option<u64>,
    alloc_stall: Option<u64>,
    async_scan: Option<u64>,
    direct_scan: Option<u64>,
    async_reclaim: Option<u64>,
    direct_reclaim: Option<u64>,
}

impl BalloonStatsResponse {
    pub const fn new(
        target_pages: u32,
        actual_pages: u32,
        target_mib: u32,
        actual_mib: u32,
    ) -> Self {
        Self {
            target_pages,
            actual_pages,
            target_mib,
            actual_mib,
            swap_in: None,
            swap_out: None,
            major_faults: None,
            minor_faults: None,
            free_memory: None,
            total_memory: None,
            available_memory: None,
            disk_caches: None,
            hugetlb_allocations: None,
            hugetlb_failures: None,
            oom_kill: None,
            alloc_stall: None,
            async_scan: None,
            direct_scan: None,
            async_reclaim: None,
            direct_reclaim: None,
        }
    }

    pub const fn with_swap_in(mut self, value: Option<u64>) -> Self {
        self.swap_in = value;
        self
    }

    pub const fn with_swap_out(mut self, value: Option<u64>) -> Self {
        self.swap_out = value;
        self
    }

    pub const fn with_major_faults(mut self, value: Option<u64>) -> Self {
        self.major_faults = value;
        self
    }

    pub const fn with_minor_faults(mut self, value: Option<u64>) -> Self {
        self.minor_faults = value;
        self
    }

    pub const fn with_free_memory(mut self, value: Option<u64>) -> Self {
        self.free_memory = value;
        self
    }

    pub const fn with_total_memory(mut self, value: Option<u64>) -> Self {
        self.total_memory = value;
        self
    }

    pub const fn with_available_memory(mut self, value: Option<u64>) -> Self {
        self.available_memory = value;
        self
    }

    pub const fn with_disk_caches(mut self, value: Option<u64>) -> Self {
        self.disk_caches = value;
        self
    }

    pub const fn with_hugetlb_allocations(mut self, value: Option<u64>) -> Self {
        self.hugetlb_allocations = value;
        self
    }

    pub const fn with_hugetlb_failures(mut self, value: Option<u64>) -> Self {
        self.hugetlb_failures = value;
        self
    }

    pub const fn with_oom_kill(mut self, value: Option<u64>) -> Self {
        self.oom_kill = value;
        self
    }

    pub const fn with_alloc_stall(mut self, value: Option<u64>) -> Self {
        self.alloc_stall = value;
        self
    }

    pub const fn with_async_scan(mut self, value: Option<u64>) -> Self {
        self.async_scan = value;
        self
    }

    pub const fn with_direct_scan(mut self, value: Option<u64>) -> Self {
        self.direct_scan = value;
        self
    }

    pub const fn with_async_reclaim(mut self, value: Option<u64>) -> Self {
        self.async_reclaim = value;
        self
    }

    pub const fn with_direct_reclaim(mut self, value: Option<u64>) -> Self {
        self.direct_reclaim = value;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalloonHintingStatusResponse {
    host_cmd: u32,
    guest_cmd: Option<u32>,
}

impl BalloonHintingStatusResponse {
    pub const fn new(host_cmd: u32, guest_cmd: Option<u32>) -> Self {
        Self {
            host_cmd,
            guest_cmd,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalloonHintingStartRequest {
    acknowledge_on_stop: bool,
}

impl BalloonHintingStartRequest {
    pub const fn new(acknowledge_on_stop: bool) -> Self {
        Self {
            acknowledge_on_stop,
        }
    }

    pub const fn acknowledge_on_stop(self) -> bool {
        self.acknowledge_on_stop
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EntropyConfigResponse {
    rate_limiter: Option<EntropyRateLimiterRequest>,
}

impl EntropyConfigResponse {
    pub const fn new() -> Self {
        Self { rate_limiter: None }
    }

    pub const fn with_rate_limiter(mut self, rate_limiter: EntropyRateLimiterRequest) -> Self {
        self.rate_limiter = Some(rate_limiter);
        self
    }

    pub const fn rate_limiter(self) -> Option<EntropyRateLimiterRequest> {
        self.rate_limiter
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmdsConfigResponse {
    network_interfaces: Vec<String>,
    version: String,
    ipv4_address: Option<String>,
    imds_compat: bool,
}

impl MmdsConfigResponse {
    pub fn new(
        network_interfaces: impl Into<Vec<String>>,
        version: impl Into<String>,
        imds_compat: bool,
    ) -> Self {
        Self {
            network_interfaces: network_interfaces.into(),
            version: version.into(),
            ipv4_address: None,
            imds_compat,
        }
    }

    pub fn with_ipv4_address(mut self, ipv4_address: impl Into<String>) -> Self {
        self.ipv4_address = Some(ipv4_address.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrivePatchRequest {
    path_drive_id: String,
    body_drive_id: String,
    path_on_host: Option<String>,
    rate_limiter: Option<DriveRateLimiterRequest>,
}

impl DrivePatchRequest {
    pub fn path_drive_id(&self) -> &str {
        &self.path_drive_id
    }

    pub fn body_drive_id(&self) -> &str {
        &self.body_drive_id
    }

    pub fn path_on_host(&self) -> Option<&str> {
        self.path_on_host.as_deref()
    }

    pub const fn rate_limiter(&self) -> Option<DriveRateLimiterRequest> {
        self.rate_limiter
    }

    pub const fn rate_limiter_configured(&self) -> bool {
        self.rate_limiter.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VsockConfigResponse {
    guest_cid: u32,
    uds_path: String,
}

impl VsockConfigResponse {
    pub fn new(guest_cid: u32, uds_path: impl Into<String>) -> Self {
        Self {
            guest_cid,
            uds_path: uds_path.into(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DriveConfigRequestBody {
    drive_id: String,
    #[serde(default)]
    path_on_host: Option<String>,
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
    rate_limiter: Option<JsonValueWithoutDuplicateObjectKeys>,
    #[serde(default)]
    socket: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DrivePatchRequestBody {
    drive_id: String,
    #[serde(default)]
    path_on_host: Option<String>,
    #[serde(default)]
    rate_limiter: Option<JsonValueWithoutDuplicateObjectKeys>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BalloonConfigRequestBody {
    amount_mib: u32,
    deflate_on_oom: bool,
    #[serde(default)]
    stats_polling_interval_s: u16,
    #[serde(default)]
    free_page_hinting: bool,
    #[serde(default)]
    free_page_reporting: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BalloonUpdateRequestBody {
    amount_mib: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BalloonStatsUpdateRequestBody {
    stats_polling_interval_s: u16,
}

#[derive(Debug, Deserialize)]
struct BalloonHintingStartRequestBody {
    #[serde(default = "default_balloon_acknowledge_on_stop")]
    acknowledge_on_stop: bool,
}

const fn default_balloon_acknowledge_on_stop() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum SnapshotType {
    Full,
    Diff,
}

const fn default_snapshot_type() -> SnapshotType {
    SnapshotType::Full
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotCreateRequestBody {
    #[serde(default = "default_snapshot_type")]
    snapshot_type: SnapshotType,
    snapshot_path: String,
    mem_file_path: String,
}

#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotCreateRequest {
    snapshot_type: SnapshotType,
    snapshot_path: String,
    mem_file_path: String,
}

impl SnapshotCreateRequest {
    fn new(snapshot_type: SnapshotType, snapshot_path: String, mem_file_path: String) -> Self {
        Self {
            snapshot_type,
            snapshot_path,
            mem_file_path,
        }
    }

    pub const fn snapshot_type(&self) -> SnapshotType {
        self.snapshot_type
    }

    pub fn snapshot_path(&self) -> &str {
        &self.snapshot_path
    }

    pub fn mem_file_path(&self) -> &str {
        &self.mem_file_path
    }
}

impl fmt::Debug for SnapshotCreateRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotCreateRequest")
            .field("snapshot_type", &self.snapshot_type)
            .field("snapshot_path", &SNAPSHOT_VALUE_REDACTED)
            .field("mem_file_path", &SNAPSHOT_VALUE_REDACTED)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum SnapshotMemoryBackendType {
    File,
    Uffd,
}

#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotMemoryBackend {
    backend_path: String,
    backend_type: SnapshotMemoryBackendType,
}

impl SnapshotMemoryBackend {
    fn new(backend_path: String, backend_type: SnapshotMemoryBackendType) -> Self {
        Self {
            backend_path,
            backend_type,
        }
    }

    pub fn backend_path(&self) -> &str {
        &self.backend_path
    }

    pub const fn backend_type(&self) -> SnapshotMemoryBackendType {
        self.backend_type
    }
}

impl fmt::Debug for SnapshotMemoryBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotMemoryBackend")
            .field("backend_path", &SNAPSHOT_VALUE_REDACTED)
            .field("backend_type", &self.backend_type)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotNetworkOverride {
    iface_id: String,
    host_dev_name: String,
}

impl SnapshotNetworkOverride {
    fn new(iface_id: String, host_dev_name: String) -> Self {
        Self {
            iface_id,
            host_dev_name,
        }
    }

    pub fn iface_id(&self) -> &str {
        &self.iface_id
    }

    pub fn host_dev_name(&self) -> &str {
        &self.host_dev_name
    }
}

impl fmt::Debug for SnapshotNetworkOverride {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotNetworkOverride")
            .field("iface_id", &SNAPSHOT_VALUE_REDACTED)
            .field("host_dev_name", &SNAPSHOT_VALUE_REDACTED)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotVsockOverride {
    uds_path: String,
}

impl SnapshotVsockOverride {
    fn new(uds_path: String) -> Self {
        Self { uds_path }
    }

    pub fn uds_path(&self) -> &str {
        &self.uds_path
    }
}

impl fmt::Debug for SnapshotVsockOverride {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotVsockOverride")
            .field("uds_path", &SNAPSHOT_VALUE_REDACTED)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotLoadRequest {
    snapshot_path: String,
    mem_backend: SnapshotMemoryBackend,
    track_dirty_pages: bool,
    resume_vm: bool,
    network_overrides: Vec<SnapshotNetworkOverride>,
    vsock_override: Option<SnapshotVsockOverride>,
    clock_realtime: bool,
    deprecated_fields_used: bool,
}

impl SnapshotLoadRequest {
    pub fn snapshot_path(&self) -> &str {
        &self.snapshot_path
    }

    pub const fn mem_backend(&self) -> &SnapshotMemoryBackend {
        &self.mem_backend
    }

    pub const fn track_dirty_pages(&self) -> bool {
        self.track_dirty_pages
    }

    pub const fn resume_vm(&self) -> bool {
        self.resume_vm
    }

    pub fn network_overrides(&self) -> &[SnapshotNetworkOverride] {
        &self.network_overrides
    }

    pub const fn vsock_override(&self) -> Option<&SnapshotVsockOverride> {
        self.vsock_override.as_ref()
    }

    pub const fn clock_realtime(&self) -> bool {
        self.clock_realtime
    }

    pub const fn deprecated_fields_used(&self) -> bool {
        self.deprecated_fields_used
    }
}

impl fmt::Debug for SnapshotLoadRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotLoadRequest")
            .field("snapshot_path", &SNAPSHOT_VALUE_REDACTED)
            .field("mem_backend", &self.mem_backend)
            .field("track_dirty_pages", &self.track_dirty_pages)
            .field("resume_vm", &self.resume_vm)
            .field("network_overrides", &SNAPSHOT_VALUE_REDACTED)
            .field(
                "vsock_override",
                &self
                    .vsock_override
                    .as_ref()
                    .map(|_| SNAPSHOT_VALUE_REDACTED),
            )
            .field("clock_realtime", &self.clock_realtime)
            .field("deprecated_fields_used", &self.deprecated_fields_used)
            .finish()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotMemBackendRequestBody {
    backend_path: String,
    backend_type: SnapshotMemoryBackendType,
}

#[derive(Debug, Deserialize)]
struct SnapshotNetworkOverrideRequestBody {
    iface_id: String,
    host_dev_name: String,
}

#[derive(Debug, Deserialize)]
struct SnapshotVsockOverrideRequestBody {
    uds_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotLoadRequestBody {
    snapshot_path: String,
    #[serde(default)]
    mem_file_path: Option<String>,
    #[serde(default)]
    mem_backend: Option<SnapshotMemBackendRequestBody>,
    #[serde(default)]
    enable_diff_snapshots: bool,
    #[serde(default)]
    track_dirty_pages: bool,
    #[serde(default)]
    resume_vm: bool,
    #[serde(default)]
    network_overrides: Vec<SnapshotNetworkOverrideRequestBody>,
    #[serde(default)]
    vsock_override: Option<SnapshotVsockOverrideRequestBody>,
    #[serde(default)]
    clock_realtime: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PmemConfigRequestBody {
    id: String,
    path_on_host: String,
    #[serde(default)]
    root_device: bool,
    #[serde(default)]
    read_only: bool,
    #[serde(default)]
    rate_limiter: Option<JsonValueWithoutDuplicateObjectKeys>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PmemPatchRequestBody {
    id: String,
    #[serde(default)]
    rate_limiter: Option<JsonValueWithoutDuplicateObjectKeys>,
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
    rx_rate_limiter: Option<JsonValueWithoutDuplicateObjectKeys>,
    #[serde(default)]
    tx_rate_limiter: Option<JsonValueWithoutDuplicateObjectKeys>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NetworkInterfacePatchRequestBody {
    iface_id: String,
    #[serde(default)]
    rx_rate_limiter: Option<JsonValueWithoutDuplicateObjectKeys>,
    #[serde(default)]
    tx_rate_limiter: Option<JsonValueWithoutDuplicateObjectKeys>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VsockConfigRequestBody {
    #[serde(default)]
    vsock_id: Option<String>,
    guest_cid: u32,
    uds_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusCode {
    Ok,
    NoContent,
    BadRequest,
    PayloadTooLarge,
}

impl StatusCode {
    pub const fn as_u16(self) -> u16 {
        match self {
            Self::Ok => 200,
            Self::NoContent => 204,
            Self::BadRequest => 400,
            Self::PayloadTooLarge => 413,
        }
    }

    const fn reason_phrase(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::NoContent => "No Content",
            Self::BadRequest => "Bad Request",
            Self::PayloadTooLarge => "Payload Too Large",
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
        if let Some(balloon) = &config.balloon {
            body.insert(
                "balloon".to_string(),
                balloon_config_response_value(balloon),
            );
        }
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
        if let Some(entropy) = &config.entropy {
            body.insert(
                "entropy".to_string(),
                entropy_config_response_value(entropy),
            );
        }
        if let Some(memory_hotplug) = &config.memory_hotplug {
            body.insert(
                "memory-hotplug".to_string(),
                memory_hotplug_config_response_value(memory_hotplug),
            );
        }
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
        body.insert(
            "pmem".to_string(),
            serde_json::Value::Array(config.pmem.iter().map(pmem_config_response_value).collect()),
        );
        if let Some(mmds_config) = &config.mmds_config {
            body.insert(
                "mmds-config".to_string(),
                mmds_config_response_value(mmds_config),
            );
        }
        if let Some(vsock) = &config.vsock {
            body.insert("vsock".to_string(), vsock_config_response_value(vsock));
        }

        Self {
            status: StatusCode::Ok,
            body: serde_json::Value::Object(body).to_string(),
        }
    }

    pub fn mmds(value: &serde_json::Value) -> Self {
        Self {
            status: StatusCode::Ok,
            body: value.to_string(),
        }
    }

    pub fn balloon_config(config: BalloonConfigResponse) -> Self {
        Self {
            status: StatusCode::Ok,
            body: balloon_config_response_value(&config).to_string(),
        }
    }

    pub fn balloon_stats(stats: BalloonStatsResponse) -> Self {
        Self {
            status: StatusCode::Ok,
            body: balloon_stats_response_value(&stats).to_string(),
        }
    }

    pub fn balloon_hinting_status(status: BalloonHintingStatusResponse) -> Self {
        Self {
            status: StatusCode::Ok,
            body: balloon_hinting_status_response_value(&status).to_string(),
        }
    }

    pub fn memory_hotplug_status(status: MemoryHotplugStatusResponse) -> Self {
        Self {
            status: StatusCode::Ok,
            body: memory_hotplug_status_response_value(&status).to_string(),
        }
    }

    pub fn fault(message: &str) -> Self {
        let body = serde_json::json!({ "fault_message": message }).to_string();

        Self {
            status: StatusCode::BadRequest,
            body,
        }
    }

    pub fn payload_too_large_fault() -> Self {
        let body = serde_json::json!({
            "fault_message": RequestError::PayloadTooLarge.fault_message()
        })
        .to_string();

        Self {
            status: StatusCode::PayloadTooLarge,
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
    if let Some(rate_limiter) = drive.rate_limiter() {
        body.insert(
            "rate_limiter".to_string(),
            drive_rate_limiter_response_value(rate_limiter),
        );
    }

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
    if let Some(mtu) = network_interface.mtu {
        body.insert(
            "mtu".to_string(),
            serde_json::Value::Number(serde_json::Number::from(mtu)),
        );
    }
    if let Some(rate_limiter) = network_interface.rx_rate_limiter {
        body.insert(
            "rx_rate_limiter".to_string(),
            network_rate_limiter_response_value(rate_limiter),
        );
    }
    if let Some(rate_limiter) = network_interface.tx_rate_limiter {
        body.insert(
            "tx_rate_limiter".to_string(),
            network_rate_limiter_response_value(rate_limiter),
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

fn pmem_config_response_value(pmem: &PmemConfigResponse) -> serde_json::Value {
    serde_json::json!({
        "id": pmem.id.clone(),
        "path_on_host": pmem.path_on_host.clone(),
        "read_only": pmem.read_only,
        "root_device": pmem.root_device,
    })
}

fn memory_hotplug_config_response_value(config: &MemoryHotplugConfigResponse) -> serde_json::Value {
    serde_json::json!({
        "block_size_mib": config.block_size_mib,
        "slot_size_mib": config.slot_size_mib,
        "total_size_mib": config.total_size_mib,
    })
}

fn entropy_config_response_value(entropy: &EntropyConfigResponse) -> serde_json::Value {
    let mut value = serde_json::Map::new();
    if let Some(rate_limiter) = entropy.rate_limiter() {
        value.insert(
            "rate_limiter".to_string(),
            entropy_rate_limiter_response_value(rate_limiter),
        );
    }
    serde_json::Value::Object(value)
}

fn entropy_rate_limiter_response_value(
    rate_limiter: EntropyRateLimiterRequest,
) -> serde_json::Value {
    rate_limiter_response_value(rate_limiter.bandwidth(), rate_limiter.ops())
}

fn drive_rate_limiter_response_value(rate_limiter: DriveRateLimiterRequest) -> serde_json::Value {
    rate_limiter_response_value(rate_limiter.bandwidth(), rate_limiter.ops())
}

fn network_rate_limiter_response_value(
    rate_limiter: NetworkRateLimiterRequest,
) -> serde_json::Value {
    rate_limiter_response_value(rate_limiter.bandwidth(), rate_limiter.ops())
}

fn rate_limiter_response_value(
    bandwidth: Option<TokenBucketRequest>,
    ops: Option<TokenBucketRequest>,
) -> serde_json::Value {
    let mut value = serde_json::Map::new();
    if let Some(bandwidth) = bandwidth {
        value.insert(
            RATE_LIMITER_BANDWIDTH_FIELD.to_string(),
            token_bucket_response_value(bandwidth),
        );
    }
    if let Some(ops) = ops {
        value.insert(
            RATE_LIMITER_OPS_FIELD.to_string(),
            token_bucket_response_value(ops),
        );
    }
    serde_json::Value::Object(value)
}

fn token_bucket_response_value(bucket: TokenBucketRequest) -> serde_json::Value {
    let mut value = serde_json::Map::new();
    value.insert(
        TOKEN_BUCKET_SIZE_FIELD.to_string(),
        serde_json::Value::Number(bucket.size().into()),
    );
    value.insert(
        TOKEN_BUCKET_ONE_TIME_BURST_FIELD.to_string(),
        bucket
            .one_time_burst()
            .map_or(serde_json::Value::Null, |burst| {
                serde_json::Value::Number(burst.into())
            }),
    );
    value.insert(
        TOKEN_BUCKET_REFILL_TIME_FIELD.to_string(),
        serde_json::Value::Number(bucket.refill_time().into()),
    );
    serde_json::Value::Object(value)
}

fn balloon_config_response_value(balloon: &BalloonConfigResponse) -> serde_json::Value {
    serde_json::json!({
        "amount_mib": balloon.amount_mib,
        "deflate_on_oom": balloon.deflate_on_oom,
        "free_page_hinting": balloon.free_page_hinting,
        "free_page_reporting": balloon.free_page_reporting,
        "stats_polling_interval_s": balloon.stats_polling_interval_s,
    })
}

fn balloon_stats_response_value(stats: &BalloonStatsResponse) -> serde_json::Value {
    let mut body = serde_json::Map::new();
    body.insert("actual_mib".to_string(), stats.actual_mib.into());
    body.insert("actual_pages".to_string(), stats.actual_pages.into());
    body.insert("target_mib".to_string(), stats.target_mib.into());
    body.insert("target_pages".to_string(), stats.target_pages.into());
    insert_optional_u64(&mut body, "swap_in", stats.swap_in);
    insert_optional_u64(&mut body, "swap_out", stats.swap_out);
    insert_optional_u64(&mut body, "major_faults", stats.major_faults);
    insert_optional_u64(&mut body, "minor_faults", stats.minor_faults);
    insert_optional_u64(&mut body, "free_memory", stats.free_memory);
    insert_optional_u64(&mut body, "total_memory", stats.total_memory);
    insert_optional_u64(&mut body, "available_memory", stats.available_memory);
    insert_optional_u64(&mut body, "disk_caches", stats.disk_caches);
    insert_optional_u64(&mut body, "hugetlb_allocations", stats.hugetlb_allocations);
    insert_optional_u64(&mut body, "hugetlb_failures", stats.hugetlb_failures);
    insert_optional_u64(&mut body, "oom_kill", stats.oom_kill);
    insert_optional_u64(&mut body, "alloc_stall", stats.alloc_stall);
    insert_optional_u64(&mut body, "async_scan", stats.async_scan);
    insert_optional_u64(&mut body, "direct_scan", stats.direct_scan);
    insert_optional_u64(&mut body, "async_reclaim", stats.async_reclaim);
    insert_optional_u64(&mut body, "direct_reclaim", stats.direct_reclaim);

    serde_json::Value::Object(body)
}

fn memory_hotplug_status_response_value(status: &MemoryHotplugStatusResponse) -> serde_json::Value {
    serde_json::json!({
        "block_size_mib": status.block_size_mib,
        "plugged_size_mib": status.plugged_size_mib,
        "requested_size_mib": status.requested_size_mib,
        "slot_size_mib": status.slot_size_mib,
        "total_size_mib": status.total_size_mib,
    })
}

fn insert_optional_u64(
    body: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: Option<u64>,
) {
    if let Some(value) = value {
        body.insert(key.to_string(), value.into());
    }
}

fn balloon_hinting_status_response_value(
    status: &BalloonHintingStatusResponse,
) -> serde_json::Value {
    serde_json::json!({
        "guest_cmd": status.guest_cmd,
        "host_cmd": status.host_cmd,
    })
}

fn mmds_config_response_value(config: &MmdsConfigResponse) -> serde_json::Value {
    let mut body = serde_json::Map::new();
    body.insert(
        "imds_compat".to_string(),
        serde_json::Value::Bool(config.imds_compat),
    );
    if let Some(ipv4_address) = &config.ipv4_address {
        body.insert(
            "ipv4_address".to_string(),
            serde_json::Value::String(ipv4_address.clone()),
        );
    }
    body.insert(
        "network_interfaces".to_string(),
        serde_json::Value::Array(
            config
                .network_interfaces
                .iter()
                .map(|iface_id| serde_json::Value::String(iface_id.clone()))
                .collect(),
        ),
    );
    body.insert(
        "version".to_string(),
        serde_json::Value::String(config.version.clone()),
    );

    serde_json::Value::Object(body)
}

fn vsock_config_response_value(vsock: &VsockConfigResponse) -> serde_json::Value {
    let mut body = serde_json::Map::new();
    body.insert(
        "guest_cid".to_string(),
        serde_json::Value::Number(serde_json::Number::from(vsock.guest_cid)),
    );
    body.insert(
        "uds_path".to_string(),
        serde_json::Value::String(vsock.uds_path.clone()),
    );
    serde_json::Value::Object(body)
}

pub fn parse_request(bytes: &[u8]) -> Result<ApiRequest, RequestError> {
    parse_request_with_limit(bytes, HTTP_MAX_PAYLOAD_SIZE)
}

pub fn parse_request_with_limit(
    bytes: &[u8],
    max_payload_size: usize,
) -> Result<ApiRequest, RequestError> {
    let (method, path, header_len, request_body) = parse_request_head(bytes)?;
    checked_request_head_len(header_len)?;
    let body = bytes
        .get(header_len..)
        .ok_or(RequestError::MalformedRequest)?;

    if request_body.has_unsupported_encoding() {
        return Err(RequestError::MalformedRequest);
    }

    checked_payload_len(request_body.content_length(), max_payload_size)?;

    if body.len() != request_body.content_length() {
        return Err(RequestError::MalformedRequest);
    }

    if method == "GET" && request_body.has_content() {
        return Err(RequestError::GetRequestBody);
    }
    if method == "DELETE" && request_body.has_content() {
        return Err(RequestError::EmptyDeleteRequest);
    }
    if method == "PUT" && body.is_empty() {
        return Err(RequestError::EmptyPutRequest);
    }
    if method == "PATCH" && body.is_empty() && !patch_request_path_accepts_empty_body(path) {
        return Err(RequestError::EmptyPatchRequest);
    }

    if let Some(request) = balloon_request_without_body_parsing(method, path) {
        return Ok(request);
    }
    if method == "PUT" && path == "/balloon" {
        return parse_balloon_config_request(body);
    }
    if method == "PATCH" && path == "/balloon" {
        return parse_balloon_update_request(body);
    }
    if method == "PATCH" && path == "/balloon/statistics" {
        return parse_balloon_stats_update_request(body);
    }
    if method == "PATCH" && path == "/balloon/hinting/start" {
        return parse_balloon_hinting_start_request(body);
    }

    if method == "GET" && path == "/hotplug/memory" {
        return Ok(ApiRequest::GetMemoryHotplug);
    }
    if method == "PUT" && path == "/hotplug/memory" {
        return parse_memory_hotplug_config_request(body);
    }
    if method == "PATCH" && path == "/hotplug/memory" {
        return parse_memory_hotplug_size_update_request(body);
    }

    if method == "PUT"
        && let Some(path_pmem_id) = pmem_path_id(path)
    {
        return parse_pmem_config_request(path_pmem_id, body);
    }
    if method == "PATCH"
        && let Some(path_pmem_id) = pmem_path_id(path)
    {
        return parse_pmem_patch_request(path_pmem_id, body);
    }
    if method == "DELETE"
        && let Some(path_pmem_id) = pmem_path_id(path)
    {
        return Ok(ApiRequest::HotUnplugDevice(Box::new(
            HotUnplugDeviceRequest::new(HotUnplugDeviceKind::Pmem, path_pmem_id),
        )));
    }

    if method == "PATCH"
        && let Some(path_drive_id) = drive_path_id(path)
    {
        return parse_drive_patch_request(path_drive_id, body);
    }
    if method == "DELETE"
        && let Some(path_drive_id) = drive_path_id(path)
    {
        return Ok(ApiRequest::HotUnplugDevice(Box::new(
            HotUnplugDeviceRequest::new(HotUnplugDeviceKind::Drive, path_drive_id),
        )));
    }
    if method == "PATCH"
        && let Some(path_iface_id) = network_interface_path_id(path)
    {
        return parse_network_interface_patch_request(path_iface_id, body);
    }
    if method == "DELETE"
        && let Some(path_iface_id) = network_interface_path_id(path)
    {
        return Ok(ApiRequest::HotUnplugDevice(Box::new(
            HotUnplugDeviceRequest::new(HotUnplugDeviceKind::NetworkInterface, path_iface_id),
        )));
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
    if method == "PUT" && path == "/cpu-config" {
        return parse_cpu_config_request(body);
    }
    if method == "PUT" && path == "/entropy" {
        return parse_entropy_config_request(body);
    }
    if method == "PUT" && path == "/serial" {
        return parse_serial_config_request(body);
    }
    if method == "PUT" && path == "/logger" {
        return parse_logger_config_request(body);
    }
    if method == "PUT" && path == "/machine-config" {
        return parse_machine_config_request(body);
    }
    if method == "PATCH" && path == "/machine-config" {
        return parse_machine_config_patch_request(body);
    }
    if method == "PATCH" && path == "/vm" {
        return parse_vm_state_update_request(body);
    }
    if method == "PUT" && path == "/metrics" {
        return parse_metrics_config_request(body);
    }
    if method == "PUT" && path == "/mmds" {
        return parse_put_mmds_request(body);
    }
    if method == "PATCH" && path == "/mmds" {
        return parse_patch_mmds_request(body);
    }
    if method == "PUT" && path == "/mmds/config" {
        return parse_mmds_config_request(body);
    }
    if method == "PUT" && path == "/vsock" {
        return parse_vsock_config_request(body);
    }
    if method == "PUT" && path == "/snapshot/create" {
        return parse_snapshot_create_request(body);
    }
    if method == "PUT" && path == "/snapshot/load" {
        return parse_snapshot_load_request(body);
    }

    match (method, path) {
        ("GET", "/") => Ok(ApiRequest::GetInstanceInfo),
        ("GET", "/machine-config") => Ok(ApiRequest::GetMachineConfig),
        ("GET", "/mmds") => Ok(ApiRequest::GetMmds),
        ("GET", "/vm/config") => Ok(ApiRequest::GetVmConfig),
        ("GET", "/version") => Ok(ApiRequest::GetVersion),
        _ => Err(RequestError::InvalidPathMethod),
    }
}

fn balloon_request_without_body_parsing(method: &str, path: &str) -> Option<ApiRequest> {
    match (method, path) {
        ("GET", "/balloon") => Some(ApiRequest::GetBalloon),
        ("GET", "/balloon/statistics") => Some(ApiRequest::GetBalloonStats),
        ("GET", "/balloon/hinting/status") => Some(ApiRequest::GetBalloonHintingStatus),
        ("PATCH", "/balloon/hinting/stop") => Some(ApiRequest::PatchBalloonHintingStop),
        _ => None,
    }
}

fn patch_request_path_accepts_empty_body(path: &str) -> bool {
    matches!(path, "/balloon/hinting/start" | "/balloon/hinting/stop")
}

fn drive_path_id(path: &str) -> Option<&str> {
    single_segment_id(path.strip_prefix("/drives/")?)
}

fn network_interface_path_id(path: &str) -> Option<&str> {
    single_segment_id(path.strip_prefix("/network-interfaces/")?)
}

fn pmem_path_id(path: &str) -> Option<&str> {
    single_segment_id(path.strip_prefix("/pmem/")?)
}

fn single_segment_id(rest: &str) -> Option<&str> {
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

fn parse_vm_state_update_request(body: &[u8]) -> Result<ApiRequest, RequestError> {
    let body = serde_json::from_slice::<VmStateUpdateRequestBody>(body)
        .map_err(|_| RequestError::MalformedRequest)?;

    let state = match body.state {
        VmStateUpdateBody::Paused => VmStateUpdate::Paused,
        VmStateUpdateBody::Resumed => VmStateUpdate::Resumed,
    };

    Ok(ApiRequest::PatchVmState(Box::new(VmStateUpdateRequest {
        state,
    })))
}

fn parse_cpu_config_request(body: &[u8]) -> Result<ApiRequest, RequestError> {
    let value = serde_json::from_slice::<serde_json::Value>(body)
        .map_err(|_| RequestError::MalformedRequest)?;
    validate_cpu_config_json_shape(&value).map_err(|()| RequestError::MalformedRequest)?;

    let body = serde_json::from_slice::<CpuConfigRequestBody>(body)
        .map_err(|_| RequestError::MalformedRequest)?;
    body.validate()
        .map_err(|()| RequestError::MalformedRequest)?;

    Ok(ApiRequest::PutCpuConfig(Box::new(CpuConfigRequest {
        category: body.category(),
    })))
}

fn validate_cpu_config_json_shape(value: &serde_json::Value) -> Result<(), ()> {
    let object = value.as_object().ok_or(())?;

    for (key, value) in object {
        match key.as_str() {
            "kvm_capabilities" => {
                value.as_array().ok_or(())?;
            }
            "reg_modifiers" => validate_cpu_config_json_object_array(value, &["addr", "bitmap"])?,
            "vcpu_features" => {
                validate_cpu_config_json_object_array(value, &["index", "bitmap"])?;
            }
            _ => return Err(()),
        }
    }

    Ok(())
}

fn validate_cpu_config_json_object_array(
    value: &serde_json::Value,
    fields: &[&str],
) -> Result<(), ()> {
    let values = value.as_array().ok_or(())?;

    for value in values {
        validate_cpu_config_json_object(value, fields)?;
    }

    Ok(())
}

fn validate_cpu_config_json_object(value: &serde_json::Value, fields: &[&str]) -> Result<(), ()> {
    let object = value.as_object().ok_or(())?;

    for field in fields {
        if !object.contains_key(*field) {
            return Err(());
        }
    }
    for key in object.keys() {
        if !fields.contains(&key.as_str()) {
            return Err(());
        }
    }

    Ok(())
}

fn validate_prefixed_u64(value: &str) -> Result<u64, ()> {
    parse_prefixed_integer(value, u64::from_str_radix)
}

fn parse_prefixed_integer<T>(
    value: &str,
    parse: impl Fn(&str, u32) -> Result<T, std::num::ParseIntError>,
) -> Result<T, ()> {
    let (digits, radix) = if let Some(binary) = value.strip_prefix("0b") {
        (binary, 2)
    } else if let Some(hex) = value.strip_prefix("0x") {
        (hex, 16)
    } else {
        return Err(());
    };

    if digits.is_empty() {
        return Err(());
    }

    parse(digits, radix).map_err(|_| ())
}

fn validate_arm64_register_bits(register_id: u64) -> Result<u32, ()> {
    match (register_id & ARM64_KVM_REG_SIZE_MASK) >> ARM64_KVM_REG_SIZE_SHIFT {
        2 => Ok(32),
        3 => Ok(64),
        4 => Ok(128),
        _ => Err(()),
    }
}

fn register_bitmap_limit(register_bits: u32) -> Option<u128> {
    if register_bits == u128::BITS {
        None
    } else {
        Some((1_u128 << register_bits) - 1)
    }
}

#[derive(Debug, Clone, Copy)]
struct CpuTemplateBitmap {
    filter: u128,
    value: u128,
}

fn validate_bitmap(bitmap: &str, max_bits: u32) -> Result<CpuTemplateBitmap, ()> {
    let bitmap = bitmap.strip_prefix("0b").unwrap_or(bitmap);
    let mut bit_count = 0;
    let mut filter = 0;
    let mut value = 0;

    for byte in bitmap.bytes().rev() {
        if bit_count == max_bits {
            return Err(());
        }

        match byte {
            b'_' => {}
            b'x' => {
                bit_count += 1;
            }
            b'0' => {
                filter |= 1_u128 << bit_count;
                bit_count += 1;
            }
            b'1' => {
                filter |= 1_u128 << bit_count;
                value |= 1_u128 << bit_count;
                bit_count += 1;
            }
            _ => return Err(()),
        }
    }

    Ok(CpuTemplateBitmap { filter, value })
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

fn parse_serial_config_request(body: &[u8]) -> Result<ApiRequest, RequestError> {
    let body = serde_json::from_slice::<SerialConfigRequestBody>(body)
        .map_err(|_| RequestError::MalformedRequest)?;
    let rate_limiter = parse_serial_token_bucket(body.rate_limiter.as_ref())?;

    Ok(ApiRequest::PutSerial(Box::new(SerialConfigRequest {
        serial_out_path: body.serial_out_path,
        rate_limiter,
    })))
}

fn parse_entropy_config_request(body: &[u8]) -> Result<ApiRequest, RequestError> {
    let body = serde_json::from_slice::<EntropyDeviceConfigRequestBody>(body)
        .map_err(|_| RequestError::MalformedRequest)?;
    let rate_limiter = parse_entropy_rate_limiter(body.rate_limiter.as_ref())?;

    Ok(ApiRequest::PutEntropy(Box::new(EntropyConfigRequest {
        rate_limiter,
    })))
}

fn parse_balloon_config_request(body: &[u8]) -> Result<ApiRequest, RequestError> {
    let BalloonConfigRequestBody {
        amount_mib,
        deflate_on_oom,
        stats_polling_interval_s,
        free_page_hinting,
        free_page_reporting,
    } = serde_json::from_slice::<BalloonConfigRequestBody>(body)
        .map_err(|_| RequestError::MalformedRequest)?;

    Ok(ApiRequest::PutBalloon(Box::new(BalloonConfigRequest {
        amount_mib,
        deflate_on_oom,
        stats_polling_interval_s,
        free_page_hinting,
        free_page_reporting,
    })))
}

fn parse_balloon_update_request(body: &[u8]) -> Result<ApiRequest, RequestError> {
    let BalloonUpdateRequestBody { amount_mib } =
        serde_json::from_slice::<BalloonUpdateRequestBody>(body)
            .map_err(|_| RequestError::MalformedRequest)?;

    Ok(ApiRequest::PatchBalloon(Box::new(BalloonUpdateRequest {
        amount_mib,
    })))
}

fn parse_balloon_stats_update_request(body: &[u8]) -> Result<ApiRequest, RequestError> {
    let BalloonStatsUpdateRequestBody {
        stats_polling_interval_s,
    } = serde_json::from_slice::<BalloonStatsUpdateRequestBody>(body)
        .map_err(|_| RequestError::MalformedRequest)?;

    Ok(ApiRequest::PatchBalloonStats(Box::new(
        BalloonStatsUpdateRequest {
            stats_polling_interval_s,
        },
    )))
}

fn parse_balloon_hinting_start_request(body: &[u8]) -> Result<ApiRequest, RequestError> {
    if body.is_empty() {
        return Ok(ApiRequest::PatchBalloonHintingStart(
            BalloonHintingStartRequest::new(default_balloon_acknowledge_on_stop()),
        ));
    }

    let BalloonHintingStartRequestBody {
        acknowledge_on_stop,
    } = serde_json::from_slice::<BalloonHintingStartRequestBody>(body)
        .map_err(|_| RequestError::MalformedRequest)?;

    Ok(ApiRequest::PatchBalloonHintingStart(
        BalloonHintingStartRequest::new(acknowledge_on_stop),
    ))
}

fn parse_snapshot_create_request(body: &[u8]) -> Result<ApiRequest, RequestError> {
    let SnapshotCreateRequestBody {
        snapshot_type,
        snapshot_path,
        mem_file_path,
    } = serde_json::from_slice::<SnapshotCreateRequestBody>(body)
        .map_err(|_| RequestError::MalformedRequest)?;

    Ok(ApiRequest::PutSnapshotCreate(SnapshotCreateRequest::new(
        snapshot_type,
        snapshot_path,
        mem_file_path,
    )))
}

fn parse_snapshot_load_request(body: &[u8]) -> Result<ApiRequest, RequestError> {
    let SnapshotLoadRequestBody {
        snapshot_path,
        mem_file_path,
        mem_backend,
        enable_diff_snapshots,
        track_dirty_pages,
        resume_vm,
        network_overrides,
        vsock_override,
        clock_realtime,
    } = serde_json::from_slice::<SnapshotLoadRequestBody>(body)
        .map_err(|_| RequestError::MalformedRequest)?;

    if mem_file_path.is_some() == mem_backend.is_some() {
        return Err(RequestError::MalformedRequest);
    }

    let deprecated_fields_used = mem_file_path.is_some() || enable_diff_snapshots;
    let mem_backend = match mem_backend {
        Some(SnapshotMemBackendRequestBody {
            backend_path,
            backend_type,
        }) => SnapshotMemoryBackend::new(backend_path, backend_type),
        None => {
            let Some(mem_file_path) = mem_file_path else {
                return Err(RequestError::MalformedRequest);
            };
            SnapshotMemoryBackend::new(mem_file_path, SnapshotMemoryBackendType::File)
        }
    };
    let network_overrides = network_overrides
        .into_iter()
        .map(
            |SnapshotNetworkOverrideRequestBody {
                 iface_id,
                 host_dev_name,
             }| SnapshotNetworkOverride::new(iface_id, host_dev_name),
        )
        .collect();
    let vsock_override = vsock_override
        .map(|SnapshotVsockOverrideRequestBody { uds_path }| SnapshotVsockOverride::new(uds_path));

    Ok(ApiRequest::PutSnapshotLoad(SnapshotLoadRequest {
        snapshot_path,
        mem_backend,
        track_dirty_pages: enable_diff_snapshots || track_dirty_pages,
        resume_vm,
        network_overrides,
        vsock_override,
        clock_realtime,
        deprecated_fields_used,
    }))
}

fn parse_memory_hotplug_config_request(body: &[u8]) -> Result<ApiRequest, RequestError> {
    let MemoryHotplugConfigRequestBody {
        total_size_mib,
        block_size_mib,
        slot_size_mib,
    } = serde_json::from_slice::<MemoryHotplugConfigRequestBody>(body)
        .map_err(|_| RequestError::MalformedRequest)?;
    Ok(ApiRequest::PutMemoryHotplug(
        MemoryHotplugConfigRequest::new(total_size_mib, block_size_mib, slot_size_mib),
    ))
}

fn parse_memory_hotplug_size_update_request(body: &[u8]) -> Result<ApiRequest, RequestError> {
    let MemoryHotplugSizeUpdateRequestBody { requested_size_mib } =
        serde_json::from_slice::<MemoryHotplugSizeUpdateRequestBody>(body)
            .map_err(|_| RequestError::MalformedRequest)?;
    Ok(ApiRequest::PatchMemoryHotplug(
        MemoryHotplugSizeUpdateRequest::new(requested_size_mib),
    ))
}

fn parse_drive_config_request(
    path_drive_id: &str,
    body: &[u8],
) -> Result<ApiRequest, RequestError> {
    let body = serde_json::from_slice::<DriveConfigRequestBody>(body)
        .map_err(|_| RequestError::MalformedRequest)?;
    if body.path_on_host.is_none() && body.socket.is_none() {
        return Err(RequestError::MalformedRequest);
    }
    if path_drive_id != body.drive_id {
        return Err(RequestError::MismatchedDriveId);
    }
    let rate_limiter = parse_drive_rate_limiter(body.rate_limiter.as_ref())?;

    Ok(ApiRequest::PutDrive(Box::new(DriveConfigRequest {
        path_drive_id: path_drive_id.to_string(),
        body_drive_id: body.drive_id,
        path_on_host: body.path_on_host,
        is_root_device: body.is_root_device,
        is_read_only: body.is_read_only,
        partuuid: body.partuuid,
        cache_type: body.cache_type,
        io_engine: body.io_engine,
        rate_limiter,
        socket: body.socket,
    })))
}

fn parse_drive_patch_request(path_drive_id: &str, body: &[u8]) -> Result<ApiRequest, RequestError> {
    let body = serde_json::from_slice::<DrivePatchRequestBody>(body)
        .map_err(|_| RequestError::MalformedRequest)?;
    if path_drive_id != body.drive_id {
        return Err(RequestError::MismatchedDriveId);
    }
    let rate_limiter = parse_drive_rate_limiter(body.rate_limiter.as_ref())?;

    Ok(ApiRequest::PatchDrive(Box::new(DrivePatchRequest {
        path_drive_id: path_drive_id.to_string(),
        body_drive_id: body.drive_id,
        path_on_host: body.path_on_host,
        rate_limiter,
    })))
}

fn parse_pmem_config_request(path_pmem_id: &str, body: &[u8]) -> Result<ApiRequest, RequestError> {
    let body = serde_json::from_slice::<PmemConfigRequestBody>(body)
        .map_err(|_| RequestError::MalformedRequest)?;
    let rate_limiter_configured = match &body.rate_limiter {
        Some(rate_limiter) => {
            validate_rate_limiter_config(rate_limiter.as_value())?;
            rate_limiter_configured(rate_limiter.as_value())?
        }
        None => false,
    };
    if path_pmem_id != body.id {
        return Err(RequestError::MismatchedPmemId);
    }

    Ok(ApiRequest::PutPmem(Box::new(PmemConfigRequest {
        path_pmem_id: path_pmem_id.to_string(),
        body_pmem_id: body.id,
        path_on_host: body.path_on_host,
        root_device: body.root_device,
        read_only: body.read_only,
        rate_limiter_configured,
    })))
}

fn parse_pmem_patch_request(path_pmem_id: &str, body: &[u8]) -> Result<ApiRequest, RequestError> {
    let body = serde_json::from_slice::<PmemPatchRequestBody>(body)
        .map_err(|_| RequestError::MalformedRequest)?;
    let rate_limiter_configured = match &body.rate_limiter {
        Some(rate_limiter) => {
            validate_rate_limiter_config(rate_limiter.as_value())?;
            rate_limiter_configured(rate_limiter.as_value())?
        }
        None => false,
    };
    if path_pmem_id != body.id {
        return Err(RequestError::MismatchedPmemId);
    }

    Ok(ApiRequest::PatchPmem(Box::new(PmemPatchRequest {
        path_pmem_id: path_pmem_id.to_string(),
        body_pmem_id: body.id,
        rate_limiter_configured,
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
    let rx_rate_limiter = parse_network_rate_limiter(body.rx_rate_limiter.as_ref())?;
    let tx_rate_limiter = parse_network_rate_limiter(body.tx_rate_limiter.as_ref())?;

    Ok(ApiRequest::PutNetworkInterface(Box::new(
        NetworkInterfaceConfigRequest {
            path_iface_id: path_iface_id.to_string(),
            body_iface_id: body.iface_id,
            host_dev_name: body.host_dev_name,
            guest_mac: body.guest_mac,
            mtu: body.mtu,
            rx_rate_limiter,
            tx_rate_limiter,
        },
    )))
}

fn parse_network_interface_patch_request(
    path_iface_id: &str,
    body: &[u8],
) -> Result<ApiRequest, RequestError> {
    let body = serde_json::from_slice::<NetworkInterfacePatchRequestBody>(body)
        .map_err(|_| RequestError::MalformedRequest)?;
    if path_iface_id != body.iface_id {
        return Err(RequestError::MismatchedInterfaceId);
    }
    let rx_rate_limiter_configured = match &body.rx_rate_limiter {
        Some(rate_limiter) => {
            validate_rate_limiter_config(rate_limiter.as_value())?;
            rate_limiter_configured(rate_limiter.as_value())?
        }
        None => false,
    };
    let tx_rate_limiter_configured = match &body.tx_rate_limiter {
        Some(rate_limiter) => {
            validate_rate_limiter_config(rate_limiter.as_value())?;
            rate_limiter_configured(rate_limiter.as_value())?
        }
        None => false,
    };

    Ok(ApiRequest::PatchNetworkInterface(Box::new(
        NetworkInterfacePatchRequest {
            path_iface_id: path_iface_id.to_string(),
            body_iface_id: body.iface_id,
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

fn parse_machine_config_patch_request(body: &[u8]) -> Result<ApiRequest, RequestError> {
    let body = serde_json::from_slice::<MachineConfigPatchRequestBody>(body)
        .map_err(|_| RequestError::MalformedRequest)?;

    validate_machine_config_patch_request(&body)?;

    Ok(ApiRequest::PatchMachineConfig(Box::new(
        MachineConfigPatchRequest {
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

fn parse_put_mmds_request(body: &[u8]) -> Result<ApiRequest, RequestError> {
    Ok(ApiRequest::PutMmds(Box::new(parse_mmds_content_request(
        body,
    )?)))
}

fn parse_patch_mmds_request(body: &[u8]) -> Result<ApiRequest, RequestError> {
    Ok(ApiRequest::PatchMmds(Box::new(parse_mmds_content_request(
        body,
    )?)))
}

fn parse_mmds_content_request(body: &[u8]) -> Result<MmdsContentRequest, RequestError> {
    let value = serde_json::from_slice::<serde_json::Value>(body)
        .map_err(|_| RequestError::MalformedRequest)?;
    if !value.is_object() {
        return Err(RequestError::MalformedRequest);
    }

    Ok(MmdsContentRequest { value })
}

fn parse_mmds_config_request(body: &[u8]) -> Result<ApiRequest, RequestError> {
    let body = serde_json::from_slice::<MmdsConfigRequestBody>(body)
        .map_err(|_| RequestError::MalformedRequest)?;

    validate_mmds_config_request(&body)?;

    Ok(ApiRequest::PutMmdsConfig(Box::new(MmdsConfigRequest {
        network_interfaces: body.network_interfaces,
        version: body.version,
        ipv4_address: body.ipv4_address,
        imds_compat: body.imds_compat,
    })))
}

fn parse_vsock_config_request(body: &[u8]) -> Result<ApiRequest, RequestError> {
    let body = serde_json::from_slice::<VsockConfigRequestBody>(body)
        .map_err(|_| RequestError::MalformedRequest)?;

    Ok(ApiRequest::PutVsock(Box::new(VsockConfigRequest {
        vsock_id: body.vsock_id,
        guest_cid: body.guest_cid,
        uds_path: body.uds_path,
    })))
}

fn validate_machine_config_request(body: &MachineConfigRequestBody) -> Result<(), RequestError> {
    if body.vcpu_count == 0 || body.vcpu_count > MAX_MACHINE_CONFIG_VCPUS {
        return Err(RequestError::MalformedRequest);
    }
    if body.mem_size_mib == 0 {
        return Err(RequestError::MalformedRequest);
    }
    Ok(())
}

fn validate_machine_config_patch_request(
    body: &MachineConfigPatchRequestBody,
) -> Result<(), RequestError> {
    if body.is_empty() {
        return Err(RequestError::MalformedRequest);
    }
    if let Some(vcpu_count) = body.vcpu_count
        && (vcpu_count == 0 || vcpu_count > MAX_MACHINE_CONFIG_VCPUS)
    {
        return Err(RequestError::MalformedRequest);
    }
    if body.mem_size_mib == Some(0) {
        return Err(RequestError::MalformedRequest);
    }
    Ok(())
}

fn validate_mmds_config_request(body: &MmdsConfigRequestBody) -> Result<(), RequestError> {
    if body
        .network_interfaces
        .iter()
        .any(|iface_id| iface_id.trim().is_empty())
    {
        return Err(RequestError::MalformedRequest);
    }

    if let Some(ipv4_address) = body.ipv4_address
        && !is_valid_mmds_link_local_ipv4(ipv4_address)
    {
        return Err(RequestError::MalformedRequest);
    }

    Ok(())
}

fn is_valid_mmds_link_local_ipv4(ipv4_address: Ipv4Addr) -> bool {
    matches!(ipv4_address.octets(), [169, 254, 1..=254, _])
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

fn rate_limiter_configured(value: &serde_json::Value) -> Result<bool, RequestError> {
    let rate_limiter = value.as_object().ok_or(RequestError::MalformedRequest)?;

    Ok(rate_limiter
        .get(RATE_LIMITER_BANDWIDTH_FIELD)
        .is_some_and(|bucket| !bucket.is_null())
        || rate_limiter
            .get(RATE_LIMITER_OPS_FIELD)
            .is_some_and(|bucket| !bucket.is_null()))
}

fn parse_serial_token_bucket(
    value: Option<&JsonValueWithoutDuplicateObjectKeys>,
) -> Result<Option<TokenBucketRequest>, RequestError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let bucket = value
        .as_value()
        .as_object()
        .ok_or(RequestError::MalformedRequest)?;
    validate_token_bucket_object(bucket)?;

    Ok(Some(TokenBucketRequest::new(
        require_u64_field(bucket, TOKEN_BUCKET_SIZE_FIELD)?,
        optional_u64_field(bucket, TOKEN_BUCKET_ONE_TIME_BURST_FIELD)?,
        require_u64_field(bucket, TOKEN_BUCKET_REFILL_TIME_FIELD)?,
    )))
}

fn parse_entropy_rate_limiter(
    value: Option<&JsonValueWithoutDuplicateObjectKeys>,
) -> Result<Option<EntropyRateLimiterRequest>, RequestError> {
    parse_rate_limiter_buckets(value).map(|buckets| {
        buckets.map(|(bandwidth, ops)| EntropyRateLimiterRequest::new(bandwidth, ops))
    })
}

fn parse_drive_rate_limiter(
    value: Option<&JsonValueWithoutDuplicateObjectKeys>,
) -> Result<Option<DriveRateLimiterRequest>, RequestError> {
    parse_rate_limiter_buckets(value)
        .map(|buckets| buckets.map(|(bandwidth, ops)| DriveRateLimiterRequest::new(bandwidth, ops)))
}

fn parse_network_rate_limiter(
    value: Option<&JsonValueWithoutDuplicateObjectKeys>,
) -> Result<Option<NetworkRateLimiterRequest>, RequestError> {
    parse_rate_limiter_buckets(value).map(|buckets| {
        buckets.map(|(bandwidth, ops)| NetworkRateLimiterRequest::new(bandwidth, ops))
    })
}

fn parse_rate_limiter_buckets(
    value: Option<&JsonValueWithoutDuplicateObjectKeys>,
) -> Result<Option<RateLimiterBuckets>, RequestError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let rate_limiter = value
        .as_value()
        .as_object()
        .ok_or(RequestError::MalformedRequest)?;
    validate_rate_limiter_config(value.as_value())?;

    let bandwidth =
        parse_optional_rate_limiter_bucket(rate_limiter.get(RATE_LIMITER_BANDWIDTH_FIELD))?;
    let ops = parse_optional_rate_limiter_bucket(rate_limiter.get(RATE_LIMITER_OPS_FIELD))?;

    if bandwidth.is_none() && ops.is_none() {
        Ok(None)
    } else {
        Ok(Some((bandwidth, ops)))
    }
}

fn parse_optional_rate_limiter_bucket(
    value: Option<&serde_json::Value>,
) -> Result<Option<TokenBucketRequest>, RequestError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }

    let bucket = value.as_object().ok_or(RequestError::MalformedRequest)?;
    validate_token_bucket_object(bucket)?;

    Ok(Some(TokenBucketRequest::new(
        require_u64_field(bucket, TOKEN_BUCKET_SIZE_FIELD)?,
        optional_u64_field(bucket, TOKEN_BUCKET_ONE_TIME_BURST_FIELD)?,
        require_u64_field(bucket, TOKEN_BUCKET_REFILL_TIME_FIELD)?,
    )))
}

fn validate_token_bucket(value: &serde_json::Value) -> Result<(), RequestError> {
    if value.is_null() {
        return Ok(());
    }

    let bucket = value.as_object().ok_or(RequestError::MalformedRequest)?;
    validate_token_bucket_object(bucket)
}

fn validate_token_bucket_object(
    bucket: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), RequestError> {
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
    optional_u64_field(bucket, TOKEN_BUCKET_ONE_TIME_BURST_FIELD)?;

    Ok(())
}

fn require_u64_field(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<u64, RequestError> {
    object
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .ok_or(RequestError::MalformedRequest)
}

fn optional_u64_field(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<u64>, RequestError> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }

    value
        .as_u64()
        .map(Some)
        .ok_or(RequestError::MalformedRequest)
}

pub fn request_total_len(bytes: &[u8]) -> Result<Option<usize>, RequestError> {
    request_total_len_with_limit(bytes, HTTP_MAX_PAYLOAD_SIZE)
}

pub fn request_total_len_with_limit(
    bytes: &[u8],
    max_payload_size: usize,
) -> Result<Option<usize>, RequestError> {
    let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut request = httparse::Request::new(&mut headers);
    let status = request
        .parse(bytes)
        .map_err(|_| RequestError::MalformedRequest)?;
    let header_len = match status {
        httparse::Status::Complete(header_len) => header_len,
        httparse::Status::Partial => {
            if bytes.len() >= HTTP_MAX_REQUEST_HEAD_SIZE {
                return Err(RequestError::MalformedRequest);
            }
            return Ok(None);
        }
    };
    let body = request_body(request.headers)?;
    checked_request_head_len(header_len)?;

    if body.has_unsupported_encoding() {
        return Err(RequestError::MalformedRequest);
    }

    checked_payload_len(body.content_length(), max_payload_size)?;

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
    header_len
        .checked_add(content_length)
        .ok_or(RequestError::PayloadTooLarge)
}

fn checked_payload_len(content_length: usize, max_payload_size: usize) -> Result<(), RequestError> {
    if content_length > max_payload_size {
        return Err(RequestError::PayloadTooLarge);
    }

    Ok(())
}

fn checked_request_head_len(header_len: usize) -> Result<(), RequestError> {
    if header_len > HTTP_MAX_REQUEST_HEAD_SIZE {
        return Err(RequestError::MalformedRequest);
    }

    Ok(())
}

impl From<ApiRequest> for Endpoint {
    fn from(request: ApiRequest) -> Self {
        match request {
            ApiRequest::GetInstanceInfo => Self::DescribeInstance,
            ApiRequest::GetMachineConfig => Self::MachineConfig,
            ApiRequest::GetMmds => Self::Mmds,
            ApiRequest::GetVmConfig => Self::VmConfig,
            ApiRequest::GetVersion => Self::Version,
            ApiRequest::GetMemoryHotplug
            | ApiRequest::PutMemoryHotplug(_)
            | ApiRequest::PatchMemoryHotplug(_) => Self::MemoryHotplug,
            ApiRequest::GetBalloon
            | ApiRequest::GetBalloonStats
            | ApiRequest::GetBalloonHintingStatus
            | ApiRequest::PutBalloon(_)
            | ApiRequest::PatchBalloon(_)
            | ApiRequest::PatchBalloonStats(_)
            | ApiRequest::PatchBalloonHintingStart(_)
            | ApiRequest::PatchBalloonHintingStop => Self::Balloon,
            ApiRequest::PatchVmState(_) => Self::VmState,
            ApiRequest::PutAction(_) => Self::Actions,
            ApiRequest::PutBootSource(_) => Self::BootSource,
            ApiRequest::PutCpuConfig(_) => Self::CpuConfig,
            ApiRequest::PutEntropy(_) => Self::Entropy,
            ApiRequest::PutDrive(_) | ApiRequest::PatchDrive(_) => Self::Drive,
            ApiRequest::HotUnplugDevice(request) => match request.kind() {
                HotUnplugDeviceKind::Drive => Self::Drive,
                HotUnplugDeviceKind::NetworkInterface => Self::NetworkInterface,
                HotUnplugDeviceKind::Pmem => Self::Pmem,
            },
            ApiRequest::PutLogger(_) => Self::Logger,
            ApiRequest::PutMachineConfig(_) => Self::MachineConfig,
            ApiRequest::PatchMachineConfig(_) => Self::MachineConfig,
            ApiRequest::PutMetrics(_) => Self::Metrics,
            ApiRequest::PutMmds(_) | ApiRequest::PatchMmds(_) | ApiRequest::PutMmdsConfig(_) => {
                Self::Mmds
            }
            ApiRequest::PutNetworkInterface(_) | ApiRequest::PatchNetworkInterface(_) => {
                Self::NetworkInterface
            }
            ApiRequest::PutPmem(_) | ApiRequest::PatchPmem(_) => Self::Pmem,
            ApiRequest::PutSerial(_) => Self::Serial,
            ApiRequest::PutSnapshotCreate(_) | ApiRequest::PutSnapshotLoad(_) => Self::Snapshot,
            ApiRequest::PutVsock(_) => Self::Vsock,
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

    fn request_without_body(method: &str, path: &str) -> Vec<u8> {
        format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").into_bytes()
    }

    #[test]
    fn empty_mutating_request_fault_messages_are_firecracker_shaped() {
        for (err, message) in [
            (RequestError::EmptyPutRequest, "Empty PUT request."),
            (RequestError::EmptyPatchRequest, "Empty PATCH request."),
            (RequestError::EmptyDeleteRequest, "Empty Delete request."),
        ] {
            assert_eq!(err.fault_message(), message);
        }
    }

    #[test]
    fn bodyless_unknown_put_or_patch_uses_empty_faults() {
        for (method, expected) in [
            ("PUT", RequestError::EmptyPutRequest),
            ("PATCH", RequestError::EmptyPatchRequest),
        ] {
            assert_eq!(
                parse_request(&request_without_body(method, "/unknown")),
                Err(expected),
                "{method}"
            );
        }
    }

    #[test]
    fn nonempty_unknown_put_or_patch_stays_invalid_path_method() {
        for method in ["PUT", "PATCH"] {
            assert_eq!(
                parse_request(&request_with_body(method, "/unknown", "{}")),
                Err(RequestError::InvalidPathMethod),
                "{method}"
            );
        }
    }

    #[test]
    fn body_required_mutating_requests_without_body_use_empty_faults() {
        for (method, path, expected) in [
            ("PUT", "/actions", RequestError::EmptyPutRequest),
            ("PATCH", "/vm", RequestError::EmptyPatchRequest),
        ] {
            assert_eq!(
                parse_request(&request_without_body(method, path)),
                Err(expected),
                "{method} {path}"
            );
        }
    }

    #[test]
    fn identifies_put_api_request_metric_endpoints_from_request_head() {
        for (path, endpoint) in [
            ("/actions", ApiRequestMetricPutEndpoint::Actions),
            ("/balloon", ApiRequestMetricPutEndpoint::Balloon),
            ("/boot-source", ApiRequestMetricPutEndpoint::BootSource),
            ("/cpu-config", ApiRequestMetricPutEndpoint::CpuConfig),
            ("/drives/rootfs", ApiRequestMetricPutEndpoint::Drive),
            (
                "/hotplug/memory",
                ApiRequestMetricPutEndpoint::HotplugMemory,
            ),
            ("/logger", ApiRequestMetricPutEndpoint::Logger),
            (
                "/machine-config",
                ApiRequestMetricPutEndpoint::MachineConfig,
            ),
            ("/metrics", ApiRequestMetricPutEndpoint::Metrics),
            ("/mmds", ApiRequestMetricPutEndpoint::Mmds),
            ("/mmds/config", ApiRequestMetricPutEndpoint::Mmds),
            (
                "/network-interfaces/eth0",
                ApiRequestMetricPutEndpoint::Network,
            ),
            ("/pmem/pmem0", ApiRequestMetricPutEndpoint::Pmem),
            ("/serial", ApiRequestMetricPutEndpoint::Serial),
            ("/vsock", ApiRequestMetricPutEndpoint::Vsock),
        ] {
            assert_eq!(
                api_request_metric_endpoint(&request_with_body("PUT", path, "{")),
                Some(ApiRequestMetricEndpoint::Put(endpoint)),
                "PUT {path}"
            );
        }
    }

    #[test]
    fn identifies_patch_api_request_metric_endpoints_from_request_head() {
        for (path, endpoint) in [
            ("/balloon", ApiRequestMetricPatchEndpoint::Balloon),
            (
                "/balloon/statistics",
                ApiRequestMetricPatchEndpoint::Balloon,
            ),
            (
                "/balloon/hinting/start",
                ApiRequestMetricPatchEndpoint::Balloon,
            ),
            (
                "/balloon/hinting/stop",
                ApiRequestMetricPatchEndpoint::Balloon,
            ),
            ("/drives/rootfs", ApiRequestMetricPatchEndpoint::Drive),
            (
                "/hotplug/memory",
                ApiRequestMetricPatchEndpoint::HotplugMemory,
            ),
            (
                "/machine-config",
                ApiRequestMetricPatchEndpoint::MachineConfig,
            ),
            ("/mmds", ApiRequestMetricPatchEndpoint::Mmds),
            (
                "/network-interfaces/eth0",
                ApiRequestMetricPatchEndpoint::Network,
            ),
            ("/pmem/pmem0", ApiRequestMetricPatchEndpoint::Pmem),
        ] {
            assert_eq!(
                api_request_metric_endpoint(&request_with_body("PATCH", path, "{")),
                Some(ApiRequestMetricEndpoint::Patch(endpoint)),
                "PATCH {path}"
            );
        }
    }

    #[test]
    fn ignores_requests_without_matching_api_request_metric_endpoint() {
        for request in [
            request_with_body("GET", "/metrics", "{}"),
            request_with_body("GET", "/balloon", "{}"),
            request_with_body("DELETE", "/drives/rootfs", "{}"),
            request_with_body("PUT", "/entropy", "{}"),
            request_with_body("PATCH", "/vm", "{}"),
            request_with_body("PUT", "/balloon/extra", "{}"),
            request_with_body("PATCH", "/balloon/hinting/status", "{}"),
            request_with_body("PATCH", "/balloon/hinting/start/extra", "{}"),
            request_with_body("PUT", "/metrics/extra", "{}"),
            request_with_body("PUT", "/drives/rootfs/extra", "{}"),
            request_with_body("PUT", "/drives/rootfs?debug=true", "{}"),
            b"PUT /metrics HTTP/1.1\r\nHost: localhost\r\n".to_vec(),
        ] {
            assert_eq!(api_request_metric_endpoint(&request), None);
        }
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
    fn parses_request_body_at_custom_payload_limit() {
        let body = "{}";
        let request = request_with_body("PUT", "/logger", body);

        assert!(request.len() > body.len());
        assert_eq!(
            parse_request_with_limit(&request, body.len()),
            Ok(ApiRequest::PutLogger(Box::new(LoggerConfigRequest {
                log_path: None,
                level: None,
                show_level: None,
                show_log_origin: None,
                module: None,
            })))
        );
        assert_eq!(
            request_total_len_with_limit(&request, body.len()),
            Ok(Some(request.len()))
        );
    }

    #[test]
    fn parses_request_with_headers_above_custom_payload_limit() {
        let body = "{}";
        let padding = "a".repeat(64);
        let request = format!(
            "PUT /logger HTTP/1.1\r\nHost: localhost\r\nX-Fill: {padding}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        assert!(request.len() > body.len());
        assert_eq!(
            parse_request_with_limit(request.as_bytes(), body.len()),
            Ok(ApiRequest::PutLogger(Box::new(LoggerConfigRequest {
                log_path: None,
                level: None,
                show_level: None,
                show_log_origin: None,
                module: None,
            })))
        );
        assert_eq!(
            request_total_len_with_limit(request.as_bytes(), body.len()),
            Ok(Some(request.len()))
        );
    }

    #[test]
    fn rejects_body_over_custom_payload_limit() {
        let request = request_with_body("PUT", "/logger", "{}");

        assert_eq!(
            parse_request_with_limit(&request, 1),
            Err(RequestError::PayloadTooLarge)
        );
        assert_eq!(
            request_total_len_with_limit(&request, 1),
            Err(RequestError::PayloadTooLarge)
        );
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
            "SendCtrlAltDel is not supported on aarch64."
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

        assert_eq!(parse_request(&request), Err(RequestError::EmptyPutRequest));
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

        assert_eq!(parse_request(&request), Err(RequestError::EmptyPutRequest));
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

        assert_eq!(parse_request(&request), Err(RequestError::EmptyPutRequest));
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
    fn parses_put_machine_config_with_firecracker_cpu_templates() {
        for (template, expected) in [
            ("C3", MachineConfigCpuTemplate::C3),
            ("T2", MachineConfigCpuTemplate::T2),
            ("T2S", MachineConfigCpuTemplate::T2S),
            ("T2CL", MachineConfigCpuTemplate::T2CL),
            ("T2A", MachineConfigCpuTemplate::T2A),
            ("V1N1", MachineConfigCpuTemplate::V1N1),
        ] {
            let body =
                format!(r#"{{"vcpu_count":1,"mem_size_mib":128,"cpu_template":"{template}"}}"#);
            let request = request_with_body("PUT", "/machine-config", &body);

            let parsed = parse_request(&request).expect("CPU template should parse");

            let ApiRequest::PutMachineConfig(config) = parsed else {
                panic!("expected machine-config request");
            };
            assert_eq!(config.cpu_template(), Some(expected), "{template}");
        }
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
    fn rejects_put_machine_config_duplicate_field() {
        for body in [
            r#"{"vcpu_count":1,"vcpu_count":2,"mem_size_mib":128}"#,
            r#"{"vcpu_count":1,"mem_size_mib":128,"mem_size_mib":256}"#,
            r#"{"vcpu_count":1,"mem_size_mib":128,"cpu_template":null,"cpu_template":"None"}"#,
        ] {
            assert_eq!(
                parse_request(&request_with_body("PUT", "/machine-config", body)),
                Err(RequestError::MalformedRequest),
                "{body}"
            );
        }
    }

    #[test]
    fn rejects_put_machine_config_invalid_field_type() {
        for body in [
            r#"{"vcpu_count":"1","mem_size_mib":128}"#,
            r#"{"vcpu_count":1,"mem_size_mib":128,"smt":"true"}"#,
            r#"{"vcpu_count":1,"mem_size_mib":128,"huge_pages":2}"#,
        ] {
            assert_eq!(
                parse_request(&request_with_body("PUT", "/machine-config", body)),
                Err(RequestError::MalformedRequest),
                "{body}"
            );
        }
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
    fn parses_put_machine_config_with_runtime_unsupported_values() {
        for body in [
            r#"{"vcpu_count":1,"mem_size_mib":128,"smt":true}"#,
            r#"{"vcpu_count":1,"mem_size_mib":128,"huge_pages":"2M"}"#,
        ] {
            let parsed = parse_request(&request_with_body("PUT", "/machine-config", body))
                .expect("known unsupported machine config value should parse");

            let ApiRequest::PutMachineConfig(config) = parsed else {
                panic!("expected machine-config request");
            };
            if body.contains(r#""smt":true"#) {
                assert!(config.smt(), "{body}");
            }
            if body.contains(r#""huge_pages":"2M""#) {
                assert_eq!(config.huge_pages(), MachineConfigHugePages::TwoM, "{body}");
            }
        }
    }

    #[test]
    fn parses_put_machine_config_with_dirty_page_tracking_enabled() {
        let body = r#"{
            "vcpu_count": 1,
            "mem_size_mib": 128,
            "track_dirty_pages": true
        }"#;
        let request = request_with_body("PUT", "/machine-config", body);

        let parsed = parse_request(&request).expect("machine-config request should parse");

        let ApiRequest::PutMachineConfig(config) = parsed else {
            panic!("expected machine-config request");
        };
        assert!(config.track_dirty_pages());
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
    fn parses_patch_machine_config_with_partial_body() {
        let body = r#"{
            "mem_size_mib": 512,
            "cpu_template": "None"
        }"#;
        let request = request_with_body("PATCH", "/machine-config", body);

        let parsed = parse_request(&request).expect("machine-config patch should parse");

        let ApiRequest::PatchMachineConfig(config) = parsed else {
            panic!("expected machine-config patch request");
        };
        assert_eq!(config.vcpu_count(), None);
        assert_eq!(config.mem_size_mib(), Some(512));
        assert_eq!(config.smt(), None);
        assert_eq!(config.cpu_template(), Some(MachineConfigCpuTemplate::None));
        assert_eq!(config.track_dirty_pages(), None);
        assert_eq!(config.huge_pages(), None);
        assert_eq!(request_total_len(&request), Ok(Some(request.len())));
    }

    #[test]
    fn parses_patch_machine_config_with_firecracker_cpu_templates() {
        for (template, expected) in [
            ("C3", MachineConfigCpuTemplate::C3),
            ("T2", MachineConfigCpuTemplate::T2),
            ("T2S", MachineConfigCpuTemplate::T2S),
            ("T2CL", MachineConfigCpuTemplate::T2CL),
            ("T2A", MachineConfigCpuTemplate::T2A),
            ("V1N1", MachineConfigCpuTemplate::V1N1),
        ] {
            let body = format!(r#"{{"cpu_template":"{template}"}}"#);
            let request = request_with_body("PATCH", "/machine-config", &body);

            let parsed = parse_request(&request).expect("CPU template patch should parse");

            let ApiRequest::PatchMachineConfig(config) = parsed else {
                panic!("expected machine-config patch request");
            };
            assert_eq!(config.cpu_template(), Some(expected), "{template}");
        }
    }

    #[test]
    fn parses_patch_machine_config_with_accepted_default_values() {
        let body = r#"{
            "smt": false,
            "track_dirty_pages": false,
            "huge_pages": "None"
        }"#;
        let request = request_with_body("PATCH", "/machine-config", body);

        let parsed = parse_request(&request).expect("machine-config patch defaults should parse");

        let ApiRequest::PatchMachineConfig(config) = parsed else {
            panic!("expected machine-config patch request");
        };
        assert_eq!(config.smt(), Some(false));
        assert_eq!(config.track_dirty_pages(), Some(false));
        assert_eq!(config.huge_pages(), Some(MachineConfigHugePages::None));
    }

    #[test]
    fn parses_patch_machine_config_treating_null_fields_as_omitted() {
        let body = r#"{
            "vcpu_count": 2,
            "smt": null,
            "cpu_template": null
        }"#;
        let request = request_with_body("PATCH", "/machine-config", body);

        let parsed = parse_request(&request).expect("machine-config patch nulls should parse");

        let ApiRequest::PatchMachineConfig(config) = parsed else {
            panic!("expected machine-config patch request");
        };
        assert_eq!(config.vcpu_count(), Some(2));
        assert_eq!(config.smt(), None);
        assert_eq!(config.cpu_template(), None);
    }

    #[test]
    fn rejects_patch_machine_config_empty_body() {
        for body in [r#"{}"#, r#"{"smt":null}"#, r#"{"cpu_template":null}"#] {
            assert_eq!(
                parse_request(&request_with_body("PATCH", "/machine-config", body)),
                Err(RequestError::MalformedRequest)
            );
        }
    }

    #[test]
    fn rejects_patch_machine_config_unknown_field() {
        let body = r#"{
            "mem_size_mib": 512,
            "unknown": true
        }"#;
        let request = request_with_body("PATCH", "/machine-config", body);

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_patch_machine_config_duplicate_field() {
        for body in [
            r#"{"mem_size_mib":128,"mem_size_mib":256}"#,
            r#"{"cpu_template":null,"cpu_template":"None"}"#,
            r#"{"smt":false,"smt":true}"#,
        ] {
            assert_eq!(
                parse_request(&request_with_body("PATCH", "/machine-config", body)),
                Err(RequestError::MalformedRequest),
                "{body}"
            );
        }
    }

    #[test]
    fn rejects_patch_machine_config_invalid_numeric_bounds() {
        for body in [
            r#"{"vcpu_count":0}"#,
            r#"{"vcpu_count":33}"#,
            r#"{"mem_size_mib":0}"#,
        ] {
            assert_eq!(
                parse_request(&request_with_body("PATCH", "/machine-config", body)),
                Err(RequestError::MalformedRequest)
            );
        }
    }

    #[test]
    fn rejects_patch_machine_config_invalid_field_type() {
        for body in [r#"{"smt":"true"}"#, r#"{"huge_pages":2}"#] {
            assert_eq!(
                parse_request(&request_with_body("PATCH", "/machine-config", body)),
                Err(RequestError::MalformedRequest),
                "{body}"
            );
        }
    }

    #[test]
    fn parses_patch_machine_config_with_runtime_unsupported_values() {
        for body in [r#"{"smt":true}"#, r#"{"huge_pages":"2M"}"#] {
            let parsed = parse_request(&request_with_body("PATCH", "/machine-config", body))
                .expect("known unsupported machine config patch value should parse");

            let ApiRequest::PatchMachineConfig(config) = parsed else {
                panic!("expected machine-config patch request");
            };
            if body.contains(r#""smt":true"#) {
                assert_eq!(config.smt(), Some(true), "{body}");
            }
            if body.contains(r#""huge_pages":"2M""#) {
                assert_eq!(
                    config.huge_pages(),
                    Some(MachineConfigHugePages::TwoM),
                    "{body}"
                );
            }
        }
    }

    #[test]
    fn rejects_machine_config_unknown_cpu_template() {
        for (method, path, body) in [
            (
                "PUT",
                "/machine-config",
                r#"{"vcpu_count":1,"mem_size_mib":128,"cpu_template":"M7G"}"#,
            ),
            ("PATCH", "/machine-config", r#"{"cpu_template":"M7G"}"#),
        ] {
            assert_eq!(
                parse_request(&request_with_body(method, path, body)),
                Err(RequestError::MalformedRequest),
                "{method} {path}"
            );
        }
    }

    #[test]
    fn rejects_machine_config_unknown_huge_pages() {
        for (method, path, body) in [
            (
                "PUT",
                "/machine-config",
                r#"{"vcpu_count":1,"mem_size_mib":128,"huge_pages":"7M"}"#,
            ),
            ("PATCH", "/machine-config", r#"{"huge_pages":"7M"}"#),
        ] {
            assert_eq!(
                parse_request(&request_with_body(method, path, body)),
                Err(RequestError::MalformedRequest),
                "{method} {path}"
            );
        }
    }

    #[test]
    fn parses_patch_machine_config_with_dirty_page_tracking_enabled() {
        let body = r#"{"track_dirty_pages":true}"#;
        let request = request_with_body("PATCH", "/machine-config", body);

        let parsed = parse_request(&request).expect("machine-config patch should parse");

        let ApiRequest::PatchMachineConfig(config) = parsed else {
            panic!("expected machine-config patch request");
        };
        assert_eq!(config.track_dirty_pages(), Some(true));
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

        assert_eq!(parse_request(&request), Err(RequestError::EmptyPutRequest));
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
    fn parses_get_mmds() {
        let request = b"GET /mmds HTTP/1.1\r\nHost: localhost\r\n\r\n";

        assert_eq!(parse_request(request), Ok(ApiRequest::GetMmds));
        assert_eq!(request_total_len(request), Ok(Some(request.len())));
    }

    #[test]
    fn parses_put_mmds_with_object_body() {
        let body = r#"{"latest":{"meta-data":{"ami-id":"ami-123"}}}"#;
        let request = request_with_body("PUT", "/mmds", body);

        let parsed = parse_request(&request).expect("MMDS PUT request should parse");

        let ApiRequest::PutMmds(content) = parsed else {
            panic!("expected MMDS PUT request");
        };
        assert_eq!(
            content.value(),
            &serde_json::json!({"latest":{"meta-data":{"ami-id":"ami-123"}}})
        );
        assert_eq!(request_total_len(&request), Ok(Some(request.len())));
    }

    #[test]
    fn parses_patch_mmds_with_object_body() {
        let body = r#"{"latest":{"dynamic":{"instance-identity":"document"}}}"#;
        let request = request_with_body("PATCH", "/mmds", body);

        let parsed = parse_request(&request).expect("MMDS PATCH request should parse");

        let ApiRequest::PatchMmds(content) = parsed else {
            panic!("expected MMDS PATCH request");
        };
        assert_eq!(
            content.value(),
            &serde_json::json!({"latest":{"dynamic":{"instance-identity":"document"}}})
        );
        assert_eq!(request_total_len(&request), Ok(Some(request.len())));
    }

    #[test]
    fn parses_put_mmds_config_with_minimal_body() {
        let body = r#"{"network_interfaces":["eth0"]}"#;
        let request = request_with_body("PUT", "/mmds/config", body);

        let parsed = parse_request(&request).expect("MMDS config request should parse");

        let ApiRequest::PutMmdsConfig(config) = parsed else {
            panic!("expected MMDS config request");
        };
        assert_eq!(config.network_interfaces(), &["eth0".to_string()]);
        assert_eq!(config.version(), MmdsVersion::V1);
        assert_eq!(config.ipv4_address(), None);
        assert!(!config.imds_compat());
    }

    #[test]
    fn parses_put_mmds_config_with_empty_network_interfaces() {
        let request = request_with_body("PUT", "/mmds/config", r#"{"network_interfaces":[]}"#);

        let parsed = parse_request(&request).expect("empty MMDS interface list should parse");

        let ApiRequest::PutMmdsConfig(config) = parsed else {
            panic!("expected MMDS config request");
        };
        assert!(config.network_interfaces().is_empty());
        assert_eq!(config.version(), MmdsVersion::V1);
        assert_eq!(config.ipv4_address(), None);
        assert!(!config.imds_compat());
    }

    #[test]
    fn parses_put_mmds_config_with_complete_body() {
        let body = r#"{
            "network_interfaces": ["eth0", "mgmt0"],
            "version": "V2",
            "ipv4_address": "169.254.169.250",
            "imds_compat": true
        }"#;
        let request = request_with_body("PUT", "/mmds/config", body);

        let parsed = parse_request(&request).expect("complete MMDS config should parse");

        let ApiRequest::PutMmdsConfig(config) = parsed else {
            panic!("expected MMDS config request");
        };
        assert_eq!(
            config.network_interfaces(),
            &["eth0".to_string(), "mgmt0".to_string()]
        );
        assert_eq!(config.version(), MmdsVersion::V2);
        assert_eq!(
            config.ipv4_address(),
            Some(std::net::Ipv4Addr::new(169, 254, 169, 250))
        );
        assert!(config.imds_compat());
        assert_eq!(request_total_len(&request), Ok(Some(request.len())));
    }

    #[test]
    fn parses_put_mmds_config_with_link_local_ipv4_boundaries() {
        for ipv4_address in ["169.254.1.0", "169.254.254.255"] {
            let body =
                format!(r#"{{"network_interfaces":["eth0"],"ipv4_address":"{ipv4_address}"}}"#);
            let request = request_with_body("PUT", "/mmds/config", &body);

            let parsed = parse_request(&request).expect("boundary MMDS config should parse");

            let ApiRequest::PutMmdsConfig(config) = parsed else {
                panic!("expected MMDS config request");
            };
            assert_eq!(
                config.ipv4_address().map(|address| address.to_string()),
                Some(ipv4_address.to_string())
            );
        }
    }

    #[test]
    fn rejects_get_mmds_with_body() {
        let request = request_with_body("GET", "/mmds", "{}");

        assert_eq!(parse_request(&request), Err(RequestError::GetRequestBody));
    }

    #[test]
    fn rejects_put_or_patch_mmds_without_object_body() {
        for (method, expected) in [
            ("PUT", RequestError::EmptyPutRequest),
            ("PATCH", RequestError::EmptyPatchRequest),
        ] {
            assert_eq!(
                parse_request(&request_with_body(method, "/mmds", "")),
                Err(expected),
                "{method}"
            );
        }

        for (method, body) in [
            ("PUT", "[]"),
            ("PUT", "null"),
            ("PATCH", r#""metadata""#),
            ("PATCH", "42"),
        ] {
            assert_eq!(
                parse_request(&request_with_body(method, "/mmds", body)),
                Err(RequestError::MalformedRequest)
            );
        }
    }

    #[test]
    fn rejects_put_mmds_config_invalid_body() {
        for body in [
            "{}",
            r#"{"network_interfaces":["eth0",""]}"#,
            r#"{"network_interfaces":["eth0","   "]}"#,
            r#"{"network_interfaces":["eth0"],"version":"V3"}"#,
            r#"{"network_interfaces":["eth0"],"ipv4_address":"127.0.0.1"}"#,
            r#"{"network_interfaces":["eth0"],"ipv4_address":"169.254.0.1"}"#,
            r#"{"network_interfaces":["eth0"],"ipv4_address":"169.254.255.1"}"#,
            r#"{"network_interfaces":["eth0"],"ipv4_address":"not-an-ip"}"#,
            r#"{"network_interfaces":["eth0"],"imds_compat":"true"}"#,
            r#"{"network_interfaces":["eth0"],"unknown":true}"#,
        ] {
            assert_eq!(
                parse_request(&request_with_body("PUT", "/mmds/config", body)),
                Err(RequestError::MalformedRequest)
            );
        }
    }

    #[test]
    fn rejects_unsupported_mmds_method_or_path() {
        assert_eq!(
            parse_request(b"POST /mmds HTTP/1.1\r\nHost: localhost\r\n\r\n"),
            Err(RequestError::InvalidPathMethod)
        );
        assert_eq!(
            parse_request(&request_with_body("PUT", "/mmds/extra", "{}")),
            Err(RequestError::InvalidPathMethod)
        );
        assert_eq!(
            parse_request(b"GET /mmds/config HTTP/1.1\r\nHost: localhost\r\n\r\n"),
            Err(RequestError::InvalidPathMethod)
        );
    }

    #[test]
    fn rejects_put_mmds_payload_over_limit() {
        let body = "{}";
        let request = format!(
            "PUT /mmds HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}",
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
    fn parses_put_vsock_with_minimal_body() {
        let body = r#"{
            "guest_cid": 3,
            "uds_path": "./v.sock"
        }"#;
        let request = request_with_body("PUT", "/vsock", body);

        let parsed = parse_request(&request).expect("vsock request should parse");

        let ApiRequest::PutVsock(config) = parsed else {
            panic!("expected vsock request");
        };
        assert_eq!(config.vsock_id(), None);
        assert_eq!(config.guest_cid(), 3);
        assert_eq!(config.uds_path(), "./v.sock");
        assert_eq!(request_total_len(&request), Ok(Some(request.len())));
    }

    #[test]
    fn parses_put_vsock_with_deprecated_vsock_id() {
        let body = r#"{
            "vsock_id": "vsock0",
            "guest_cid": 42,
            "uds_path": "/tmp/v.sock"
        }"#;
        let request = request_with_body("PUT", "/vsock", body);

        let parsed = parse_request(&request).expect("vsock request should parse");

        let ApiRequest::PutVsock(config) = parsed else {
            panic!("expected vsock request");
        };
        assert_eq!(config.vsock_id(), Some("vsock0"));
        assert_eq!(config.guest_cid(), 42);
        assert_eq!(config.uds_path(), "/tmp/v.sock");
    }

    #[test]
    fn parses_put_vsock_with_null_vsock_id() {
        let body = r#"{
            "vsock_id": null,
            "guest_cid": 3,
            "uds_path": "./v.sock"
        }"#;
        let request = request_with_body("PUT", "/vsock", body);

        let parsed = parse_request(&request).expect("vsock request should parse");

        let ApiRequest::PutVsock(config) = parsed else {
            panic!("expected vsock request");
        };
        assert_eq!(config.vsock_id(), None);
    }

    #[test]
    fn rejects_put_vsock_missing_required_fields() {
        assert_eq!(
            parse_request(&request_with_body(
                "PUT",
                "/vsock",
                r#"{"uds_path":"./v.sock"}"#,
            )),
            Err(RequestError::MalformedRequest)
        );
        assert_eq!(
            parse_request(&request_with_body("PUT", "/vsock", r#"{"guest_cid":3}"#)),
            Err(RequestError::MalformedRequest)
        );
    }

    #[test]
    fn rejects_put_vsock_unknown_field() {
        let body = r#"{
            "guest_cid": 3,
            "uds_path": "./v.sock",
            "unknown": true
        }"#;
        let request = request_with_body("PUT", "/vsock", body);

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_put_vsock_invalid_field_type() {
        for body in [
            r#"{"guest_cid":"3","uds_path":"./v.sock"}"#,
            r#"{"guest_cid":3,"uds_path":null}"#,
            r#"{"guest_cid":3,"uds_path":["./v.sock"]}"#,
            r#"{"vsock_id":1,"guest_cid":3,"uds_path":"./v.sock"}"#,
        ] {
            assert_eq!(
                parse_request(&request_with_body("PUT", "/vsock", body)),
                Err(RequestError::MalformedRequest)
            );
        }
    }

    #[test]
    fn rejects_put_vsock_empty_body() {
        let request = request_with_body("PUT", "/vsock", "");

        assert_eq!(parse_request(&request), Err(RequestError::EmptyPutRequest));
    }

    #[test]
    fn rejects_unsupported_vsock_method_or_path() {
        assert_eq!(
            parse_request(b"GET /vsock HTTP/1.1\r\nHost: localhost\r\n\r\n"),
            Err(RequestError::InvalidPathMethod)
        );
        assert_eq!(
            parse_request(&request_with_body(
                "PUT",
                "/vsock/extra",
                r#"{"guest_cid":3,"uds_path":"./v.sock"}"#,
            )),
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
        assert_eq!(config.path_on_host(), Some("/tmp/rootfs.ext4"));
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
        assert_eq!(config.path_on_host(), Some("/tmp/rootfs.ext4"));
        assert!(config.is_root_device());
        assert_eq!(config.is_read_only(), Some(true));
        assert_eq!(config.partuuid(), Some("0eaa91a0-01"));
        assert_eq!(config.cache_type(), Some(DriveCacheType::Unsafe));
        assert_eq!(config.io_engine(), Some(DriveIoEngine::Sync));
        assert!(config.rate_limiter_configured());
        assert_eq!(
            config.rate_limiter(),
            Some(DriveRateLimiterRequest::new(
                Some(TokenBucketRequest::new(0, Some(0), 0)),
                None,
            ))
        );
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
    fn parses_put_drive_with_socket_without_path_on_host() {
        let body = r#"{
            "drive_id": "vhost",
            "is_root_device": false,
            "socket": "/tmp/vhost.sock"
        }"#;
        let request = request_with_body("PUT", "/drives/vhost", body);

        let parsed = parse_request(&request).expect("socket-backed drive request should parse");

        let ApiRequest::PutDrive(config) = parsed else {
            panic!("expected drive request");
        };
        assert_eq!(config.path_drive_id(), "vhost");
        assert_eq!(config.body_drive_id(), "vhost");
        assert_eq!(config.path_on_host(), None);
        assert!(!config.is_root_device());
        assert_eq!(config.socket(), Some("/tmp/vhost.sock"));
    }

    #[test]
    fn parses_put_drive_with_noop_rate_limiter_objects() {
        for body in [
            r#"{"drive_id":"rootfs","path_on_host":"/tmp/rootfs.ext4","is_root_device":true,"rate_limiter":{}}"#,
            r#"{"drive_id":"rootfs","path_on_host":"/tmp/rootfs.ext4","is_root_device":true,"rate_limiter":{"bandwidth":null}}"#,
            r#"{"drive_id":"rootfs","path_on_host":"/tmp/rootfs.ext4","is_root_device":true,"rate_limiter":{"ops":null}}"#,
            r#"{"drive_id":"rootfs","path_on_host":"/tmp/rootfs.ext4","is_root_device":true,"rate_limiter":{"bandwidth":null,"ops":null}}"#,
        ] {
            let request = request_with_body("PUT", "/drives/rootfs", body);

            let parsed = parse_request(&request).expect("drive request should parse");

            let ApiRequest::PutDrive(config) = parsed else {
                panic!("expected drive request");
            };
            assert!(!config.rate_limiter_configured(), "{body}");
        }
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

        assert_eq!(parse_request(request), Err(RequestError::EmptyPutRequest));
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
    fn rejects_put_drive_without_path_on_host_or_socket() {
        for (path, body) in [
            (
                "/drives/rootfs",
                r#"{"drive_id":"rootfs","is_root_device":true}"#,
            ),
            (
                "/drives/rootfs",
                r#"{"drive_id":"rootfs","is_root_device":true,"path_on_host":null,"socket":null}"#,
            ),
            (
                "/drives/other",
                r#"{"drive_id":"rootfs","is_root_device":true}"#,
            ),
        ] {
            let request = request_with_body("PUT", path, body);

            assert_eq!(
                parse_request(&request),
                Err(RequestError::MalformedRequest),
                "{body}"
            );
        }
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
    fn rejects_put_drive_duplicate_rate_limiter_fields() {
        for body in [
            r#"{
                "drive_id": "rootfs",
                "path_on_host": "/tmp/rootfs.ext4",
                "is_root_device": true,
                "rate_limiter": {
                    "ops": null,
                    "ops": null
                }
            }"#,
            r#"{
                "drive_id": "rootfs",
                "path_on_host": "/tmp/rootfs.ext4",
                "is_root_device": true,
                "rate_limiter": {
                    "ops": {
                        "size": 100,
                        "size": 200,
                        "refill_time": 1000
                    }
                }
            }"#,
        ] {
            let request = request_with_body("PUT", "/drives/rootfs", body);

            assert_eq!(
                parse_request(&request),
                Err(RequestError::MalformedRequest),
                "{body}"
            );
        }
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
        assert_eq!(
            config.rate_limiter(),
            Some(DriveRateLimiterRequest::new(
                None,
                Some(TokenBucketRequest::new(100, None, 1000)),
            ))
        );
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
    fn parses_patch_drive_with_path_update() {
        let request = request_with_body(
            "PATCH",
            "/drives/rootfs",
            r#"{"drive_id":"rootfs","path_on_host":"/tmp/replaced.ext4"}"#,
        );

        let parsed = parse_request(&request).expect("drive patch should parse");

        let ApiRequest::PatchDrive(config) = parsed else {
            panic!("expected drive patch request");
        };
        assert_eq!(config.path_drive_id(), "rootfs");
        assert_eq!(config.body_drive_id(), "rootfs");
        assert_eq!(config.path_on_host(), Some("/tmp/replaced.ext4"));
        assert!(!config.rate_limiter_configured());
    }

    #[test]
    fn parses_patch_drive_with_only_drive_id() {
        let request = request_with_body("PATCH", "/drives/rootfs", r#"{"drive_id":"rootfs"}"#);

        let parsed = parse_request(&request).expect("drive patch should parse");

        let ApiRequest::PatchDrive(config) = parsed else {
            panic!("expected drive patch request");
        };
        assert_eq!(config.path_drive_id(), "rootfs");
        assert_eq!(config.body_drive_id(), "rootfs");
        assert_eq!(config.path_on_host(), None);
        assert!(!config.rate_limiter_configured());
    }

    #[test]
    fn parses_patch_drive_with_null_optional_fields() {
        let request = request_with_body(
            "PATCH",
            "/drives/rootfs",
            r#"{"drive_id":"rootfs","path_on_host":null,"rate_limiter":null}"#,
        );

        let parsed = parse_request(&request).expect("drive patch should parse");

        let ApiRequest::PatchDrive(config) = parsed else {
            panic!("expected drive patch request");
        };
        assert_eq!(config.path_drive_id(), "rootfs");
        assert_eq!(config.body_drive_id(), "rootfs");
        assert_eq!(config.path_on_host(), None);
        assert!(!config.rate_limiter_configured());
    }

    #[test]
    fn parses_patch_drive_with_noop_rate_limiter_objects() {
        for body in [
            r#"{"drive_id":"rootfs","rate_limiter":{}}"#,
            r#"{"drive_id":"rootfs","rate_limiter":{"bandwidth":null}}"#,
            r#"{"drive_id":"rootfs","rate_limiter":{"ops":null}}"#,
            r#"{"drive_id":"rootfs","rate_limiter":{"bandwidth":null,"ops":null}}"#,
        ] {
            let request = request_with_body("PATCH", "/drives/rootfs", body);

            let parsed = parse_request(&request).expect("drive patch should parse");

            let ApiRequest::PatchDrive(config) = parsed else {
                panic!("expected drive patch request");
            };
            assert_eq!(config.path_drive_id(), "rootfs", "{body}");
            assert_eq!(config.body_drive_id(), "rootfs", "{body}");
            assert_eq!(config.path_on_host(), None, "{body}");
            assert!(!config.rate_limiter_configured(), "{body}");
        }
    }

    #[test]
    fn rejects_patch_drive_malformed_body() {
        let request = request_with_body("PATCH", "/drives/rootfs", "not-json");

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_patch_drive_missing_drive_id() {
        let request = request_with_body("PATCH", "/drives/rootfs", r#"{"path_on_host":"x"}"#);

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_patch_drive_mismatched_drive_id() {
        let request = request_with_body("PATCH", "/drives/rootfs", r#"{"drive_id":"data"}"#);

        assert_eq!(
            parse_request(&request),
            Err(RequestError::MismatchedDriveId)
        );
    }

    #[test]
    fn rejects_patch_drive_unknown_field() {
        let request = request_with_body(
            "PATCH",
            "/drives/rootfs",
            r#"{"drive_id":"rootfs","is_read_only":true}"#,
        );

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_patch_drive_invalid_rate_limiter_shape() {
        let request = request_with_body(
            "PATCH",
            "/drives/rootfs",
            r#"{"drive_id":"rootfs","rate_limiter":"unsupported"}"#,
        );

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn parses_patch_drive_rate_limiter_config() {
        let request = request_with_body(
            "PATCH",
            "/drives/rootfs",
            r#"{"drive_id":"rootfs","rate_limiter":{"ops":{"size":100,"one_time_burst":null,"refill_time":1000}}}"#,
        );

        let parsed = parse_request(&request).expect("drive patch should parse");

        let ApiRequest::PatchDrive(config) = parsed else {
            panic!("expected drive patch request");
        };
        assert_eq!(config.path_drive_id(), "rootfs");
        assert_eq!(config.body_drive_id(), "rootfs");
        assert_eq!(config.path_on_host(), None);
        assert!(config.rate_limiter_configured());
        assert_eq!(
            config.rate_limiter(),
            Some(DriveRateLimiterRequest::new(
                None,
                Some(TokenBucketRequest::new(100, None, 1000)),
            ))
        );
    }

    #[test]
    fn parses_drive_hot_unplug_without_body() {
        let request = b"DELETE /drives/rootfs HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec();
        let parsed = parse_request(&request).expect("drive hot-unplug should parse");

        let ApiRequest::HotUnplugDevice(request) = parsed else {
            panic!("expected hot-unplug request");
        };
        assert_eq!(request.kind(), HotUnplugDeviceKind::Drive);
        assert_eq!(request.id(), "rootfs");
    }

    #[test]
    fn rejects_delete_request_bodies_before_hot_unplug_routing() {
        for (route, request) in [
            (
                "DELETE /drives/rootfs",
                request_with_body("DELETE", "/drives/rootfs", "{}"),
            ),
            (
                "DELETE /network-interfaces/eth0",
                request_with_body("DELETE", "/network-interfaces/eth0", "not-json"),
            ),
            (
                "DELETE /pmem/pmem0",
                request_with_body("DELETE", "/pmem/pmem0", "{}"),
            ),
            (
                "DELETE /unknown",
                request_with_body("DELETE", "/unknown", "{}"),
            ),
        ] {
            assert_eq!(
                parse_request(&request),
                Err(RequestError::EmptyDeleteRequest),
                "{route}"
            );
        }
    }

    #[test]
    fn rejects_non_exact_drive_update_paths_as_invalid_path_method() {
        for method in ["PATCH", "DELETE"] {
            for path in [
                "/drives",
                "/drives/",
                "/drives/rootfs/extra",
                "/drives/root-fs",
                "/drives/rootfs?debug=true",
            ] {
                let request = if method == "DELETE" {
                    request_without_body(method, path)
                } else {
                    request_with_body(method, path, "{}")
                };

                assert_eq!(
                    parse_request(&request),
                    Err(RequestError::InvalidPathMethod),
                    "{method} {path}"
                );
            }
        }
    }

    #[test]
    fn rejects_unsupported_drive_update_methods_as_invalid_path_method() {
        for (route, request) in [
            (
                "GET /drives/rootfs",
                b"GET /drives/rootfs HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec(),
            ),
            (
                "POST /drives/rootfs",
                request_with_body("POST", "/drives/rootfs", "{}"),
            ),
        ] {
            assert_eq!(
                parse_request(&request),
                Err(RequestError::InvalidPathMethod),
                "{route}"
            );
        }
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
        assert_eq!(config.mtu(), None);
        assert_eq!(config.rx_rate_limiter(), None);
        assert_eq!(config.tx_rate_limiter(), None);
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
        assert_eq!(config.mtu(), Some(1500));
        assert_eq!(
            config.rx_rate_limiter(),
            Some(NetworkRateLimiterRequest::new(
                Some(TokenBucketRequest::new(1024, Some(2048), 1000)),
                None,
            ))
        );
        assert_eq!(
            config.tx_rate_limiter(),
            Some(NetworkRateLimiterRequest::new(
                None,
                Some(TokenBucketRequest::new(100, None, 1000)),
            ))
        );
    }

    #[test]
    fn rejects_put_network_interface_duplicate_rate_limiter_fields() {
        for body in [
            r#"{
                "iface_id": "eth0",
                "host_dev_name": "tap0",
                "rx_rate_limiter": {
                    "bandwidth": null,
                    "bandwidth": null
                }
            }"#,
            r#"{
                "iface_id": "eth0",
                "host_dev_name": "tap0",
                "tx_rate_limiter": {
                    "bandwidth": {
                        "size": 1024,
                        "one_time_burst": null,
                        "refill_time": 1000,
                        "refill_time": 2000
                    }
                }
            }"#,
        ] {
            let request = request_with_body("PUT", "/network-interfaces/eth0", body);

            assert_eq!(
                parse_request(&request),
                Err(RequestError::MalformedRequest),
                "{body}"
            );
        }
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
        assert_eq!(config.mtu(), None);
        assert_eq!(config.rx_rate_limiter(), None);
        assert_eq!(config.tx_rate_limiter(), None);
    }

    #[test]
    fn parses_put_network_interface_with_noop_rate_limiter_objects() {
        for (body, expected_rx, expected_tx) in [
            (
                r#"{"iface_id":"eth0","host_dev_name":"tap0","rx_rate_limiter":{},"tx_rate_limiter":{}}"#,
                false,
                false,
            ),
            (
                r#"{"iface_id":"eth0","host_dev_name":"tap0","rx_rate_limiter":{"bandwidth":null},"tx_rate_limiter":{"ops":null}}"#,
                false,
                false,
            ),
            (
                r#"{"iface_id":"eth0","host_dev_name":"tap0","rx_rate_limiter":{"bandwidth":null,"ops":{"size":100,"one_time_burst":null,"refill_time":1000}},"tx_rate_limiter":{"bandwidth":null,"ops":null}}"#,
                true,
                false,
            ),
        ] {
            let request = request_with_body("PUT", "/network-interfaces/eth0", body);

            let parsed = parse_request(&request).expect("network request should parse");

            let ApiRequest::PutNetworkInterface(config) = parsed else {
                panic!("expected network interface request");
            };
            assert_eq!(config.rx_rate_limiter().is_some(), expected_rx, "{body}");
            assert_eq!(config.tx_rate_limiter().is_some(), expected_tx, "{body}");
        }
    }

    #[test]
    fn parses_put_network_interface_with_explicit_disabled_rate_limiter_buckets() {
        let body = r#"{
            "iface_id":"eth0",
            "host_dev_name":"tap0",
            "rx_rate_limiter":{"bandwidth":{"size":0,"one_time_burst":123,"refill_time":100}},
            "tx_rate_limiter":{"ops":{"size":10,"one_time_burst":null,"refill_time":0}}
        }"#;
        let request = request_with_body("PUT", "/network-interfaces/eth0", body);

        let parsed = parse_request(&request).expect("disabled limiter controls should parse");
        let ApiRequest::PutNetworkInterface(config) = parsed else {
            panic!("expected network interface request");
        };
        assert_eq!(
            config.rx_rate_limiter(),
            Some(NetworkRateLimiterRequest::new(
                Some(TokenBucketRequest::new(0, Some(123), 100)),
                None,
            ))
        );
        assert_eq!(
            config.tx_rate_limiter(),
            Some(NetworkRateLimiterRequest::new(
                None,
                Some(TokenBucketRequest::new(10, None, 0)),
            ))
        );
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

        assert_eq!(parse_request(request), Err(RequestError::EmptyPutRequest));
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
    fn parses_valid_network_interface_patch() {
        for (body, rx_rate_limiter_configured, tx_rate_limiter_configured) in [
            (r#"{"iface_id":"eth0"}"#, false, false),
            (
                r#"{"iface_id":"eth0","rx_rate_limiter":null,"tx_rate_limiter":null}"#,
                false,
                false,
            ),
            (
                r#"{"iface_id":"eth0","rx_rate_limiter":{},"tx_rate_limiter":{"bandwidth":null,"ops":null}}"#,
                false,
                false,
            ),
            (
                r#"{"iface_id":"eth0","rx_rate_limiter":{"bandwidth":{"size":100,"one_time_burst":null,"refill_time":1000}}}"#,
                true,
                false,
            ),
            (
                r#"{"iface_id":"eth0","tx_rate_limiter":{"ops":{"size":100,"one_time_burst":200,"refill_time":1000}}}"#,
                false,
                true,
            ),
            (
                r#"{"iface_id":"eth0","rx_rate_limiter":{"bandwidth":null,"ops":{"size":100,"one_time_burst":null,"refill_time":1000}},"tx_rate_limiter":{"bandwidth":null}}"#,
                true,
                false,
            ),
        ] {
            let request = request_with_body("PATCH", "/network-interfaces/eth0", body);
            let parsed = parse_request(&request).expect("network interface patch should parse");

            let ApiRequest::PatchNetworkInterface(config) = parsed else {
                panic!("expected network interface patch request");
            };
            assert_eq!(config.path_iface_id(), "eth0", "{body}");
            assert_eq!(config.body_iface_id(), "eth0", "{body}");
            assert_eq!(
                config.rx_rate_limiter_configured(),
                rx_rate_limiter_configured,
                "{body}"
            );
            assert_eq!(
                config.tx_rate_limiter_configured(),
                tx_rate_limiter_configured,
                "{body}"
            );
        }
    }

    #[test]
    fn rejects_invalid_network_interface_patch_before_vmm_dispatch() {
        assert_eq!(
            parse_request(&request_with_body("PATCH", "/network-interfaces/eth0", "")),
            Err(RequestError::EmptyPatchRequest)
        );

        for body in [
            "not-json",
            "{}",
            r#"{"iface_id":"eth0","unknown":true}"#,
            r#"{"iface_id":"eth0","rx_rate_limiter":"unsupported"}"#,
            r#"{"iface_id":"eth0","rx_rate_limiter":{"ops":{"size":100}}}"#,
            r#"{"iface_id":"eth0","tx_rate_limiter":{"bandwidth":{"size":100}}}"#,
        ] {
            let request = request_with_body("PATCH", "/network-interfaces/eth0", body);
            assert_eq!(
                parse_request(&request),
                Err(RequestError::MalformedRequest),
                "{body}"
            );
        }

        let request = request_with_body(
            "PATCH",
            "/network-interfaces/eth0",
            r#"{"iface_id":"eth1"}"#,
        );
        assert_eq!(
            parse_request(&request),
            Err(RequestError::MismatchedInterfaceId)
        );
    }

    #[test]
    fn parses_network_interface_delete_without_body() {
        let request = b"DELETE /network-interfaces/eth0 HTTP/1.1\r\nHost: localhost\r\n\r\n";
        let parsed = parse_request(request).expect("network interface delete should parse");

        let ApiRequest::HotUnplugDevice(request) = parsed else {
            panic!("expected hot-unplug request");
        };
        assert_eq!(request.kind(), HotUnplugDeviceKind::NetworkInterface);
        assert_eq!(request.id(), "eth0");
    }

    #[test]
    fn rejects_non_exact_network_interface_update_paths_as_invalid_path_method() {
        for method in ["PATCH", "DELETE"] {
            for path in [
                "/network-interfaces",
                "/network-interfaces/",
                "/network-interfaces/eth0/extra",
                "/network-interfaces/eth-0",
                "/network-interfaces/eth0?debug=true",
            ] {
                let request = if method == "DELETE" {
                    request_without_body(method, path)
                } else {
                    request_with_body(method, path, "{}")
                };

                assert_eq!(
                    parse_request(&request),
                    Err(RequestError::InvalidPathMethod),
                    "{method} {path}"
                );
            }
        }
    }

    #[test]
    fn rejects_unsupported_network_interface_update_methods_as_invalid_path_method() {
        for (route, request) in [
            (
                "GET /network-interfaces/eth0",
                b"GET /network-interfaces/eth0 HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec(),
            ),
            (
                "POST /network-interfaces/eth0",
                request_with_body("POST", "/network-interfaces/eth0", "{}"),
            ),
        ] {
            assert_eq!(
                parse_request(&request),
                Err(RequestError::InvalidPathMethod),
                "{route}"
            );
        }
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
    fn bodyless_unsupported_put_or_patch_method_uses_empty_faults() {
        for (method, expected) in [
            ("PUT", RequestError::EmptyPutRequest),
            ("PATCH", RequestError::EmptyPatchRequest),
        ] {
            assert_eq!(
                parse_request(&request_without_body(method, "/version")),
                Err(expected),
                "{method}"
            );
        }
    }

    #[test]
    fn nonempty_unsupported_put_or_patch_method_stays_invalid_path_method() {
        for method in ["PUT", "PATCH"] {
            assert_eq!(
                parse_request(&request_with_body(method, "/version", "{}")),
                Err(RequestError::InvalidPathMethod),
                "{method}"
            );
        }
    }

    #[test]
    fn rejects_unsupported_path() {
        let request = b"GET /unknown HTTP/1.1\r\n\r\n";

        assert_eq!(parse_request(request), Err(RequestError::InvalidPathMethod));
    }

    #[test]
    fn parses_empty_cpu_config_request() {
        let request = request_with_body("PUT", "/cpu-config", "{}");

        let parsed = parse_request(&request).expect("empty cpu-config should parse");

        let ApiRequest::PutCpuConfig(config) = parsed else {
            panic!("expected cpu-config request");
        };
        assert_eq!(config.category(), None);
    }

    #[test]
    fn parses_empty_array_cpu_config_request_as_noop() {
        let request = request_with_body(
            "PUT",
            "/cpu-config",
            r#"{"kvm_capabilities":[],"reg_modifiers":[],"vcpu_features":[]}"#,
        );

        let parsed = parse_request(&request).expect("empty cpu-config arrays should parse");

        let ApiRequest::PutCpuConfig(config) = parsed else {
            panic!("expected cpu-config request");
        };
        assert_eq!(config.category(), None);
    }

    #[test]
    fn classifies_firecracker_shaped_cpu_config_request_categories() {
        for (body, expected) in [
            (
                r#"{"kvm_capabilities":["4294967295"]}"#,
                CpuConfigTemplateCategory::KvmCapabilities,
            ),
            (
                r#"{"reg_modifiers":[{"addr":"0x0030000000000000","bitmap":"0b10100101"}]}"#,
                CpuConfigTemplateCategory::ArmRegisterModifiers,
            ),
            (
                r#"{"vcpu_features":[{"index":31415926,"bitmap":"0b11010011"}]}"#,
                CpuConfigTemplateCategory::VcpuFeatures,
            ),
            (
                r#"{"kvm_capabilities":["4294967295"],"reg_modifiers":[{"addr":"0x0030000000000000","bitmap":"0b10100101"}]}"#,
                CpuConfigTemplateCategory::Mixed,
            ),
            (
                r#"{"kvm_capabilities":["4294967295"],"vcpu_features":[{"index":31415926,"bitmap":"0b11010011"}]}"#,
                CpuConfigTemplateCategory::Mixed,
            ),
            (
                r#"{"reg_modifiers":[{"addr":"0x0030000000000000","bitmap":"0b10100101"}],"vcpu_features":[{"index":31415926,"bitmap":"0b11010011"}]}"#,
                CpuConfigTemplateCategory::Mixed,
            ),
        ] {
            let request = request_with_body("PUT", "/cpu-config", body);
            let parsed = parse_request(&request).expect("cpu-config should parse");

            let ApiRequest::PutCpuConfig(config) = parsed else {
                panic!("expected cpu-config request");
            };
            assert_eq!(config.category(), Some(expected), "{body}");

            let debug = format!("{config:?}");
            for raw_value in [
                "4294967295",
                "0x0030000000000000",
                "0b10100101",
                "31415926",
                "0b11010011",
            ] {
                assert!(!debug.contains(raw_value), "{body}: {debug}");
            }
        }
    }

    #[test]
    fn classifies_three_cpu_config_categories_as_mixed() {
        let body = r#"{
            "kvm_capabilities": ["1", "!2"],
            "reg_modifiers": [
                {
                    "addr": "0x0030000000000000",
                    "bitmap": "0bx00100x0x1xxxx01xxx1xxxxxxxxxxx1"
                }
            ],
            "vcpu_features": [
                {
                    "index": 0,
                    "bitmap": "0b1100000"
                }
            ]
        }"#;
        let request = request_with_body("PUT", "/cpu-config", body);

        let parsed = parse_request(&request).expect("cpu-config should parse");

        let ApiRequest::PutCpuConfig(config) = parsed else {
            panic!("expected cpu-config request");
        };
        assert_eq!(config.category(), Some(CpuConfigTemplateCategory::Mixed));
    }

    #[test]
    fn rejects_malformed_cpu_config_bodies() {
        assert_eq!(
            parse_request(&request_with_body("PUT", "/cpu-config", "")),
            Err(RequestError::EmptyPutRequest)
        );

        for body in [
            "not-json",
            "[]",
            "null",
            r#"{"unknown":[]}"#,
            r#"{"kvm_capabilities":null}"#,
            r#"{"kvm_capabilities":["!"]}"#,
            r#"{"kvm_capabilities":["!a2"]}"#,
            r#"{"cpuid_modifiers":[]}"#,
            r#"{"msr_modifiers":[]}"#,
            r#"{"reg_modifiers":[{"addr":"0x1"}]}"#,
            r#"{"reg_modifiers":[{"addr":"1","bitmap":"0b1"}]}"#,
            r#"{"reg_modifiers":[{"addr":"0x1","bitmap":"0b2"}]}"#,
            r#"{"reg_modifiers":[{"addr":"0x0010000000000000","bitmap":"0b1"}]}"#,
            r#"{"reg_modifiers":[["0x0030000000000000","0b1"]]}"#,
            r#"{"vcpu_features":[[0,"0b1"]]}"#,
            r#"{"vcpu_features":[{"index":4294967296,"bitmap":"0b1"}]}"#,
        ] {
            let request = request_with_body("PUT", "/cpu-config", body);

            assert_eq!(
                parse_request(&request),
                Err(RequestError::MalformedRequest),
                "{body}"
            );
        }

        let high_bit_body = format!(
            r#"{{"reg_modifiers":[{{"addr":"0x0030000000000000","bitmap":"0b1{}"}}]}}"#,
            "0".repeat(64)
        );
        let request = request_with_body("PUT", "/cpu-config", &high_bit_body);

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_duplicate_cpu_config_fields() {
        for body in [
            r#"{"kvm_capabilities":[],"kvm_capabilities":[]}"#,
            r#"{"reg_modifiers":[],"reg_modifiers":[]}"#,
            r#"{"vcpu_features":[],"vcpu_features":[]}"#,
            r#"{"reg_modifiers":[{"addr":"0x0030000000000000","addr":"0x0030000000000000","bitmap":"0b1"}]}"#,
            r#"{"reg_modifiers":[{"addr":"0x0030000000000000","bitmap":"0b1","bitmap":"0b0"}]}"#,
            r#"{"vcpu_features":[{"index":0,"index":1,"bitmap":"0b1"}]}"#,
            r#"{"vcpu_features":[{"index":0,"bitmap":"0b1","bitmap":"0b0"}]}"#,
        ] {
            let request = request_with_body("PUT", "/cpu-config", body);

            assert_eq!(
                parse_request(&request),
                Err(RequestError::MalformedRequest),
                "{body}"
            );
        }
    }

    #[test]
    fn rejects_non_exact_cpu_config_path_as_invalid_path_method() {
        let request = request_with_body("PUT", "/cpu-config/extra", "{}");

        assert_eq!(
            parse_request(&request),
            Err(RequestError::InvalidPathMethod)
        );
    }

    #[test]
    fn parses_entropy_config_without_configured_rate_limiter() {
        for body in [
            "{}",
            r#"{"rate_limiter":null}"#,
            r#"{"rate_limiter":{}}"#,
            r#"{"rate_limiter":{"bandwidth":null,"ops":null}}"#,
        ] {
            let request = request_with_body("PUT", "/entropy", body);

            let parsed = parse_request(&request).expect("entropy config should parse");

            let ApiRequest::PutEntropy(config) = parsed else {
                panic!("expected entropy config request");
            };
            assert_eq!(config.rate_limiter(), None, "{body}");
        }
    }

    #[test]
    fn parses_entropy_config_with_configured_rate_limiter_buckets() {
        for (body, expected) in [
            (
                r#"{"rate_limiter":{"bandwidth":{"size":100,"one_time_burst":null,"refill_time":1000}}}"#,
                EntropyRateLimiterRequest::new(
                    Some(TokenBucketRequest::new(100, None, 1000)),
                    None,
                ),
            ),
            (
                r#"{"rate_limiter":{"ops":{"size":100,"one_time_burst":200,"refill_time":1000}}}"#,
                EntropyRateLimiterRequest::new(
                    None,
                    Some(TokenBucketRequest::new(100, Some(200), 1000)),
                ),
            ),
            (
                r#"{"rate_limiter":{"bandwidth":{"size":1,"one_time_burst":2,"refill_time":3},"ops":{"size":4,"one_time_burst":null,"refill_time":5}}}"#,
                EntropyRateLimiterRequest::new(
                    Some(TokenBucketRequest::new(1, Some(2), 3)),
                    Some(TokenBucketRequest::new(4, None, 5)),
                ),
            ),
        ] {
            let request = request_with_body("PUT", "/entropy", body);

            let parsed = parse_request(&request).expect("entropy config should parse");

            let ApiRequest::PutEntropy(config) = parsed else {
                panic!("expected entropy config request");
            };
            assert_eq!(config.rate_limiter(), Some(expected), "{body}");
        }
    }

    #[test]
    fn rejects_invalid_entropy_config_before_unsupported() {
        assert_eq!(
            parse_request(&request_with_body("PUT", "/entropy", "")),
            Err(RequestError::EmptyPutRequest)
        );

        for body in [
            "not-json",
            r#"{"unknown":true}"#,
            r#"{"rate_limiter":"unsupported"}"#,
            r#"{"rate_limiter":{"bad":{"size":1,"refill_time":1}}}"#,
            r#"{"rate_limiter":{"bandwidth":{"size":1}}}"#,
            r#"{"rate_limiter":{"ops":{"refill_time":1}}}"#,
            r#"{"rate_limiter":{"ops":null,"ops":null}}"#,
            r#"{"rate_limiter":{"bandwidth":{"size":1,"refill_time":1,"refill_time":2}}}"#,
        ] {
            let request = request_with_body("PUT", "/entropy", body);

            assert_eq!(
                parse_request(&request),
                Err(RequestError::MalformedRequest),
                "{body}"
            );
        }
    }

    #[test]
    fn rejects_non_exact_entropy_path_as_invalid_path_method() {
        let request = request_with_body("PUT", "/entropy/extra", "{}");

        assert_eq!(
            parse_request(&request),
            Err(RequestError::InvalidPathMethod)
        );
    }

    #[test]
    fn parses_valid_balloon_methods() {
        let requests = [
            (
                "GET /balloon",
                b"GET /balloon HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec(),
                ApiRequest::GetBalloon,
            ),
            (
                "GET /balloon/statistics",
                b"GET /balloon/statistics HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec(),
                ApiRequest::GetBalloonStats,
            ),
            (
                "GET /balloon/hinting/status",
                b"GET /balloon/hinting/status HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec(),
                ApiRequest::GetBalloonHintingStatus,
            ),
            (
                "PUT /balloon",
                request_with_body(
                    "PUT",
                    "/balloon",
                    r#"{"amount_mib":64,"deflate_on_oom":true}"#,
                ),
                ApiRequest::PutBalloon(Box::new(BalloonConfigRequest {
                    amount_mib: 64,
                    deflate_on_oom: true,
                    stats_polling_interval_s: 0,
                    free_page_hinting: false,
                    free_page_reporting: false,
                })),
            ),
            (
                "PUT /balloon optional fields",
                request_with_body(
                    "PUT",
                    "/balloon",
                    r#"{"amount_mib":64,"deflate_on_oom":false,"stats_polling_interval_s":1,"free_page_hinting":true,"free_page_reporting":false}"#,
                ),
                ApiRequest::PutBalloon(Box::new(BalloonConfigRequest {
                    amount_mib: 64,
                    deflate_on_oom: false,
                    stats_polling_interval_s: 1,
                    free_page_hinting: true,
                    free_page_reporting: false,
                })),
            ),
            (
                "PUT /balloon max numeric fields",
                request_with_body(
                    "PUT",
                    "/balloon",
                    r#"{"amount_mib":4294967295,"deflate_on_oom":true,"stats_polling_interval_s":65535}"#,
                ),
                ApiRequest::PutBalloon(Box::new(BalloonConfigRequest {
                    amount_mib: 4_294_967_295,
                    deflate_on_oom: true,
                    stats_polling_interval_s: 65_535,
                    free_page_hinting: false,
                    free_page_reporting: false,
                })),
            ),
            (
                "PATCH /balloon",
                request_with_body("PATCH", "/balloon", r#"{"amount_mib":32}"#),
                ApiRequest::PatchBalloon(Box::new(BalloonUpdateRequest { amount_mib: 32 })),
            ),
            (
                "PATCH /balloon max amount",
                request_with_body("PATCH", "/balloon", r#"{"amount_mib":4294967295}"#),
                ApiRequest::PatchBalloon(Box::new(BalloonUpdateRequest {
                    amount_mib: 4_294_967_295,
                })),
            ),
            (
                "PATCH /balloon/statistics",
                request_with_body(
                    "PATCH",
                    "/balloon/statistics",
                    r#"{"stats_polling_interval_s":1}"#,
                ),
                ApiRequest::PatchBalloonStats(Box::new(BalloonStatsUpdateRequest {
                    stats_polling_interval_s: 1,
                })),
            ),
            (
                "PATCH /balloon/statistics max interval",
                request_with_body(
                    "PATCH",
                    "/balloon/statistics",
                    r#"{"stats_polling_interval_s":65535}"#,
                ),
                ApiRequest::PatchBalloonStats(Box::new(BalloonStatsUpdateRequest {
                    stats_polling_interval_s: 65_535,
                })),
            ),
            (
                "PATCH /balloon/hinting/start without body",
                b"PATCH /balloon/hinting/start HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec(),
                ApiRequest::PatchBalloonHintingStart(BalloonHintingStartRequest::new(true)),
            ),
            (
                "PATCH /balloon/hinting/start empty body",
                request_with_body("PATCH", "/balloon/hinting/start", ""),
                ApiRequest::PatchBalloonHintingStart(BalloonHintingStartRequest::new(true)),
            ),
            (
                "PATCH /balloon/hinting/start",
                request_with_body("PATCH", "/balloon/hinting/start", "{}"),
                ApiRequest::PatchBalloonHintingStart(BalloonHintingStartRequest::new(true)),
            ),
            (
                "PATCH /balloon/hinting/start empty sequence",
                request_with_body("PATCH", "/balloon/hinting/start", "[]"),
                ApiRequest::PatchBalloonHintingStart(BalloonHintingStartRequest::new(true)),
            ),
            (
                "PATCH /balloon/hinting/start explicit",
                request_with_body(
                    "PATCH",
                    "/balloon/hinting/start",
                    r#"{"acknowledge_on_stop":false}"#,
                ),
                ApiRequest::PatchBalloonHintingStart(BalloonHintingStartRequest::new(false)),
            ),
            (
                "PATCH /balloon/hinting/start unknown field",
                request_with_body(
                    "PATCH",
                    "/balloon/hinting/start",
                    r#"{"acknowledge_on_stop":false,"unknown":true}"#,
                ),
                ApiRequest::PatchBalloonHintingStart(BalloonHintingStartRequest::new(false)),
            ),
            (
                "PATCH /balloon/hinting/start only unknown field",
                request_with_body("PATCH", "/balloon/hinting/start", r#"{"unknown":true}"#),
                ApiRequest::PatchBalloonHintingStart(BalloonHintingStartRequest::new(true)),
            ),
            (
                "PATCH /balloon/hinting/stop",
                b"PATCH /balloon/hinting/stop HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec(),
                ApiRequest::PatchBalloonHintingStop,
            ),
        ];

        for (route, request, expected) in requests {
            let parsed = parse_request(&request).expect("balloon request should parse");
            assert_eq!(parsed, expected, "{route}");
        }
    }

    #[test]
    fn rejects_invalid_balloon_body_methods_before_unsupported() {
        for (method, path, expected) in [
            ("PUT", "/balloon", RequestError::EmptyPutRequest),
            ("PATCH", "/balloon", RequestError::EmptyPatchRequest),
            (
                "PATCH",
                "/balloon/statistics",
                RequestError::EmptyPatchRequest,
            ),
        ] {
            assert_eq!(
                parse_request(&request_with_body(method, path, "")),
                Err(expected),
                "{method} {path}"
            );
        }

        for (method, path, body) in [
            ("PUT", "/balloon", "not-json"),
            ("PUT", "/balloon", "null"),
            ("PUT", "/balloon", "[]"),
            ("PUT", "/balloon", "{}"),
            ("PUT", "/balloon", r#"{"amount_mib":64}"#),
            ("PUT", "/balloon", r#"{"deflate_on_oom":true}"#),
            (
                "PUT",
                "/balloon",
                r#"{"amount_mib":"64","deflate_on_oom":true}"#,
            ),
            (
                "PUT",
                "/balloon",
                r#"{"amount_mib":-1,"deflate_on_oom":true}"#,
            ),
            (
                "PUT",
                "/balloon",
                r#"{"amount_mib":4294967296,"deflate_on_oom":true}"#,
            ),
            (
                "PUT",
                "/balloon",
                r#"{"amount_mib":64,"deflate_on_oom":"true"}"#,
            ),
            (
                "PUT",
                "/balloon",
                r#"{"amount_mib":64,"deflate_on_oom":true,"stats_polling_interval_s":65536}"#,
            ),
            (
                "PUT",
                "/balloon",
                r#"{"amount_mib":64,"deflate_on_oom":true,"free_page_hinting":null}"#,
            ),
            (
                "PUT",
                "/balloon",
                r#"{"amount_mib":64,"deflate_on_oom":true,"unknown":true}"#,
            ),
            ("PATCH", "/balloon", "not-json"),
            ("PATCH", "/balloon", "null"),
            ("PATCH", "/balloon", "[]"),
            ("PATCH", "/balloon", "{}"),
            ("PATCH", "/balloon", r#"{"amount_mib":"32"}"#),
            ("PATCH", "/balloon", r#"{"amount_mib":-1}"#),
            ("PATCH", "/balloon", r#"{"amount_mib":4294967296}"#),
            ("PATCH", "/balloon", r#"{"amount_mib":32,"unknown":true}"#),
            ("PATCH", "/balloon/statistics", "not-json"),
            ("PATCH", "/balloon/statistics", "null"),
            ("PATCH", "/balloon/statistics", "[]"),
            ("PATCH", "/balloon/statistics", "{}"),
            (
                "PATCH",
                "/balloon/statistics",
                r#"{"stats_polling_interval_s":"1"}"#,
            ),
            (
                "PATCH",
                "/balloon/statistics",
                r#"{"stats_polling_interval_s":-1}"#,
            ),
            (
                "PATCH",
                "/balloon/statistics",
                r#"{"stats_polling_interval_s":65536}"#,
            ),
            (
                "PATCH",
                "/balloon/statistics",
                r#"{"stats_polling_interval_s":1,"unknown":true}"#,
            ),
            ("PATCH", "/balloon/hinting/start", "not-json"),
            ("PATCH", "/balloon/hinting/start", "null"),
            (
                "PATCH",
                "/balloon/hinting/start",
                r#"{"acknowledge_on_stop":"false"}"#,
            ),
        ] {
            let request = request_with_body(method, path, body);

            assert_eq!(
                parse_request(&request),
                Err(RequestError::MalformedRequest),
                "{method} {path} {body}"
            );
        }
    }

    #[test]
    fn parses_balloon_hinting_stop_without_parsing_body() {
        for request in [
            request_with_body("PATCH", "/balloon/hinting/stop", "not-json"),
            request_with_body("PATCH", "/balloon/hinting/stop", ""),
        ] {
            assert_eq!(
                parse_request(&request),
                Ok(ApiRequest::PatchBalloonHintingStop)
            );
        }
    }

    #[test]
    fn rejects_balloon_get_with_body_before_endpoint_handling() {
        for path in ["/balloon", "/balloon/statistics", "/balloon/hinting/status"] {
            let request = request_with_body("GET", path, "{}");

            assert_eq!(
                parse_request(&request),
                Err(RequestError::GetRequestBody),
                "{path}"
            );
        }
    }

    #[test]
    fn rejects_non_exact_balloon_paths_as_invalid_path_method() {
        let requests = [
            (
                "GET /balloon/extra",
                b"GET /balloon/extra HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec(),
            ),
            (
                "GET /balloon/hinting",
                b"GET /balloon/hinting HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec(),
            ),
            (
                "PUT /balloon/extra",
                request_with_body("PUT", "/balloon/extra", "{}"),
            ),
            (
                "PATCH /balloon/hinting",
                request_with_body("PATCH", "/balloon/hinting", "{}"),
            ),
            (
                "PATCH /balloon/hinting/status",
                request_with_body("PATCH", "/balloon/hinting/status", "{}"),
            ),
        ];

        for (route, request) in requests {
            assert_eq!(
                parse_request(&request),
                Err(RequestError::InvalidPathMethod),
                "{route}"
            );
        }
    }

    #[test]
    fn rejects_unsupported_balloon_methods_as_invalid_path_method() {
        let requests = [
            (
                "DELETE /balloon",
                b"DELETE /balloon HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec(),
            ),
            ("POST /balloon", request_with_body("POST", "/balloon", "{}")),
            (
                "PUT /balloon/statistics",
                request_with_body("PUT", "/balloon/statistics", "{}"),
            ),
            (
                "PUT /balloon/hinting/start",
                request_with_body("PUT", "/balloon/hinting/start", "{}"),
            ),
            (
                "GET /balloon/hinting/start",
                b"GET /balloon/hinting/start HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec(),
            ),
            (
                "GET /balloon/hinting/stop",
                b"GET /balloon/hinting/stop HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec(),
            ),
            (
                "PATCH /balloon/hinting/status",
                request_with_body("PATCH", "/balloon/hinting/status", "{}"),
            ),
        ];

        for (route, request) in requests {
            assert_eq!(
                parse_request(&request),
                Err(RequestError::InvalidPathMethod),
                "{route}"
            );
        }
    }

    #[test]
    fn parses_memory_hotplug_methods_after_schema_validation() {
        let get_request = b"GET /hotplug/memory HTTP/1.1\r\nHost: localhost\r\n\r\n";
        assert_eq!(
            parse_request(get_request),
            Ok(ApiRequest::GetMemoryHotplug),
            "GET"
        );

        for (method, body, expected) in [
            (
                "PUT",
                r#"{"total_size_mib":2048}"#,
                ApiRequest::PutMemoryHotplug(MemoryHotplugConfigRequest::new(2048, 2, 128)),
            ),
            (
                "PUT",
                r#"{"total_size_mib":2048,"block_size_mib":4,"slot_size_mib":256}"#,
                ApiRequest::PutMemoryHotplug(MemoryHotplugConfigRequest::new(2048, 4, 256)),
            ),
            (
                "PATCH",
                r#"{"requested_size_mib":256}"#,
                ApiRequest::PatchMemoryHotplug(MemoryHotplugSizeUpdateRequest::new(256)),
            ),
        ] {
            let request = request_with_body(method, "/hotplug/memory", body);
            assert_eq!(parse_request(&request), Ok(expected), "{method} {body}");
        }
    }

    #[test]
    fn rejects_invalid_memory_hotplug_body_methods_before_vmm_dispatch() {
        for (method, expected) in [
            ("PUT", RequestError::EmptyPutRequest),
            ("PATCH", RequestError::EmptyPatchRequest),
        ] {
            assert_eq!(
                parse_request(&request_with_body(method, "/hotplug/memory", "")),
                Err(expected),
                "{method}"
            );
        }

        for (method, body) in [
            ("PUT", "not-json"),
            ("PUT", "{}"),
            ("PUT", r#"{"size_mib":128}"#),
            ("PUT", r#"{"total_size_mib":-1}"#),
            ("PUT", r#"{"total_size_mib":"2048"}"#),
            ("PUT", r#"{"total_size_mib":2048,"block_size_mib":null}"#),
            ("PUT", r#"{"total_size_mib":2048,"slot_size_mib":null}"#),
            ("PATCH", "not-json"),
            ("PATCH", "{}"),
            ("PATCH", r#"{"size_mib":256}"#),
            ("PATCH", r#"{"requested_size_mib":-1}"#),
            ("PATCH", r#"{"requested_size_mib":null}"#),
            ("PATCH", r#"{"requested_size_mib":"256"}"#),
        ] {
            let request = request_with_body(method, "/hotplug/memory", body);

            assert_eq!(
                parse_request(&request),
                Err(RequestError::MalformedRequest),
                "{method} {body}"
            );
        }
    }

    #[test]
    fn rejects_memory_hotplug_get_with_body_before_endpoint_handling() {
        let request = request_with_body("GET", "/hotplug/memory", "{}");

        assert_eq!(parse_request(&request), Err(RequestError::GetRequestBody));
    }

    #[test]
    fn rejects_non_exact_memory_hotplug_path_as_invalid_path_method() {
        let requests = [
            (
                "GET",
                b"GET /hotplug/memory/extra HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec(),
            ),
            (
                "PUT",
                request_with_body("PUT", "/hotplug/memory/extra", "{}"),
            ),
            (
                "PATCH",
                request_with_body("PATCH", "/hotplug/memory/extra", "{}"),
            ),
        ];

        for (method, request) in requests {
            assert_eq!(
                parse_request(&request),
                Err(RequestError::InvalidPathMethod),
                "{method}"
            );
        }
    }

    #[test]
    fn parses_valid_pmem_body_methods() {
        for (route, request, expected_root_device, expected_read_only, expected_rate_limiter) in [
            (
                "PUT /pmem/pmem0",
                request_with_body(
                    "PUT",
                    "/pmem/pmem0",
                    r#"{"id":"pmem0","path_on_host":"/tmp/pmem.img"}"#,
                ),
                false,
                false,
                false,
            ),
            (
                "PUT /pmem/pmem_0",
                request_with_body(
                    "PUT",
                    "/pmem/pmem_0",
                    r#"{"id":"pmem_0","path_on_host":"/tmp/pmem.img","root_device":true,"read_only":false,"rate_limiter":{"bandwidth":{"size":100,"one_time_burst":null,"refill_time":1000}}}"#,
                ),
                true,
                false,
                true,
            ),
            (
                "PUT /pmem/pmem1 empty rate limiter",
                request_with_body(
                    "PUT",
                    "/pmem/pmem1",
                    r#"{"id":"pmem1","path_on_host":"/tmp/pmem.img","rate_limiter":{}}"#,
                ),
                false,
                false,
                false,
            ),
            (
                "PUT /pmem/pmem2 null rate limiter",
                request_with_body(
                    "PUT",
                    "/pmem/pmem2",
                    r#"{"id":"pmem2","path_on_host":"/tmp/pmem.img","rate_limiter":null}"#,
                ),
                false,
                false,
                false,
            ),
            (
                "PUT /pmem/pmem3 all-null rate limiter buckets",
                request_with_body(
                    "PUT",
                    "/pmem/pmem3",
                    r#"{"id":"pmem3","path_on_host":"/tmp/pmem.img","rate_limiter":{"bandwidth":null,"ops":null}}"#,
                ),
                false,
                false,
                false,
            ),
        ] {
            let parsed = parse_request(&request).expect("pmem request should parse");
            let ApiRequest::PutPmem(config) = parsed else {
                panic!("unexpected pmem request for {route}");
            };
            assert_eq!(config.path_pmem_id(), config.body_pmem_id(), "{route}");
            assert_eq!(config.path_on_host(), "/tmp/pmem.img", "{route}");
            assert_eq!(config.root_device(), expected_root_device, "{route}");
            assert_eq!(config.read_only(), expected_read_only, "{route}");
            assert_eq!(
                config.rate_limiter_configured(),
                expected_rate_limiter,
                "{route}"
            );
        }

        for (route, request, expected_rate_limiter) in [
            (
                "PATCH /pmem/pmem0",
                request_with_body("PATCH", "/pmem/pmem0", r#"{"id":"pmem0"}"#),
                false,
            ),
            (
                "PATCH /pmem/pmem0 empty rate limiter",
                request_with_body(
                    "PATCH",
                    "/pmem/pmem0",
                    r#"{"id":"pmem0","rate_limiter":{}}"#,
                ),
                false,
            ),
            (
                "PATCH /pmem/pmem0 null rate limiter",
                request_with_body(
                    "PATCH",
                    "/pmem/pmem0",
                    r#"{"id":"pmem0","rate_limiter":null}"#,
                ),
                false,
            ),
            (
                "PATCH /pmem/pmem0 all-null rate limiter buckets",
                request_with_body(
                    "PATCH",
                    "/pmem/pmem0",
                    r#"{"id":"pmem0","rate_limiter":{"bandwidth":null,"ops":null}}"#,
                ),
                false,
            ),
            (
                "PATCH /pmem/pmem0 rate limiter",
                request_with_body(
                    "PATCH",
                    "/pmem/pmem0",
                    r#"{"id":"pmem0","rate_limiter":{"ops":{"size":100,"one_time_burst":200,"refill_time":1000}}}"#,
                ),
                true,
            ),
        ] {
            let parsed = parse_request(&request).expect("pmem request should parse");
            match parsed {
                ApiRequest::PatchPmem(config) => {
                    assert_eq!(config.path_pmem_id(), "pmem0", "{route}");
                    assert_eq!(config.body_pmem_id(), "pmem0", "{route}");
                    assert_eq!(
                        config.rate_limiter_configured(),
                        expected_rate_limiter,
                        "{route}"
                    );
                }
                other => panic!("unexpected pmem request for {route}: {other:?}"),
            }
        }
    }

    #[test]
    fn rejects_invalid_pmem_body_methods_before_unsupported() {
        for (method, expected) in [
            ("PUT", RequestError::EmptyPutRequest),
            ("PATCH", RequestError::EmptyPatchRequest),
        ] {
            assert_eq!(
                parse_request(&request_with_body(method, "/pmem/pmem0", "")),
                Err(expected),
                "{method}"
            );
        }

        for (method, body) in [
            ("PUT", "not-json"),
            ("PUT", "{}"),
            ("PUT", r#"{"id":"pmem0"}"#),
            ("PUT", r#"{"id":3,"path_on_host":"/tmp/pmem.img"}"#),
            ("PUT", r#"{"id":"pmem0","path_on_host":3}"#),
            (
                "PUT",
                r#"{"id":"pmem0","path_on_host":"/tmp/pmem.img","unknown":true}"#,
            ),
            (
                "PUT",
                r#"{"id":"pmem0","path_on_host":"/tmp/pmem.img","root_device":null}"#,
            ),
            (
                "PUT",
                r#"{"id":"pmem0","path_on_host":"/tmp/pmem.img","read_only":"false"}"#,
            ),
            (
                "PUT",
                r#"{"id":"pmem0","path_on_host":"/tmp/pmem.img","rate_limiter":"bad"}"#,
            ),
            (
                "PUT",
                r#"{"id":"pmem0","path_on_host":"/tmp/pmem.img","rate_limiter":{"bandwidth":{"size":1}}}"#,
            ),
            (
                "PUT",
                r#"{"id":"pmem0","path_on_host":"/tmp/pmem.img","rate_limiter":{"ops":null,"ops":null}}"#,
            ),
            ("PATCH", "not-json"),
            ("PATCH", "{}"),
            ("PATCH", r#"{"id":3}"#),
            ("PATCH", r#"{"id":"pmem0","unknown":true}"#),
            ("PATCH", r#"{"id":"pmem0","rate_limiter":"bad"}"#),
            (
                "PATCH",
                r#"{"id":"pmem0","rate_limiter":{"ops":{"refill_time":1}}}"#,
            ),
            (
                "PATCH",
                r#"{"id":"pmem0","rate_limiter":{"bandwidth":{"size":1,"refill_time":1,"refill_time":2}}}"#,
            ),
        ] {
            let request = request_with_body(method, "/pmem/pmem0", body);

            assert_eq!(
                parse_request(&request),
                Err(RequestError::MalformedRequest),
                "{method} {body}"
            );
        }
    }

    #[test]
    fn rejects_mismatched_pmem_ids_before_unsupported() {
        for (method, body) in [
            ("PUT", r#"{"id":"other","path_on_host":"/tmp/pmem.img"}"#),
            ("PATCH", r#"{"id":"other"}"#),
        ] {
            let request = request_with_body(method, "/pmem/pmem0", body);

            let err = parse_request(&request).expect_err("mismatched pmem id should fail");
            assert_eq!(err, RequestError::MismatchedPmemId, "{method}");
            assert_eq!(err.fault_message(), "path pmem id must match body id.");
        }
    }

    #[test]
    fn parses_pmem_delete_without_body() {
        let request = b"DELETE /pmem/pmem0 HTTP/1.1\r\nHost: localhost\r\n\r\n";
        let parsed = parse_request(request).expect("pmem delete should parse");

        let ApiRequest::HotUnplugDevice(request) = parsed else {
            panic!("expected hot-unplug request");
        };
        assert_eq!(request.kind(), HotUnplugDeviceKind::Pmem);
        assert_eq!(request.id(), "pmem0");
    }

    #[test]
    fn rejects_non_exact_pmem_paths_as_invalid_path_method() {
        for method in ["PUT", "PATCH", "DELETE"] {
            for path in [
                "/pmem",
                "/pmem/",
                "/pmem/pmem0/extra",
                "/pmem/pmem-0",
                "/pmem/pmem0?debug=true",
            ] {
                let request = if method == "DELETE" {
                    request_without_body(method, path)
                } else {
                    request_with_body(method, path, "{}")
                };

                assert_eq!(
                    parse_request(&request),
                    Err(RequestError::InvalidPathMethod),
                    "{method} {path}"
                );
            }
        }
    }

    #[test]
    fn rejects_unsupported_pmem_methods_as_invalid_path_method() {
        for (route, request) in [
            (
                "GET /pmem/pmem0",
                b"GET /pmem/pmem0 HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec(),
            ),
            (
                "POST /pmem/pmem0",
                request_with_body("POST", "/pmem/pmem0", "{}"),
            ),
        ] {
            assert_eq!(
                parse_request(&request),
                Err(RequestError::InvalidPathMethod),
                "{route}"
            );
        }
    }

    #[test]
    fn parses_serial_config_with_output_path() {
        let request =
            request_with_body("PUT", "/serial", r#"{"serial_out_path":"/tmp/serial.out"}"#);

        let parsed = parse_request(&request).expect("serial config should parse");

        let ApiRequest::PutSerial(config) = parsed else {
            panic!("expected serial config request");
        };
        assert_eq!(config.serial_out_path(), Some("/tmp/serial.out"));
        assert_eq!(config.rate_limiter(), None);
    }

    #[test]
    fn parses_serial_config_clear_request() {
        for body in [r#"{}"#, r#"{"serial_out_path":null}"#] {
            let parsed = parse_request(&request_with_body("PUT", "/serial", body))
                .expect("serial clear request should parse");

            let ApiRequest::PutSerial(config) = parsed else {
                panic!("expected serial config request");
            };
            assert_eq!(config.serial_out_path(), None);
            assert_eq!(config.rate_limiter(), None);
        }
    }

    #[test]
    fn parses_serial_config_with_null_rate_limiter_as_unconfigured() {
        let request = request_with_body(
            "PUT",
            "/serial",
            r#"{"serial_out_path":"/tmp/serial.out","rate_limiter":null}"#,
        );

        let parsed = parse_request(&request).expect("serial config should parse");

        let ApiRequest::PutSerial(config) = parsed else {
            panic!("expected serial config request");
        };
        assert_eq!(config.serial_out_path(), Some("/tmp/serial.out"));
        assert_eq!(config.rate_limiter(), None);
    }

    #[test]
    fn parses_serial_rate_limiter_config() {
        let request = request_with_body(
            "PUT",
            "/serial",
            r#"{"rate_limiter":{"size":2,"one_time_burst":3,"refill_time":4}}"#,
        );

        let parsed = parse_request(&request).expect("serial config should parse");

        let ApiRequest::PutSerial(config) = parsed else {
            panic!("expected serial config request");
        };
        assert_eq!(config.serial_out_path(), None);
        assert_eq!(
            config.rate_limiter(),
            Some(SerialRateLimiterRequest::new(2, Some(3), 4))
        );
    }

    #[test]
    fn parses_serial_rate_limiter_null_burst_as_none() {
        let request = request_with_body(
            "PUT",
            "/serial",
            r#"{"rate_limiter":{"size":1,"one_time_burst":null,"refill_time":2}}"#,
        );

        let parsed = parse_request(&request).expect("serial config should parse");

        let ApiRequest::PutSerial(config) = parsed else {
            panic!("expected serial config request");
        };
        assert_eq!(
            config.rate_limiter(),
            Some(SerialRateLimiterRequest::new(1, None, 2))
        );
    }

    #[test]
    fn rejects_invalid_serial_rate_limiter_shape() {
        for body in [
            r#"{"rate_limiter":"unsupported"}"#,
            r#"{"rate_limiter":{"bad":{"size":1,"refill_time":1}}}"#,
            r#"{"rate_limiter":{"size":1}}"#,
            r#"{"rate_limiter":{"refill_time":1}}"#,
            r#"{"rate_limiter":{"size":"1","refill_time":1}}"#,
            r#"{"rate_limiter":{"size":1,"refill_time":"1"}}"#,
            r#"{"rate_limiter":{"size":1,"one_time_burst":"1","refill_time":1}}"#,
            r#"{"rate_limiter":{"bandwidth":{"size":1}}}"#,
            r#"{"rate_limiter":{"ops":null,"ops":null}}"#,
            r#"{"rate_limiter":{"size":1,"refill_time":1,"refill_time":2}}"#,
        ] {
            let request = request_with_body("PUT", "/serial", body);

            assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
        }
    }

    #[test]
    fn rejects_malformed_serial_config_body() {
        let malformed_body = request_with_body("PUT", "/serial", "not-json");
        let empty_body = b"PUT /serial HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n";

        assert_eq!(
            parse_request(&malformed_body),
            Err(RequestError::MalformedRequest)
        );
        assert_eq!(
            parse_request(empty_body),
            Err(RequestError::EmptyPutRequest)
        );
    }

    #[test]
    fn rejects_unknown_serial_config_fields() {
        let request = request_with_body("PUT", "/serial", r#"{"bad":true}"#);

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_non_exact_serial_path_as_invalid_path_method() {
        let request = request_with_body("PUT", "/serial/extra", "{}");

        assert_eq!(
            parse_request(&request),
            Err(RequestError::InvalidPathMethod)
        );
    }

    #[test]
    fn parses_vm_state_update_pause() {
        let request = request_with_body("PATCH", "/vm", r#"{"state":"Paused"}"#);

        let parsed = parse_request(&request).expect("VM state update should parse");

        let ApiRequest::PatchVmState(update) = parsed else {
            panic!("expected VM state update request");
        };
        assert_eq!(update.state(), VmStateUpdate::Paused);
    }

    #[test]
    fn parses_vm_state_update_resume() {
        let request = request_with_body("PATCH", "/vm", r#"{"state":"Resumed"}"#);

        let parsed = parse_request(&request).expect("VM state update should parse");

        let ApiRequest::PatchVmState(update) = parsed else {
            panic!("expected VM state update request");
        };
        assert_eq!(update.state(), VmStateUpdate::Resumed);
    }

    #[test]
    fn rejects_malformed_vm_state_update_bodies() {
        assert_eq!(
            parse_request(&request_with_body("PATCH", "/vm", "")),
            Err(RequestError::EmptyPatchRequest)
        );

        for body in [
            "not-json",
            "{}",
            r#"{"state":null}"#,
            r#"{"state":"Running"}"#,
            r#"{"state":"Paused","unknown":true}"#,
        ] {
            let request = request_with_body("PATCH", "/vm", body);

            assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
        }
    }

    #[test]
    fn rejects_non_exact_vm_state_update_path_as_invalid_path_method() {
        let request = request_with_body("PATCH", "/vm/extra", r#"{"state":"Paused"}"#);

        assert_eq!(
            parse_request(&request),
            Err(RequestError::InvalidPathMethod)
        );
    }

    #[test]
    fn parses_valid_snapshot_create_requests() {
        for (body, expected_type) in [
            (
                r#"{"snapshot_path":"vmstate","mem_file_path":"memory"}"#,
                SnapshotType::Full,
            ),
            (
                r#"{"snapshot_type":"Full","snapshot_path":"vmstate","mem_file_path":"memory"}"#,
                SnapshotType::Full,
            ),
            (
                r#"{"snapshot_type":"Diff","snapshot_path":"vmstate","mem_file_path":"memory"}"#,
                SnapshotType::Diff,
            ),
        ] {
            let request = request_with_body("PUT", "/snapshot/create", body);

            assert_eq!(
                parse_request(&request),
                Ok(ApiRequest::PutSnapshotCreate(SnapshotCreateRequest::new(
                    expected_type,
                    "vmstate".to_string(),
                    "memory".to_string(),
                ))),
                "{body}"
            );
        }
    }

    #[test]
    fn rejects_invalid_snapshot_create_before_unsupported() {
        assert_eq!(
            parse_request(&request_with_body("PUT", "/snapshot/create", "")),
            Err(RequestError::EmptyPutRequest)
        );

        for body in [
            "not-json",
            "null",
            "[]",
            "{}",
            r#"{"snapshot_path":"vmstate"}"#,
            r#"{"mem_file_path":"memory"}"#,
            r#"{"snapshot_type":"Incremental","snapshot_path":"vmstate","mem_file_path":"memory"}"#,
            r#"{"snapshot_type":true,"snapshot_path":"vmstate","mem_file_path":"memory"}"#,
            r#"{"snapshot_path":42,"mem_file_path":"memory"}"#,
            r#"{"snapshot_path":"vmstate","mem_file_path":42}"#,
            r#"{"snapshot_path":"vmstate","mem_file_path":"memory","unknown":true}"#,
        ] {
            let request = request_with_body("PUT", "/snapshot/create", body);

            assert_eq!(
                parse_request(&request),
                Err(RequestError::MalformedRequest),
                "{body}"
            );
        }
    }

    #[test]
    fn parses_valid_snapshot_load_requests() {
        for (
            body,
            backend_type,
            deprecated_fields_used,
            track_dirty_pages,
            resume_vm,
            clock_realtime,
            expected_network_override,
            expected_vsock_path,
        ) in [
            (
                r#"{"snapshot_path":"vmstate","mem_backend":{"backend_path":"memory","backend_type":"File"}}"#,
                SnapshotMemoryBackendType::File,
                false,
                false,
                false,
                false,
                None,
                None,
            ),
            (
                r#"{"snapshot_path":"vmstate","mem_backend":{"backend_path":"memory","backend_type":"Uffd"},"enable_diff_snapshots":true,"track_dirty_pages":true,"resume_vm":true,"clock_realtime":true}"#,
                SnapshotMemoryBackendType::Uffd,
                true,
                true,
                true,
                true,
                None,
                None,
            ),
            (
                r#"{"snapshot_path":"vmstate","mem_backend":{"backend_path":"memory","backend_type":"File"},"enable_diff_snapshots":false}"#,
                SnapshotMemoryBackendType::File,
                false,
                false,
                false,
                false,
                None,
                None,
            ),
            (
                r#"{"snapshot_path":"vmstate","mem_file_path":"memory","resume_vm":true}"#,
                SnapshotMemoryBackendType::File,
                true,
                false,
                true,
                false,
                None,
                None,
            ),
            (
                r#"{"snapshot_path":"vmstate","mem_backend":{"backend_path":"memory","backend_type":"File"},"network_overrides":[{"iface_id":"eth0","host_dev_name":"tap0"}],"vsock_override":{"uds_path":"/tmp/v.sock"}}"#,
                SnapshotMemoryBackendType::File,
                false,
                false,
                false,
                false,
                Some(("eth0", "tap0")),
                Some("/tmp/v.sock"),
            ),
            (
                r#"{"snapshot_path":"vmstate","mem_backend":{"backend_path":"memory","backend_type":"File"},"network_overrides":[{"iface_id":"eth0","host_dev_name":"tap0","unknown":true}],"vsock_override":{"uds_path":"/tmp/v.sock","unknown":true}}"#,
                SnapshotMemoryBackendType::File,
                false,
                false,
                false,
                false,
                Some(("eth0", "tap0")),
                Some("/tmp/v.sock"),
            ),
        ] {
            let request = request_with_body("PUT", "/snapshot/load", body);
            let parsed = parse_request(&request)
                .unwrap_or_else(|err| panic!("valid snapshot load should parse: {body}: {err}"));
            let ApiRequest::PutSnapshotLoad(parsed) = parsed else {
                panic!("expected snapshot load request: {body}");
            };

            assert_eq!(parsed.snapshot_path(), "vmstate", "{body}");
            assert_eq!(parsed.mem_backend().backend_path(), "memory", "{body}");
            assert_eq!(parsed.mem_backend().backend_type(), backend_type, "{body}");
            assert_eq!(
                parsed.deprecated_fields_used(),
                deprecated_fields_used,
                "{body}"
            );
            assert_eq!(parsed.track_dirty_pages(), track_dirty_pages, "{body}");
            assert_eq!(parsed.resume_vm(), resume_vm, "{body}");
            assert_eq!(parsed.clock_realtime(), clock_realtime, "{body}");
            assert_eq!(
                parsed
                    .network_overrides()
                    .first()
                    .map(|value| (value.iface_id(), value.host_dev_name())),
                expected_network_override,
                "{body}"
            );
            assert_eq!(
                parsed.vsock_override().map(SnapshotVsockOverride::uds_path),
                expected_vsock_path,
                "{body}"
            );
        }
    }

    #[test]
    fn normalizes_snapshot_load_dirty_and_deprecated_fields() {
        for (body, expected_dirty, expected_deprecated) in [
            (
                r#"{"snapshot_path":"state","mem_backend":{"backend_path":"memory","backend_type":"File"},"enable_diff_snapshots":true}"#,
                true,
                true,
            ),
            (
                r#"{"snapshot_path":"state","mem_backend":{"backend_path":"memory","backend_type":"File"},"track_dirty_pages":true}"#,
                true,
                false,
            ),
            (
                r#"{"snapshot_path":"state","mem_backend":{"backend_path":"memory","backend_type":"File"},"enable_diff_snapshots":false,"track_dirty_pages":false}"#,
                false,
                false,
            ),
        ] {
            let parsed = parse_request(&request_with_body("PUT", "/snapshot/load", body))
                .expect("snapshot load should parse");
            let ApiRequest::PutSnapshotLoad(parsed) = parsed else {
                panic!("expected snapshot load request");
            };

            assert_eq!(parsed.track_dirty_pages(), expected_dirty, "{body}");
            assert_eq!(
                parsed.deprecated_fields_used(),
                expected_deprecated,
                "{body}"
            );
        }
    }

    #[test]
    fn snapshot_request_debug_redacts_paths_and_override_values() {
        let create = parse_request(&request_with_body(
            "PUT",
            "/snapshot/create",
            r#"{"snapshot_path":"private-create-state","mem_file_path":"private-create-memory"}"#,
        ))
        .expect("snapshot create should parse");
        let load = parse_request(&request_with_body(
            "PUT",
            "/snapshot/load",
            r#"{"snapshot_path":"private-load-state","mem_backend":{"backend_path":"private-load-memory","backend_type":"File"},"network_overrides":[{"iface_id":"private-iface","host_dev_name":"private-host-device"}],"vsock_override":{"uds_path":"private-vsock"}}"#,
        ))
        .expect("snapshot load should parse");
        let debug = format!("{create:?} {load:?}");

        for private in [
            "private-create-state",
            "private-create-memory",
            "private-load-state",
            "private-load-memory",
            "private-iface",
            "private-host-device",
            "private-vsock",
        ] {
            assert!(!debug.contains(private), "request Debug leaked {private:?}");
        }
        assert!(debug.contains(SNAPSHOT_VALUE_REDACTED));
    }

    #[test]
    fn rejects_invalid_snapshot_load_before_unsupported() {
        assert_eq!(
            parse_request(&request_with_body("PUT", "/snapshot/load", "")),
            Err(RequestError::EmptyPutRequest)
        );

        for body in [
            "not-json",
            "null",
            "[]",
            "{}",
            r#"{"snapshot_path":"vmstate"}"#,
            r#"{"mem_backend":{"backend_path":"memory","backend_type":"File"}}"#,
            r#"{"snapshot_path":"vmstate","mem_file_path":"memory","mem_backend":{"backend_path":"memory","backend_type":"File"}}"#,
            r#"{"snapshot_path":"vmstate","mem_backend":null}"#,
            r#"{"snapshot_path":"vmstate","mem_file_path":42}"#,
            r#"{"snapshot_path":42,"mem_file_path":"memory"}"#,
            r#"{"snapshot_path":"vmstate","mem_file_path":"memory","mem_file_path":"memory2"}"#,
            r#"{"snapshot_path":"vmstate","mem_backend":{"backend_path":"memory","backend_type":"File"},"mem_backend":{"backend_path":"memory2","backend_type":"File"}}"#,
            r#"{"snapshot_path":"vmstate","mem_backend":{"backend_type":"File"}}"#,
            r#"{"snapshot_path":"vmstate","mem_backend":{"backend_path":"memory"}}"#,
            r#"{"snapshot_path":"vmstate","mem_backend":{"backend_path":"memory","backend_type":"Shared"}}"#,
            r#"{"snapshot_path":"vmstate","mem_backend":{"backend_path":"memory","backend_path":"memory2","backend_type":"File"}}"#,
            r#"{"snapshot_path":"vmstate","mem_backend":{"backend_path":"memory","backend_type":"File","unknown":true}}"#,
            r#"{"snapshot_path":"vmstate","mem_backend":{"backend_path":"memory","backend_type":"File"},"enable_diff_snapshots":false,"enable_diff_snapshots":true}"#,
            r#"{"snapshot_path":"vmstate","mem_backend":{"backend_path":"memory","backend_type":"File"},"enable_diff_snapshots":"true"}"#,
            r#"{"snapshot_path":"vmstate","mem_backend":{"backend_path":"memory","backend_type":"File"},"track_dirty_pages":"true"}"#,
            r#"{"snapshot_path":"vmstate","mem_backend":{"backend_path":"memory","backend_type":"File"},"resume_vm":"true"}"#,
            r#"{"snapshot_path":"vmstate","mem_backend":{"backend_path":"memory","backend_type":"File"},"clock_realtime":"true"}"#,
            r#"{"snapshot_path":"vmstate","mem_backend":{"backend_path":"memory","backend_type":"File"},"network_overrides":"eth0"}"#,
            r#"{"snapshot_path":"vmstate","mem_backend":{"backend_path":"memory","backend_type":"File"},"network_overrides":[{"iface_id":"eth0"}]}"#,
            r#"{"snapshot_path":"vmstate","mem_backend":{"backend_path":"memory","backend_type":"File"},"vsock_override":{}}"#,
            r#"{"snapshot_path":"vmstate","mem_backend":{"backend_path":"memory","backend_type":"File"},"unknown":true}"#,
        ] {
            let request = request_with_body("PUT", "/snapshot/load", body);

            assert_eq!(
                parse_request(&request),
                Err(RequestError::MalformedRequest),
                "{body}"
            );
        }
    }

    #[test]
    fn rejects_non_exact_snapshot_paths_as_invalid_path_method() {
        for (method, path) in [
            ("PUT", "/snapshot"),
            ("PUT", "/snapshot/create/extra"),
            ("PUT", "/snapshot/load/extra"),
            ("PATCH", "/snapshot/load"),
        ] {
            let request = request_with_body(method, path, "{}");

            assert_eq!(
                parse_request(&request),
                Err(RequestError::InvalidPathMethod),
                "{method} {path}"
            );
        }

        assert_eq!(
            parse_request(b"GET /snapshot/create HTTP/1.1\r\nHost: localhost\r\n\r\n"),
            Err(RequestError::InvalidPathMethod)
        );
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
    fn rejects_declared_content_length_over_custom_payload_limit() {
        let request = b"GET /version HTTP/1.1\r\nContent-Length: 10\r\n\r\n";

        assert_eq!(
            parse_request_with_limit(request, 9),
            Err(RequestError::PayloadTooLarge)
        );
        assert_eq!(
            request_total_len_with_limit(request, 9),
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
    fn rejects_incomplete_request_head_at_head_limit() {
        let mut request = b"GET /version HTTP/1.1\r\nX-Fill: ".to_vec();
        request.resize(HTTP_MAX_REQUEST_HEAD_SIZE, b'a');

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
        assert_eq!(
            request_total_len(&request),
            Err(RequestError::MalformedRequest)
        );
    }

    #[test]
    fn rejects_complete_request_head_over_head_limit() {
        let padding = "a".repeat(HTTP_MAX_REQUEST_HEAD_SIZE);
        let request = format!("GET /version HTTP/1.1\r\nX-Fill: {padding}\r\n\r\n");
        let header_len = request
            .find("\r\n\r\n")
            .map(|delimiter| delimiter + "\r\n\r\n".len())
            .expect("request should have a header terminator");

        assert!(header_len > HTTP_MAX_REQUEST_HEAD_SIZE);
        assert_eq!(
            parse_request(request.as_bytes()),
            Err(RequestError::MalformedRequest)
        );
        assert_eq!(
            request_total_len(request.as_bytes()),
            Err(RequestError::MalformedRequest)
        );
    }

    #[test]
    fn parses_request_above_default_with_custom_payload_limit() {
        let module = "a".repeat(HTTP_MAX_PAYLOAD_SIZE);
        let body = format!(r#"{{"module":"{module}"}}"#);
        let request = request_with_body("PUT", "/logger", &body);

        assert_eq!(parse_request(&request), Err(RequestError::PayloadTooLarge));
        assert_eq!(
            request_total_len_with_limit(&request, request.len()),
            Ok(Some(request.len()))
        );

        let parsed = parse_request_with_limit(&request, request.len())
            .expect("logger request above the default limit should parse");

        let ApiRequest::PutLogger(config) = parsed else {
            panic!("expected logger request");
        };
        assert_eq!(config.module(), Some(module.as_str()));
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
            None,
            None,
            None,
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
                "pmem": [],
            })
        );
        assert_eq!(body.get("boot-source"), None);
        assert_eq!(body.get("balloon"), None);
        assert_eq!(body.get("logger"), None);
        assert_eq!(body.get("mmds-config"), None);
        assert_eq!(body.get("entropy"), None);
        assert_eq!(body.get("memory-hotplug"), None);
        assert_eq!(body.get("pmem"), Some(&serde_json::json!([])));
        assert_eq!(body.get("vsock"), None);
    }

    #[test]
    fn response_body_contains_configured_vm_config() {
        let boot_source = BootSourceResponse::new("/tmp/vmlinux")
            .with_initrd_path("/tmp/initrd.img")
            .with_boot_args("console=hvc0 reboot=k panic=1");
        let drive =
            DriveConfigResponse::new("rootfs", "/tmp/rootfs.ext4", true, true, "Unsafe", "Sync")
                .with_partuuid("0eaa91a0-01")
                .with_rate_limiter(DriveRateLimiterRequest::new(
                    Some(TokenBucketRequest::new(1024, Some(2048), 100)),
                    Some(TokenBucketRequest::new(10, None, 1000)),
                ));
        let network_interface = NetworkInterfaceConfigResponse::new("eth0", "tap0")
            .with_guest_mac("12:34:56:78:9a:bc")
            .with_mtu(1500)
            .with_rx_rate_limiter(NetworkRateLimiterRequest::new(
                Some(TokenBucketRequest::new(4096, Some(8192), 100)),
                None,
            ))
            .with_tx_rate_limiter(NetworkRateLimiterRequest::new(
                None,
                Some(TokenBucketRequest::new(10, None, 1000)),
            ));
        let mmds_config = MmdsConfigResponse::new(vec!["eth0".to_string()], "V2", true)
            .with_ipv4_address("169.254.169.254");
        let vsock = VsockConfigResponse::new(3, "./v.sock");
        let memory_hotplug = MemoryHotplugConfigResponse::new(1024, 2, 128);
        let balloon = BalloonConfigResponse::new(128, true, 60, true, false);
        let pmem = PmemConfigResponse::new("pmem0", "/tmp/pmem.img", true, false);
        let response = HttpResponse::vm_config(
            &VmConfigResponse::new(
                MachineConfigResponse::new(2, 256, false, false, "None"),
                Some(boot_source),
                vec![drive],
                vec![network_interface],
                Some(mmds_config),
                Some(vsock),
                Some(EntropyConfigResponse::new()),
            )
            .with_memory_hotplug(Some(memory_hotplug))
            .with_balloon(Some(balloon))
            .with_pmem(vec![pmem]),
        );
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
                "balloon": {
                    "amount_mib": 128,
                    "deflate_on_oom": true,
                    "free_page_hinting": true,
                    "free_page_reporting": false,
                    "stats_polling_interval_s": 60,
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
                        "rate_limiter": {
                            "bandwidth": {
                                "one_time_burst": 2048,
                                "refill_time": 100,
                                "size": 1024,
                            },
                            "ops": {
                                "one_time_burst": null,
                                "refill_time": 1000,
                                "size": 10,
                            },
                        },
                    },
                ],
                "entropy": {},
                "machine-config": {
                    "huge_pages": "None",
                    "mem_size_mib": 256,
                    "smt": false,
                    "track_dirty_pages": false,
                    "vcpu_count": 2,
                },
                "memory-hotplug": {
                    "block_size_mib": 2,
                    "slot_size_mib": 128,
                    "total_size_mib": 1024,
                },
                "network-interfaces": [
                    {
                        "guest_mac": "12:34:56:78:9a:bc",
                        "host_dev_name": "tap0",
                        "iface_id": "eth0",
                        "mtu": 1500,
                        "rx_rate_limiter": {
                            "bandwidth": {
                                "one_time_burst": 8192,
                                "refill_time": 100,
                                "size": 4096,
                            },
                        },
                        "tx_rate_limiter": {
                            "ops": {
                                "one_time_burst": null,
                                "refill_time": 1000,
                                "size": 10,
                            },
                        },
                    },
                ],
                "pmem": [
                    {
                        "id": "pmem0",
                        "path_on_host": "/tmp/pmem.img",
                        "read_only": false,
                        "root_device": true,
                    },
                ],
                "mmds-config": {
                    "imds_compat": true,
                    "ipv4_address": "169.254.169.254",
                    "network_interfaces": ["eth0"],
                    "version": "V2",
                },
                "vsock": {
                    "guest_cid": 3,
                    "uds_path": "./v.sock",
                },
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
            None,
            Some(VsockConfigResponse::new(3, "./v.sock")),
            None,
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
        assert_eq!(network_interface.get("tx_rate_limiter"), None);
        assert_eq!(
            body.get("vsock"),
            Some(&serde_json::json!({
                "guest_cid": 3,
                "uds_path": "./v.sock",
            }))
        );
        assert_eq!(
            body.get("vsock").and_then(|vsock| vsock.get("vsock_id")),
            None
        );
    }

    #[test]
    fn response_body_contains_balloon_stats() {
        let response = HttpResponse::balloon_stats(BalloonStatsResponse::new(1024, 513, 4, 2));
        let body: serde_json::Value =
            serde_json::from_str(response.body()).expect("body should be JSON");

        assert_eq!(response.status(), StatusCode::Ok);
        assert_eq!(
            body,
            serde_json::json!({
                "actual_mib": 2,
                "actual_pages": 513,
                "target_mib": 4,
                "target_pages": 1024,
            })
        );
    }

    #[test]
    fn response_body_contains_recorded_balloon_optional_stats() {
        let response = HttpResponse::balloon_stats(
            BalloonStatsResponse::new(1024, 513, 4, 2)
                .with_swap_out(Some(9))
                .with_free_memory(Some(0x5678))
                .with_direct_reclaim(Some(42)),
        );
        let body: serde_json::Value =
            serde_json::from_str(response.body()).expect("body should be JSON");

        assert_eq!(response.status(), StatusCode::Ok);
        assert_eq!(
            body,
            serde_json::json!({
                "actual_mib": 2,
                "actual_pages": 513,
                "direct_reclaim": 42,
                "free_memory": 0x5678,
                "swap_out": 9,
                "target_mib": 4,
                "target_pages": 1024,
            })
        );
        assert_eq!(body.get("swap_in"), None);
    }

    #[test]
    fn response_body_contains_balloon_hinting_status() {
        let response =
            HttpResponse::balloon_hinting_status(BalloonHintingStatusResponse::new(7, None));
        let body: serde_json::Value =
            serde_json::from_str(response.body()).expect("body should be JSON");

        assert_eq!(response.status(), StatusCode::Ok);
        assert_eq!(
            body,
            serde_json::json!({
                "guest_cmd": null,
                "host_cmd": 7,
            })
        );
    }

    #[test]
    fn response_body_contains_memory_hotplug_status() {
        let response = HttpResponse::memory_hotplug_status(MemoryHotplugStatusResponse::new(
            1024, 2, 128, 0, 0,
        ));
        let body: serde_json::Value =
            serde_json::from_str(response.body()).expect("body should be JSON");

        assert_eq!(response.status(), StatusCode::Ok);
        assert_eq!(
            body,
            serde_json::json!({
                "block_size_mib": 2,
                "plugged_size_mib": 0,
                "requested_size_mib": 0,
                "slot_size_mib": 128,
                "total_size_mib": 1024,
            })
        );
    }

    #[test]
    fn response_body_contains_mmds_value() {
        let value = serde_json::json!({
            "latest": {
                "meta-data": {
                    "ami-id": "ami-123",
                },
            },
        });
        let response = HttpResponse::mmds(&value);
        let body: serde_json::Value =
            serde_json::from_str(response.body()).expect("body should be JSON");

        assert_eq!(response.status(), StatusCode::Ok);
        assert_eq!(body, value);
    }

    #[test]
    fn fault_body_contains_fault_message() {
        let response = HttpResponse::fault("message");

        assert_eq!(response.status(), StatusCode::BadRequest);
        assert_eq!(response.body(), r#"{"fault_message":"message"}"#);
    }

    #[test]
    fn payload_too_large_fault_uses_413_with_fault_message() {
        let response = HttpResponse::payload_too_large_fault();
        let bytes = response.to_http_bytes();
        let text = std::str::from_utf8(&bytes).expect("response should be utf-8");

        assert_eq!(response.status(), StatusCode::PayloadTooLarge);
        assert_eq!(
            response.body(),
            r#"{"fault_message":"HTTP request payload exceeds the configured limit."}"#
        );
        assert!(text.starts_with("HTTP/1.1 413 Payload Too Large\r\n"));
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
        let request = parse_request(&request_with_body("PATCH", "/vm", r#"{"state":"Paused"}"#))
            .expect("VM state update request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::VmState);

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

        let request = parse_request(&request_with_body("PUT", "/cpu-config", "{}"))
            .expect("cpu-config request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::CpuConfig);

        assert_eq!(Endpoint::from(ApiRequest::GetBalloon), Endpoint::Balloon);
        assert_eq!(
            Endpoint::from(ApiRequest::GetBalloonStats),
            Endpoint::Balloon
        );
        assert_eq!(
            Endpoint::from(ApiRequest::GetBalloonHintingStatus),
            Endpoint::Balloon
        );

        let request = parse_request(&request_with_body(
            "PUT",
            "/balloon",
            r#"{"amount_mib":64,"deflate_on_oom":true}"#,
        ))
        .expect("balloon request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::Balloon);

        let request = parse_request(&request_with_body("PATCH", "/balloon/hinting/start", "{}"))
            .expect("balloon hinting request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::Balloon);

        let request = parse_request(&request_with_body("PUT", "/entropy", "{}"))
            .expect("entropy request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::Entropy);

        assert_eq!(
            Endpoint::from(ApiRequest::GetMemoryHotplug),
            Endpoint::MemoryHotplug
        );

        let request = parse_request(&request_with_body(
            "PUT",
            "/hotplug/memory",
            r#"{"total_size_mib":2048}"#,
        ))
        .expect("memory hotplug request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::MemoryHotplug);

        let request = parse_request(&request_with_body(
            "PATCH",
            "/hotplug/memory",
            r#"{"requested_size_mib":1024}"#,
        ))
        .expect("memory hotplug patch request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::MemoryHotplug);

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
            "PATCH",
            "/drives/rootfs",
            r#"{"drive_id":"rootfs"}"#,
        ))
        .expect("drive patch request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::Drive);

        let request = parse_request(b"DELETE /drives/rootfs HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .expect("drive delete request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::Drive);

        let request = parse_request(&request_with_body(
            "PUT",
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":128}"#,
        ))
        .expect("machine-config request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::MachineConfig);

        let request = parse_request(&request_with_body(
            "PATCH",
            "/machine-config",
            r#"{"mem_size_mib":256}"#,
        ))
        .expect("machine-config patch request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::MachineConfig);

        let request = parse_request(&request_with_body(
            "PUT",
            "/metrics",
            r#"{"metrics_path":"/tmp/metrics"}"#,
        ))
        .expect("metrics request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::Metrics);

        assert_eq!(Endpoint::from(ApiRequest::GetMmds), Endpoint::Mmds);

        let request = parse_request(&request_with_body("PUT", "/mmds", "{}"))
            .expect("MMDS PUT request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::Mmds);

        let request = parse_request(&request_with_body("PATCH", "/mmds", "{}"))
            .expect("MMDS PATCH request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::Mmds);

        let request = parse_request(&request_with_body(
            "PUT",
            "/mmds/config",
            r#"{"network_interfaces":["eth0"]}"#,
        ))
        .expect("MMDS config request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::Mmds);

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

        let request = parse_request(&request_with_body(
            "PATCH",
            "/network-interfaces/eth0",
            r#"{"iface_id":"eth0"}"#,
        ))
        .expect("network interface patch request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::NetworkInterface);

        let request =
            parse_request(b"DELETE /network-interfaces/eth0 HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .expect("network interface delete request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::NetworkInterface);

        let request = parse_request(&request_with_body(
            "PUT",
            "/pmem/pmem0",
            r#"{"id":"pmem0","path_on_host":"/tmp/pmem.img"}"#,
        ))
        .expect("pmem request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::Pmem);

        let request = parse_request(&request_with_body(
            "PATCH",
            "/pmem/pmem0",
            r#"{"id":"pmem0"}"#,
        ))
        .expect("pmem patch request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::Pmem);

        let request = parse_request(b"DELETE /pmem/pmem0 HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .expect("pmem delete request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::Pmem);

        let request = parse_request(&request_with_body(
            "PUT",
            "/vsock",
            r#"{"guest_cid":3,"uds_path":"./v.sock"}"#,
        ))
        .expect("vsock request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::Vsock);

        let request = parse_request(&request_with_body(
            "PUT",
            "/snapshot/create",
            r#"{"snapshot_path":"vmstate","mem_file_path":"memory"}"#,
        ))
        .expect("snapshot create request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::Snapshot);

        let request = parse_request(&request_with_body(
            "PUT",
            "/snapshot/load",
            r#"{"snapshot_path":"vmstate","mem_backend":{"backend_path":"memory","backend_type":"File"}}"#,
        ))
        .expect("snapshot load request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::Snapshot);
    }
}
