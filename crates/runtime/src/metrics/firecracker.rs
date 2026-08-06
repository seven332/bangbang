use std::fmt;
use std::io::{self, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};

use crate::block::DriveConfig;
use crate::network::{MAX_NETWORK_INTERFACE_COUNT, NetworkInterfaceConfig};

pub(crate) const FIRECRACKER_METRICS_MAX_DYNAMIC_ROOTS: usize = 985;
pub(crate) const FIRECRACKER_METRICS_MAX_IDENTITY_BYTES: usize =
    1024 * 1024 + (FIRECRACKER_METRICS_MAX_DYNAMIC_ROOTS - 1) * 51_200;
pub(crate) const FIRECRACKER_METRICS_MAX_LINE_BYTES: usize = 64 * 1024 * 1024;
const FIRECRACKER_METRICS_MAX_JSON_BYTES: usize = FIRECRACKER_METRICS_MAX_LINE_BYTES - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MetricsLineBuildError {
    Clock,
    TooManyConfiguredDevices,
    TooManyNetworkInterfaces,
    ConfiguredIdentityBytes,
    DuplicateConfiguredIdentity,
    Allocation,
    Serialization,
    LineTooLong,
}

pub(super) trait MetricsClock: fmt::Debug + Send {
    fn now(&self) -> SystemTime;
}

pub(super) trait MetricsLineSerializer: fmt::Debug + Send {
    fn serialize(&self, line: &FirecrackerMetricsLine) -> Result<Vec<u8>, MetricsLineBuildError>;
}

#[derive(Debug, Default)]
pub(super) struct SystemMetricsClock;

impl MetricsClock for SystemMetricsClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

#[derive(Debug, Default)]
pub(super) struct SystemMetricsLineSerializer;

impl MetricsLineSerializer for SystemMetricsLineSerializer {
    fn serialize(&self, line: &FirecrackerMetricsLine) -> Result<Vec<u8>, MetricsLineBuildError> {
        serialize_metrics_line(line)
    }
}

pub(super) fn unix_timestamp_ms(clock: &dyn MetricsClock) -> Result<u64, MetricsLineBuildError> {
    let duration = clock
        .now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| MetricsLineBuildError::Clock)?;
    u64::try_from(duration.as_millis()).map_err(|_| MetricsLineBuildError::Clock)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ConfiguredMetricsDevices {
    ordinary_block_ids: Vec<String>,
    vhost_user_block_ids: Vec<String>,
    network_ids: Vec<String>,
}

impl ConfiguredMetricsDevices {
    pub(super) fn try_from_configs(
        drives: &[DriveConfig],
        networks: &[NetworkInterfaceConfig],
    ) -> Result<Self, MetricsLineBuildError> {
        let dynamic_count = drives
            .len()
            .checked_add(networks.len())
            .ok_or(MetricsLineBuildError::TooManyConfiguredDevices)?;

        let identity_bytes = drives
            .iter()
            .map(|drive| drive.drive_id().len())
            .chain(networks.iter().map(|network| network.iface_id().len()))
            .try_fold(0_usize, |total, length| total.checked_add(length))
            .ok_or(MetricsLineBuildError::ConfiguredIdentityBytes)?;
        validate_configured_metrics_bounds(dynamic_count, networks.len(), identity_bytes)?;

        let ordinary_count = drives.iter().filter(|drive| !drive.is_vhost_user()).count();
        let vhost_user_count = drives.len() - ordinary_count;
        let mut ordinary_block_ids = Vec::new();
        ordinary_block_ids
            .try_reserve_exact(ordinary_count)
            .map_err(|_| MetricsLineBuildError::Allocation)?;
        let mut vhost_user_block_ids = Vec::new();
        vhost_user_block_ids
            .try_reserve_exact(vhost_user_count)
            .map_err(|_| MetricsLineBuildError::Allocation)?;
        let mut network_ids = Vec::new();
        network_ids
            .try_reserve_exact(networks.len())
            .map_err(|_| MetricsLineBuildError::Allocation)?;

        for drive in drives {
            let copied = try_copy_identity(drive.drive_id())?;
            if drive.is_vhost_user() {
                vhost_user_block_ids.push(copied);
            } else {
                ordinary_block_ids.push(copied);
            }
        }
        for network in networks {
            network_ids.push(try_copy_identity(network.iface_id())?);
        }

        ordinary_block_ids.sort_unstable();
        vhost_user_block_ids.sort_unstable();
        network_ids.sort_unstable();
        if contains_duplicate(&ordinary_block_ids)
            || contains_duplicate(&vhost_user_block_ids)
            || contains_duplicate(&network_ids)
            || ordinary_block_ids
                .iter()
                .any(|id| vhost_user_block_ids.binary_search(id).is_ok())
        {
            return Err(MetricsLineBuildError::DuplicateConfiguredIdentity);
        }

        Ok(Self {
            ordinary_block_ids,
            vhost_user_block_ids,
            network_ids,
        })
    }

    pub(super) fn ordinary_block_ids(&self) -> &[String] {
        &self.ordinary_block_ids
    }

    pub(super) fn vhost_user_block_ids(&self) -> &[String] {
        &self.vhost_user_block_ids
    }

    pub(super) fn network_ids(&self) -> &[String] {
        &self.network_ids
    }

    #[cfg(test)]
    pub(super) fn from_test_ids(
        ordinary_block_ids: &[&str],
        vhost_user_block_ids: &[&str],
        network_ids: &[&str],
    ) -> Self {
        let mut configured = Self {
            ordinary_block_ids: ordinary_block_ids
                .iter()
                .map(|id| (*id).to_owned())
                .collect(),
            vhost_user_block_ids: vhost_user_block_ids
                .iter()
                .map(|id| (*id).to_owned())
                .collect(),
            network_ids: network_ids.iter().map(|id| (*id).to_owned()).collect(),
        };
        configured.ordinary_block_ids.sort_unstable();
        configured.vhost_user_block_ids.sort_unstable();
        configured.network_ids.sort_unstable();
        configured
    }
}

fn validate_configured_metrics_bounds(
    dynamic_count: usize,
    network_count: usize,
    identity_bytes: usize,
) -> Result<(), MetricsLineBuildError> {
    if network_count > MAX_NETWORK_INTERFACE_COUNT {
        return Err(MetricsLineBuildError::TooManyNetworkInterfaces);
    }
    if dynamic_count > FIRECRACKER_METRICS_MAX_DYNAMIC_ROOTS {
        return Err(MetricsLineBuildError::TooManyConfiguredDevices);
    }
    if identity_bytes > FIRECRACKER_METRICS_MAX_IDENTITY_BYTES {
        return Err(MetricsLineBuildError::ConfiguredIdentityBytes);
    }
    Ok(())
}

fn try_copy_identity(value: &str) -> Result<String, MetricsLineBuildError> {
    let mut copied = String::new();
    copied
        .try_reserve_exact(value.len())
        .map_err(|_| MetricsLineBuildError::Allocation)?;
    copied.push_str(value);
    Ok(copied)
}

fn contains_duplicate(values: &[String]) -> bool {
    values
        .windows(2)
        .any(|pair| matches!(pair, [left, right] if left == right))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
struct LatencyAggregate {
    min_us: u64,
    max_us: u64,
    sum_us: u64,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MetricFieldKind {
    Incremental,
    Store,
    Latency,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MetricFieldDeclaration {
    name: &'static str,
    kind: MetricFieldKind,
}

#[cfg(test)]
trait MetricFamilySchema {
    const ROOT_TEMPLATE: &'static str;
    const FIELDS: &'static [MetricFieldDeclaration];
}

#[cfg(test)]
trait MaximumMetricValue {
    fn maximum_metric_value() -> Self;
}

#[cfg(test)]
impl MaximumMetricValue for u64 {
    fn maximum_metric_value() -> Self {
        u64::MAX
    }
}

#[cfg(test)]
impl MaximumMetricValue for LatencyAggregate {
    fn maximum_metric_value() -> Self {
        Self {
            min_us: u64::MAX,
            max_us: u64::MAX,
            sum_us: u64::MAX,
        }
    }
}

macro_rules! define_metrics_family {
    ($name:ident, $root:literal, { $($field:ident: $field_type:ty => $kind:ident),+ $(,)? }) => {
        #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
        struct $name {
            $($field: $field_type,)+
        }

        #[cfg(test)]
        impl MetricFamilySchema for $name {
            const ROOT_TEMPLATE: &'static str = $root;
            const FIELDS: &'static [MetricFieldDeclaration] = &[
                $(MetricFieldDeclaration {
                    name: stringify!($field),
                    kind: MetricFieldKind::$kind,
                },)+
            ];
        }

        #[cfg(test)]
        impl $name {
            fn maximum_for_test() -> Self {
                Self {
                    $($field: <$field_type as MaximumMetricValue>::maximum_metric_value(),)+
                }
            }
        }
    };
}

define_metrics_family!(ApiServerMetrics, "api_server", {
    process_startup_time_us: u64 => Store,
    process_startup_time_cpu_us: u64 => Store,
});

define_metrics_family!(BalloonMetrics, "balloon", {
    activate_fails: u64 => Incremental,
    inflate_count: u64 => Incremental,
    stats_updates_count: u64 => Incremental,
    stats_update_fails: u64 => Incremental,
    deflate_count: u64 => Incremental,
    event_fails: u64 => Incremental,
    free_page_report_count: u64 => Incremental,
    free_page_report_freed: u64 => Incremental,
    free_page_report_fails: u64 => Incremental,
    free_page_hint_count: u64 => Incremental,
    free_page_hint_freed: u64 => Incremental,
    free_page_hint_fails: u64 => Incremental,
});

define_metrics_family!(BlockMetrics, "block", {
    activate_fails: u64 => Incremental,
    cfg_fails: u64 => Incremental,
    no_avail_buffer: u64 => Incremental,
    event_fails: u64 => Incremental,
    execute_fails: u64 => Incremental,
    invalid_reqs_count: u64 => Incremental,
    flush_count: u64 => Incremental,
    queue_event_count: u64 => Incremental,
    rate_limiter_event_count: u64 => Incremental,
    update_count: u64 => Incremental,
    update_fails: u64 => Incremental,
    read_bytes: u64 => Incremental,
    write_bytes: u64 => Incremental,
    read_count: u64 => Incremental,
    write_count: u64 => Incremental,
    read_agg: LatencyAggregate => Latency,
    write_agg: LatencyAggregate => Latency,
    rate_limiter_throttled_events: u64 => Incremental,
    io_engine_throttled_events: u64 => Incremental,
    remaining_reqs_count: u64 => Incremental,
});

define_metrics_family!(DeprecatedApiMetrics, "deprecated_api", {
    deprecated_http_api_calls: u64 => Incremental,
});

define_metrics_family!(GetApiRequestMetrics, "get_api_requests", {
    instance_info_count: u64 => Incremental,
    machine_cfg_count: u64 => Incremental,
    mmds_count: u64 => Incremental,
    vmm_version_count: u64 => Incremental,
    hotplug_memory_count: u64 => Incremental,
});

define_metrics_family!(I8042Metrics, "i8042", {
    error_count: u64 => Incremental,
    missed_read_count: u64 => Incremental,
    missed_write_count: u64 => Incremental,
    read_count: u64 => Incremental,
    reset_count: u64 => Incremental,
    write_count: u64 => Incremental,
});

define_metrics_family!(RtcMetrics, "rtc", {
    error_count: u64 => Incremental,
    missed_read_count: u64 => Incremental,
    missed_write_count: u64 => Incremental,
});

define_metrics_family!(UartMetrics, "uart", {
    error_count: u64 => Incremental,
    flush_count: u64 => Incremental,
    missed_read_count: u64 => Incremental,
    missed_write_count: u64 => Incremental,
    read_count: u64 => Incremental,
    write_count: u64 => Incremental,
    rate_limiter_dropped_bytes: u64 => Incremental,
});

define_metrics_family!(LatencyMetrics, "latencies_us", {
    full_create_snapshot: u64 => Store,
    diff_create_snapshot: u64 => Store,
    load_snapshot: u64 => Store,
    pause_vm: u64 => Store,
    resume_vm: u64 => Store,
    vmm_full_create_snapshot: u64 => Store,
    vmm_diff_create_snapshot: u64 => Store,
    vmm_load_snapshot: u64 => Store,
    vmm_pause_vm: u64 => Store,
    vmm_resume_vm: u64 => Store,
});

define_metrics_family!(LoggerMetrics, "logger", {
    missed_metrics_count: u64 => Incremental,
    metrics_fails: u64 => Incremental,
    missed_log_count: u64 => Incremental,
    rate_limited_log_count: u64 => Incremental,
});

define_metrics_family!(MmdsMetrics, "mmds", {
    rx_accepted: u64 => Incremental,
    rx_accepted_err: u64 => Incremental,
    rx_accepted_unusual: u64 => Incremental,
    rx_bad_eth: u64 => Incremental,
    rx_invalid_token: u64 => Incremental,
    rx_no_token: u64 => Incremental,
    rx_count: u64 => Incremental,
    tx_bytes: u64 => Incremental,
    tx_count: u64 => Incremental,
    tx_errors: u64 => Incremental,
    tx_frames: u64 => Incremental,
    connections_created: u64 => Incremental,
    connections_destroyed: u64 => Incremental,
});

define_metrics_family!(NetworkMetrics, "net", {
    activate_fails: u64 => Incremental,
    cfg_fails: u64 => Incremental,
    mac_address_updates: u64 => Incremental,
    no_rx_avail_buffer: u64 => Incremental,
    no_tx_avail_buffer: u64 => Incremental,
    event_fails: u64 => Incremental,
    rx_queue_event_count: u64 => Incremental,
    rx_event_rate_limiter_count: u64 => Incremental,
    rx_rate_limiter_throttled: u64 => Incremental,
    rx_tap_event_count: u64 => Incremental,
    rx_bytes_count: u64 => Incremental,
    rx_packets_count: u64 => Incremental,
    rx_fails: u64 => Incremental,
    rx_count: u64 => Incremental,
    tap_read_fails: u64 => Incremental,
    tap_write_fails: u64 => Incremental,
    tap_write_agg: LatencyAggregate => Latency,
    tx_bytes_count: u64 => Incremental,
    tx_malformed_frames: u64 => Incremental,
    tx_fails: u64 => Incremental,
    tx_count: u64 => Incremental,
    tx_packets_count: u64 => Incremental,
    tx_queue_event_count: u64 => Incremental,
    tx_rate_limiter_event_count: u64 => Incremental,
    tx_rate_limiter_throttled: u64 => Incremental,
    tx_spoofed_mac_count: u64 => Incremental,
    tx_remaining_reqs_count: u64 => Incremental,
});

define_metrics_family!(PatchApiRequestMetrics, "patch_api_requests", {
    drive_count: u64 => Incremental,
    drive_fails: u64 => Incremental,
    network_count: u64 => Incremental,
    network_fails: u64 => Incremental,
    machine_cfg_count: u64 => Incremental,
    machine_cfg_fails: u64 => Incremental,
    mmds_count: u64 => Incremental,
    mmds_fails: u64 => Incremental,
    hotplug_memory_count: u64 => Incremental,
    hotplug_memory_fails: u64 => Incremental,
    pmem_count: u64 => Incremental,
    pmem_fails: u64 => Incremental,
});

define_metrics_family!(PutApiRequestMetrics, "put_api_requests", {
    actions_count: u64 => Incremental,
    actions_fails: u64 => Incremental,
    boot_source_count: u64 => Incremental,
    boot_source_fails: u64 => Incremental,
    drive_count: u64 => Incremental,
    drive_fails: u64 => Incremental,
    logger_count: u64 => Incremental,
    logger_fails: u64 => Incremental,
    machine_cfg_count: u64 => Incremental,
    machine_cfg_fails: u64 => Incremental,
    cpu_cfg_count: u64 => Incremental,
    cpu_cfg_fails: u64 => Incremental,
    metrics_count: u64 => Incremental,
    metrics_fails: u64 => Incremental,
    network_count: u64 => Incremental,
    network_fails: u64 => Incremental,
    mmds_count: u64 => Incremental,
    mmds_fails: u64 => Incremental,
    vsock_count: u64 => Incremental,
    vsock_fails: u64 => Incremental,
    pmem_count: u64 => Incremental,
    pmem_fails: u64 => Incremental,
    serial_count: u64 => Incremental,
    serial_fails: u64 => Incremental,
    hotplug_memory_count: u64 => Incremental,
    hotplug_memory_fails: u64 => Incremental,
});

define_metrics_family!(SeccompMetrics, "seccomp", {
    num_faults: u64 => Store,
});

define_metrics_family!(VcpuMetrics, "vcpu", {
    exit_io_in: u64 => Incremental,
    exit_io_out: u64 => Incremental,
    exit_mmio_read: u64 => Incremental,
    exit_mmio_write: u64 => Incremental,
    failures: u64 => Incremental,
    kvmclock_ctrl_fails: u64 => Incremental,
    exit_io_in_agg: LatencyAggregate => Latency,
    exit_io_out_agg: LatencyAggregate => Latency,
    exit_mmio_read_agg: LatencyAggregate => Latency,
    exit_mmio_write_agg: LatencyAggregate => Latency,
});

define_metrics_family!(VmmMetrics, "vmm", {
    panic_count: u64 => Store,
});

define_metrics_family!(SignalMetrics, "signals", {
    sigbus: u64 => Store,
    sigsegv: u64 => Store,
    sigxfsz: u64 => Store,
    sigxcpu: u64 => Store,
    sigpipe: u64 => Incremental,
    sighup: u64 => Store,
    sigill: u64 => Store,
});

define_metrics_family!(VsockMetrics, "vsock", {
    activate_fails: u64 => Incremental,
    cfg_fails: u64 => Incremental,
    rx_queue_event_fails: u64 => Incremental,
    tx_queue_event_fails: u64 => Incremental,
    ev_queue_event_fails: u64 => Incremental,
    muxer_event_fails: u64 => Incremental,
    conn_event_fails: u64 => Incremental,
    rx_queue_event_count: u64 => Incremental,
    tx_queue_event_count: u64 => Incremental,
    rx_bytes_count: u64 => Incremental,
    tx_bytes_count: u64 => Incremental,
    rx_packets_count: u64 => Incremental,
    tx_packets_count: u64 => Incremental,
    conns_added: u64 => Incremental,
    conns_killed: u64 => Incremental,
    conns_removed: u64 => Incremental,
    killq_resync: u64 => Incremental,
    tx_flush_fails: u64 => Incremental,
    tx_write_fails: u64 => Incremental,
    rx_read_fails: u64 => Incremental,
});

define_metrics_family!(EntropyMetrics, "entropy", {
    activate_fails: u64 => Incremental,
    entropy_event_fails: u64 => Incremental,
    entropy_event_count: u64 => Incremental,
    entropy_bytes: u64 => Incremental,
    host_rng_fails: u64 => Incremental,
    entropy_rate_limiter_throttled: u64 => Incremental,
    rate_limiter_event_count: u64 => Incremental,
});

define_metrics_family!(PmemMetrics, "pmem", {
    activate_fails: u64 => Incremental,
    cfg_fails: u64 => Incremental,
    event_fails: u64 => Incremental,
    queue_event_count: u64 => Incremental,
    rate_limiter_throttled_events: u64 => Incremental,
    rate_limiter_event_count: u64 => Incremental,
});

define_metrics_family!(InterruptMetrics, "interrupts", {
    triggers: u64 => Incremental,
    config_updates: u64 => Incremental,
});

define_metrics_family!(MemoryHotplugMetrics, "memory_hotplug", {
    activate_fails: u64 => Incremental,
    queue_event_fails: u64 => Incremental,
    queue_event_count: u64 => Incremental,
    plug_agg: LatencyAggregate => Latency,
    plug_count: u64 => Incremental,
    plug_bytes: u64 => Incremental,
    plug_fails: u64 => Incremental,
    unplug_agg: LatencyAggregate => Latency,
    unplug_count: u64 => Incremental,
    unplug_bytes: u64 => Incremental,
    unplug_fails: u64 => Incremental,
    unplug_discard_fails: u64 => Incremental,
    unplug_all_agg: LatencyAggregate => Latency,
    unplug_all_count: u64 => Incremental,
    unplug_all_fails: u64 => Incremental,
    state_agg: LatencyAggregate => Latency,
    state_count: u64 => Incremental,
    state_fails: u64 => Incremental,
});

define_metrics_family!(VhostUserBlockMetrics, "vhost_user_block_{drive_id}", {
    activate_fails: u64 => Incremental,
    cfg_fails: u64 => Incremental,
    init_time_us: u64 => Store,
    activate_time_us: u64 => Store,
    config_change_time_us: u64 => Store,
});

#[derive(Debug, Clone, PartialEq, Eq)]
struct DynamicMetrics<T> {
    root: String,
    values: T,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct FirecrackerMetricsLine {
    utc_timestamp_ms: u64,
    api_server: ApiServerMetrics,
    balloon: BalloonMetrics,
    block_devices: Vec<DynamicMetrics<BlockMetrics>>,
    block: BlockMetrics,
    deprecated_api: DeprecatedApiMetrics,
    get_api_requests: GetApiRequestMetrics,
    i8042: I8042Metrics,
    rtc: RtcMetrics,
    uart: UartMetrics,
    latencies_us: LatencyMetrics,
    logger: LoggerMetrics,
    mmds: MmdsMetrics,
    network_devices: Vec<DynamicMetrics<NetworkMetrics>>,
    net: NetworkMetrics,
    patch_api_requests: PatchApiRequestMetrics,
    put_api_requests: PutApiRequestMetrics,
    seccomp: SeccompMetrics,
    vcpu: VcpuMetrics,
    vmm: VmmMetrics,
    signals: SignalMetrics,
    vsock: VsockMetrics,
    entropy: EntropyMetrics,
    pmem: PmemMetrics,
    vhost_user_block_devices: Vec<DynamicMetrics<VhostUserBlockMetrics>>,
    interrupts: InterruptMetrics,
    memory_hotplug: MemoryHotplugMetrics,
}

impl Serialize for FirecrackerMetricsLine {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let dynamic_count = self
            .block_devices
            .len()
            .saturating_add(self.network_devices.len())
            .saturating_add(self.vhost_user_block_devices.len());
        let mut root = serializer.serialize_map(Some(24_usize.saturating_add(dynamic_count)))?;
        root.serialize_entry("utc_timestamp_ms", &self.utc_timestamp_ms)?;
        root.serialize_entry("api_server", &self.api_server)?;
        root.serialize_entry("balloon", &self.balloon)?;
        for metrics in &self.block_devices {
            root.serialize_entry(&metrics.root, &metrics.values)?;
        }
        root.serialize_entry("block", &self.block)?;
        root.serialize_entry("deprecated_api", &self.deprecated_api)?;
        root.serialize_entry("get_api_requests", &self.get_api_requests)?;
        root.serialize_entry("i8042", &self.i8042)?;
        root.serialize_entry("rtc", &self.rtc)?;
        root.serialize_entry("uart", &self.uart)?;
        root.serialize_entry("latencies_us", &self.latencies_us)?;
        root.serialize_entry("logger", &self.logger)?;
        root.serialize_entry("mmds", &self.mmds)?;
        for metrics in &self.network_devices {
            root.serialize_entry(&metrics.root, &metrics.values)?;
        }
        root.serialize_entry("net", &self.net)?;
        root.serialize_entry("patch_api_requests", &self.patch_api_requests)?;
        root.serialize_entry("put_api_requests", &self.put_api_requests)?;
        root.serialize_entry("seccomp", &self.seccomp)?;
        root.serialize_entry("vcpu", &self.vcpu)?;
        root.serialize_entry("vmm", &self.vmm)?;
        root.serialize_entry("signals", &self.signals)?;
        root.serialize_entry("vsock", &self.vsock)?;
        root.serialize_entry("entropy", &self.entropy)?;
        root.serialize_entry("pmem", &self.pmem)?;
        for metrics in &self.vhost_user_block_devices {
            root.serialize_entry(&metrics.root, &metrics.values)?;
        }
        root.serialize_entry("interrupts", &self.interrupts)?;
        root.serialize_entry("memory_hotplug", &self.memory_hotplug)?;
        root.end()
    }
}

#[cfg(test)]
impl FirecrackerMetricsLine {
    fn maximum_static_for_test() -> Self {
        Self {
            utc_timestamp_ms: u64::MAX,
            api_server: ApiServerMetrics::maximum_for_test(),
            balloon: BalloonMetrics::maximum_for_test(),
            block: BlockMetrics::maximum_for_test(),
            deprecated_api: DeprecatedApiMetrics::maximum_for_test(),
            get_api_requests: GetApiRequestMetrics::maximum_for_test(),
            i8042: I8042Metrics::maximum_for_test(),
            rtc: RtcMetrics::maximum_for_test(),
            uart: UartMetrics::maximum_for_test(),
            latencies_us: LatencyMetrics::maximum_for_test(),
            logger: LoggerMetrics::maximum_for_test(),
            mmds: MmdsMetrics::maximum_for_test(),
            net: NetworkMetrics::maximum_for_test(),
            patch_api_requests: PatchApiRequestMetrics::maximum_for_test(),
            put_api_requests: PutApiRequestMetrics::maximum_for_test(),
            seccomp: SeccompMetrics::maximum_for_test(),
            vcpu: VcpuMetrics::maximum_for_test(),
            vmm: VmmMetrics::maximum_for_test(),
            signals: SignalMetrics::maximum_for_test(),
            vsock: VsockMetrics::maximum_for_test(),
            entropy: EntropyMetrics::maximum_for_test(),
            pmem: PmemMetrics::maximum_for_test(),
            interrupts: InterruptMetrics::maximum_for_test(),
            memory_hotplug: MemoryHotplugMetrics::maximum_for_test(),
            ..Self::default()
        }
    }
}

pub(super) fn build_metrics_line(
    utc_timestamp_ms: u64,
    current: &super::MetricsSnapshot,
    interval: &super::MetricsSnapshot,
    configured: &ConfiguredMetricsDevices,
) -> Result<FirecrackerMetricsLine, MetricsLineBuildError> {
    let mut block_devices = Vec::new();
    block_devices
        .try_reserve_exact(configured.ordinary_block_ids().len())
        .map_err(|_| MetricsLineBuildError::Allocation)?;
    let mut block = BlockMetrics::default();
    for drive_id in configured.ordinary_block_ids() {
        let metrics = interval
            .diagnostics
            .block_device_metrics_by_drive
            .as_ref()
            .and_then(|metrics| metrics.metrics.get(drive_id))
            .map(|entry| entry.metrics)
            .unwrap_or_default();
        let values = map_block_metrics(metrics);
        add_block_metrics(&mut block, &values);
        block_devices.push(DynamicMetrics {
            root: try_dynamic_root("block_", drive_id)?,
            values,
        });
    }

    let mut network_devices = Vec::new();
    network_devices
        .try_reserve_exact(configured.network_ids().len())
        .map_err(|_| MetricsLineBuildError::Allocation)?;
    let mut net = NetworkMetrics::default();
    for iface_id in configured.network_ids() {
        let metrics = interval
            .diagnostics
            .network_interface_metrics_by_interface
            .as_ref()
            .and_then(|metrics| metrics.metrics.get(iface_id))
            .map(|entry| entry.metrics)
            .unwrap_or_default();
        let values = map_network_metrics(metrics);
        add_network_metrics(&mut net, &values);
        network_devices.push(DynamicMetrics {
            root: try_dynamic_root("net_", iface_id)?,
            values,
        });
    }

    let mut vhost_user_block_devices = Vec::new();
    vhost_user_block_devices
        .try_reserve_exact(configured.vhost_user_block_ids().len())
        .map_err(|_| MetricsLineBuildError::Allocation)?;
    for drive_id in configured.vhost_user_block_ids() {
        let current_metrics = current
            .diagnostics
            .vhost_user_block_device_metrics_by_drive
            .as_ref()
            .and_then(|metrics| metrics.metrics.get(drive_id))
            .map(|entry| entry.metrics)
            .unwrap_or_default();
        let interval_metrics = interval
            .diagnostics
            .vhost_user_block_device_metrics_by_drive
            .as_ref()
            .and_then(|metrics| metrics.metrics.get(drive_id))
            .map(|entry| entry.metrics)
            .unwrap_or_default();
        vhost_user_block_devices.push(DynamicMetrics {
            root: try_dynamic_root("vhost_user_block_", drive_id)?,
            values: VhostUserBlockMetrics {
                activate_fails: interval_metrics.activate_fails,
                cfg_fails: interval_metrics.cfg_fails,
                init_time_us: current_metrics.init_time_us.unwrap_or_default(),
                activate_time_us: current_metrics.activate_time_us.unwrap_or_default(),
                config_change_time_us: current_metrics.config_change_time_us.unwrap_or_default(),
            },
        });
    }

    let current_diagnostics = &current.diagnostics;
    let interval_diagnostics = &interval.diagnostics;
    let current_vcpu = current_diagnostics.vcpu_metrics.unwrap_or_default();
    let interval_vcpu = interval_diagnostics.vcpu_metrics.unwrap_or_default();
    let interrupts = interval_diagnostics.interrupt_metrics.unwrap_or_default();
    let balloon = interval_diagnostics
        .balloon_device_metrics
        .unwrap_or_default();
    let balloon_hint = balloon.hinting_discard;
    let balloon_report = balloon.free_page_report;
    let serial = interval_diagnostics
        .serial_output_metrics
        .unwrap_or_default();
    let rtc = interval_diagnostics.rtc_device_metrics.unwrap_or_default();
    let mmds = interval_diagnostics.mmds_metrics.unwrap_or_default();
    let vsock = interval_diagnostics
        .vsock_device_metrics
        .unwrap_or_default();
    let entropy = interval_diagnostics
        .entropy_device_metrics
        .unwrap_or_default();
    let pmem = interval_diagnostics.pmem_device_metrics.unwrap_or_default();
    let memory_hotplug = interval_diagnostics
        .memory_hotplug_device_metrics
        .unwrap_or_default();

    Ok(FirecrackerMetricsLine {
        utc_timestamp_ms,
        api_server: ApiServerMetrics {
            process_startup_time_us: current_diagnostics.start_time_us.unwrap_or_default(),
            process_startup_time_cpu_us: current_diagnostics
                .start_time_cpu_us
                .unwrap_or_default()
                .saturating_add(current_diagnostics.parent_cpu_time_us.unwrap_or_default()),
        },
        balloon: BalloonMetrics {
            activate_fails: balloon.activate_fails,
            inflate_count: balloon.inflate_count,
            stats_updates_count: balloon.stats_updates_count,
            stats_update_fails: balloon.stats_update_fails,
            deflate_count: balloon.deflate_count,
            event_fails: balloon.event_fails,
            free_page_report_count: balloon_report.count,
            free_page_report_freed: balloon_report.completed_bytes,
            free_page_report_fails: balloon_report.failures,
            free_page_hint_count: balloon_hint.attempts,
            free_page_hint_freed: balloon_hint.completed_bytes,
            free_page_hint_fails: balloon_hint.failures,
        },
        block_devices,
        block,
        deprecated_api: DeprecatedApiMetrics {
            deprecated_http_api_calls: interval.deprecated_api.deprecated_http_api_calls,
        },
        get_api_requests: GetApiRequestMetrics {
            instance_info_count: interval.get_api_requests.instance_info_count,
            machine_cfg_count: interval.get_api_requests.machine_cfg_count,
            mmds_count: interval.get_api_requests.mmds_count,
            vmm_version_count: interval.get_api_requests.vmm_version_count,
            hotplug_memory_count: interval.get_api_requests.hotplug_memory_count,
        },
        i8042: I8042Metrics {
            error_count: 0,
            missed_read_count: 0,
            missed_write_count: 0,
            read_count: 0,
            reset_count: 0,
            write_count: 0,
        },
        rtc: RtcMetrics {
            error_count: rtc.error_count,
            missed_read_count: rtc.missed_read_count,
            missed_write_count: rtc.missed_write_count,
        },
        uart: UartMetrics {
            error_count: serial.error_count(),
            // Firecracker v1.16.0 declares this field but has no UART producer.
            flush_count: 0,
            missed_read_count: serial.missed_read_count(),
            missed_write_count: serial.missed_write_count(),
            read_count: serial.read_count(),
            write_count: serial.write_count(),
            rate_limiter_dropped_bytes: serial.rate_limiter_dropped_bytes(),
        },
        latencies_us: LatencyMetrics {
            full_create_snapshot: interval
                .latencies_us
                .full_create_snapshot
                .unwrap_or_default(),
            diff_create_snapshot: interval
                .latencies_us
                .diff_create_snapshot
                .unwrap_or_default(),
            load_snapshot: interval.latencies_us.load_snapshot.unwrap_or_default(),
            pause_vm: interval.latencies_us.pause_vm.unwrap_or_default(),
            resume_vm: interval.latencies_us.resume_vm.unwrap_or_default(),
            vmm_full_create_snapshot: interval
                .latencies_us
                .vmm_full_create_snapshot
                .unwrap_or_default(),
            vmm_diff_create_snapshot: interval
                .latencies_us
                .vmm_diff_create_snapshot
                .unwrap_or_default(),
            vmm_load_snapshot: interval.latencies_us.vmm_load_snapshot.unwrap_or_default(),
            vmm_pause_vm: interval.latencies_us.vmm_pause_vm.unwrap_or_default(),
            vmm_resume_vm: interval.latencies_us.vmm_resume_vm.unwrap_or_default(),
        },
        logger: LoggerMetrics {
            missed_metrics_count: interval.logger_metrics.missed_metrics_count,
            metrics_fails: 0,
            missed_log_count: interval.logger_metrics.missed_log_count,
            rate_limited_log_count: interval.logger_metrics.rate_limited_log_count,
        },
        mmds: MmdsMetrics {
            rx_accepted: mmds.rx_accepted,
            rx_accepted_err: mmds.rx_accepted_err,
            rx_accepted_unusual: mmds.rx_accepted_unusual,
            rx_bad_eth: mmds.rx_bad_eth,
            rx_invalid_token: mmds.rx_invalid_token,
            rx_no_token: mmds.rx_no_token,
            rx_count: mmds.rx_count,
            tx_bytes: mmds.tx_bytes,
            tx_count: mmds.tx_count,
            tx_errors: mmds.tx_errors,
            tx_frames: mmds.tx_frames,
            connections_created: mmds.connections_created,
            connections_destroyed: mmds.connections_destroyed,
        },
        network_devices,
        net,
        patch_api_requests: PatchApiRequestMetrics {
            drive_count: interval.patch_api_requests.drive_count,
            drive_fails: interval.patch_api_requests.drive_fails,
            network_count: interval.patch_api_requests.network_count,
            network_fails: interval.patch_api_requests.network_fails,
            machine_cfg_count: interval.patch_api_requests.machine_cfg_count,
            machine_cfg_fails: interval.patch_api_requests.machine_cfg_fails,
            mmds_count: interval.patch_api_requests.mmds_count,
            mmds_fails: interval.patch_api_requests.mmds_fails,
            hotplug_memory_count: interval.patch_api_requests.hotplug_memory_count,
            hotplug_memory_fails: interval.patch_api_requests.hotplug_memory_fails,
            pmem_count: interval.patch_api_requests.pmem_count,
            pmem_fails: interval.patch_api_requests.pmem_fails,
        },
        put_api_requests: PutApiRequestMetrics {
            actions_count: interval.put_api_requests.actions_count,
            actions_fails: interval.put_api_requests.actions_fails,
            boot_source_count: interval.put_api_requests.boot_source_count,
            boot_source_fails: interval.put_api_requests.boot_source_fails,
            drive_count: interval.put_api_requests.drive_count,
            drive_fails: interval.put_api_requests.drive_fails,
            logger_count: interval.put_api_requests.logger_count,
            logger_fails: interval.put_api_requests.logger_fails,
            machine_cfg_count: interval.put_api_requests.machine_cfg_count,
            machine_cfg_fails: interval.put_api_requests.machine_cfg_fails,
            cpu_cfg_count: interval.put_api_requests.cpu_cfg_count,
            cpu_cfg_fails: interval.put_api_requests.cpu_cfg_fails,
            metrics_count: interval.put_api_requests.metrics_count,
            metrics_fails: interval.put_api_requests.metrics_fails,
            network_count: interval.put_api_requests.network_count,
            network_fails: interval.put_api_requests.network_fails,
            mmds_count: interval.put_api_requests.mmds_count,
            mmds_fails: interval.put_api_requests.mmds_fails,
            vsock_count: interval.put_api_requests.vsock_count,
            vsock_fails: interval.put_api_requests.vsock_fails,
            pmem_count: interval.put_api_requests.pmem_count,
            pmem_fails: interval.put_api_requests.pmem_fails,
            serial_count: interval.put_api_requests.serial_count,
            serial_fails: interval.put_api_requests.serial_fails,
            hotplug_memory_count: interval.put_api_requests.hotplug_memory_count,
            hotplug_memory_fails: interval.put_api_requests.hotplug_memory_fails,
        },
        seccomp: SeccompMetrics::default(),
        vcpu: VcpuMetrics {
            exit_io_in: 0,
            exit_io_out: 0,
            exit_mmio_read: interval_vcpu.exit_mmio_read(),
            exit_mmio_write: interval_vcpu.exit_mmio_write(),
            failures: interval_vcpu.failures(),
            kvmclock_ctrl_fails: 0,
            exit_io_in_agg: LatencyAggregate {
                min_us: 0,
                max_us: 0,
                sum_us: 0,
            },
            exit_io_out_agg: LatencyAggregate {
                min_us: 0,
                max_us: 0,
                sum_us: 0,
            },
            exit_mmio_read_agg: LatencyAggregate {
                min_us: current_vcpu.exit_mmio_read_agg().min_us(),
                max_us: current_vcpu.exit_mmio_read_agg().max_us(),
                sum_us: interval_vcpu.exit_mmio_read_agg().sum_us(),
            },
            exit_mmio_write_agg: LatencyAggregate {
                min_us: current_vcpu.exit_mmio_write_agg().min_us(),
                max_us: current_vcpu.exit_mmio_write_agg().max_us(),
                sum_us: interval_vcpu.exit_mmio_write_agg().sum_us(),
            },
        },
        vmm: VmmMetrics {
            panic_count: current.panic_count,
        },
        signals: SignalMetrics {
            sigxfsz: current_diagnostics
                .signal_metrics
                .unwrap_or_default()
                .sigxfsz(),
            sigxcpu: current_diagnostics
                .signal_metrics
                .unwrap_or_default()
                .sigxcpu(),
            sigpipe: interval_diagnostics
                .signal_metrics
                .unwrap_or_default()
                .sigpipe(),
            sighup: current_diagnostics
                .signal_metrics
                .unwrap_or_default()
                .sighup(),
            ..SignalMetrics::default()
        },
        vsock: VsockMetrics {
            activate_fails: vsock.activate_fails,
            cfg_fails: vsock.cfg_fails,
            rx_queue_event_fails: vsock.rx_queue_event_fails,
            tx_queue_event_fails: vsock.tx_queue_event_fails,
            ev_queue_event_fails: vsock.ev_queue_event_fails,
            muxer_event_fails: vsock.muxer_event_fails,
            conn_event_fails: vsock.conn_event_fails,
            rx_queue_event_count: vsock.rx_queue_event_count,
            tx_queue_event_count: vsock.tx_queue_event_count,
            rx_bytes_count: vsock.rx_bytes_count,
            tx_bytes_count: vsock.tx_bytes_count,
            rx_packets_count: vsock.rx_packets_count,
            tx_packets_count: vsock.tx_packets_count,
            conns_added: vsock.conns_added,
            conns_killed: vsock.conns_killed,
            conns_removed: vsock.conns_removed,
            killq_resync: vsock.killq_resync,
            tx_flush_fails: vsock.tx_flush_fails,
            tx_write_fails: vsock.tx_write_fails,
            rx_read_fails: vsock.rx_read_fails,
        },
        entropy: EntropyMetrics {
            activate_fails: entropy.activate_fails,
            entropy_event_fails: entropy.entropy_event_fails,
            entropy_event_count: entropy.entropy_event_count,
            entropy_bytes: entropy.entropy_bytes,
            host_rng_fails: entropy.host_rng_fails,
            entropy_rate_limiter_throttled: entropy.entropy_rate_limiter_throttled,
            rate_limiter_event_count: entropy.rate_limiter_event_count,
        },
        pmem: PmemMetrics {
            activate_fails: pmem.activate_fails,
            cfg_fails: pmem.cfg_fails,
            event_fails: pmem.event_fails,
            queue_event_count: pmem.queue_event_count,
            rate_limiter_throttled_events: pmem.rate_limiter_throttled_events,
            rate_limiter_event_count: pmem.rate_limiter_event_count,
        },
        vhost_user_block_devices,
        interrupts: InterruptMetrics {
            triggers: interrupts.triggers(),
            config_updates: interrupts.config_updates(),
        },
        memory_hotplug: MemoryHotplugMetrics {
            activate_fails: memory_hotplug.activate_fails,
            queue_event_fails: memory_hotplug.queue_event_fails,
            queue_event_count: memory_hotplug.queue_event_count,
            plug_agg: map_memory_hotplug_latency(memory_hotplug.plug_agg),
            plug_count: memory_hotplug.plug_count,
            plug_bytes: memory_hotplug.plug_bytes,
            plug_fails: memory_hotplug.plug_fails,
            unplug_agg: map_memory_hotplug_latency(memory_hotplug.unplug_agg),
            unplug_count: memory_hotplug.unplug_count,
            unplug_bytes: memory_hotplug.unplug_bytes,
            unplug_fails: memory_hotplug.unplug_fails,
            unplug_discard_fails: memory_hotplug.unplug_discard_fails,
            unplug_all_agg: map_memory_hotplug_latency(memory_hotplug.unplug_all_agg),
            unplug_all_count: memory_hotplug.unplug_all_count,
            unplug_all_fails: memory_hotplug.unplug_all_fails,
            state_agg: map_memory_hotplug_latency(memory_hotplug.state_agg),
            state_count: memory_hotplug.state_count,
            state_fails: memory_hotplug.state_fails,
        },
    })
}

fn try_dynamic_root(prefix: &str, id: &str) -> Result<String, MetricsLineBuildError> {
    let length = prefix
        .len()
        .checked_add(id.len())
        .ok_or(MetricsLineBuildError::ConfiguredIdentityBytes)?;
    let mut root = String::new();
    root.try_reserve_exact(length)
        .map_err(|_| MetricsLineBuildError::Allocation)?;
    root.push_str(prefix);
    root.push_str(id);
    Ok(root)
}

fn map_block_metrics(metrics: super::BlockDeviceMetrics) -> BlockMetrics {
    BlockMetrics {
        activate_fails: metrics.activate_fails,
        cfg_fails: metrics.cfg_fails,
        no_avail_buffer: metrics.no_avail_buffer,
        event_fails: metrics.event_fails,
        execute_fails: metrics.execute_fails,
        invalid_reqs_count: metrics.invalid_reqs_count,
        flush_count: metrics.flush_count,
        queue_event_count: metrics.queue_event_count,
        rate_limiter_event_count: metrics.rate_limiter_event_count,
        update_count: metrics.update_count,
        update_fails: metrics.update_fails,
        read_bytes: metrics.read_bytes,
        write_bytes: metrics.write_bytes,
        read_count: metrics.read_count,
        write_count: metrics.write_count,
        read_agg: LatencyAggregate {
            min_us: metrics.read_agg.min_us(),
            max_us: metrics.read_agg.max_us(),
            sum_us: metrics.read_agg.sum_us(),
        },
        write_agg: LatencyAggregate {
            min_us: metrics.write_agg.min_us(),
            max_us: metrics.write_agg.max_us(),
            sum_us: metrics.write_agg.sum_us(),
        },
        rate_limiter_throttled_events: metrics.rate_limiter_throttled_events,
        io_engine_throttled_events: metrics.io_engine_throttled_events,
        remaining_reqs_count: metrics.remaining_reqs_count,
    }
}

fn add_block_metrics(aggregate: &mut BlockMetrics, metrics: &BlockMetrics) {
    aggregate.activate_fails = aggregate
        .activate_fails
        .saturating_add(metrics.activate_fails);
    aggregate.cfg_fails = aggregate.cfg_fails.saturating_add(metrics.cfg_fails);
    aggregate.no_avail_buffer = aggregate
        .no_avail_buffer
        .saturating_add(metrics.no_avail_buffer);
    aggregate.event_fails = aggregate.event_fails.saturating_add(metrics.event_fails);
    aggregate.execute_fails = aggregate
        .execute_fails
        .saturating_add(metrics.execute_fails);
    aggregate.invalid_reqs_count = aggregate
        .invalid_reqs_count
        .saturating_add(metrics.invalid_reqs_count);
    aggregate.flush_count = aggregate.flush_count.saturating_add(metrics.flush_count);
    aggregate.queue_event_count = aggregate
        .queue_event_count
        .saturating_add(metrics.queue_event_count);
    aggregate.rate_limiter_event_count = aggregate
        .rate_limiter_event_count
        .saturating_add(metrics.rate_limiter_event_count);
    aggregate.update_count = aggregate.update_count.saturating_add(metrics.update_count);
    aggregate.update_fails = aggregate.update_fails.saturating_add(metrics.update_fails);
    aggregate.read_bytes = aggregate.read_bytes.saturating_add(metrics.read_bytes);
    aggregate.write_bytes = aggregate.write_bytes.saturating_add(metrics.write_bytes);
    aggregate.read_count = aggregate.read_count.saturating_add(metrics.read_count);
    aggregate.write_count = aggregate.write_count.saturating_add(metrics.write_count);
    aggregate.read_agg.sum_us = aggregate
        .read_agg
        .sum_us
        .saturating_add(metrics.read_agg.sum_us);
    aggregate.write_agg.sum_us = aggregate
        .write_agg
        .sum_us
        .saturating_add(metrics.write_agg.sum_us);
    aggregate.rate_limiter_throttled_events = aggregate
        .rate_limiter_throttled_events
        .saturating_add(metrics.rate_limiter_throttled_events);
    aggregate.io_engine_throttled_events = aggregate
        .io_engine_throttled_events
        .saturating_add(metrics.io_engine_throttled_events);
    aggregate.remaining_reqs_count = aggregate
        .remaining_reqs_count
        .saturating_add(metrics.remaining_reqs_count);
}

fn map_network_metrics(metrics: super::NetworkInterfaceMetrics) -> NetworkMetrics {
    NetworkMetrics {
        activate_fails: metrics.activate_fails,
        cfg_fails: metrics.cfg_fails,
        no_rx_avail_buffer: metrics.no_rx_avail_buffer,
        no_tx_avail_buffer: metrics.no_tx_avail_buffer,
        event_fails: metrics.event_fails,
        rx_queue_event_count: metrics.rx_queue_event_count,
        rx_event_rate_limiter_count: metrics.rx_rate_limiter_event_count,
        rx_rate_limiter_throttled: metrics.rx_rate_limiter_throttled,
        rx_tap_event_count: metrics.rx_tap_event_count,
        rx_bytes_count: metrics.rx_bytes_count,
        rx_packets_count: metrics.rx_packets_count,
        rx_fails: metrics.rx_fails,
        rx_count: metrics.rx_count,
        tap_read_fails: metrics.tap_read_fails,
        tap_write_fails: metrics.tap_write_fails,
        tap_write_agg: LatencyAggregate {
            min_us: metrics.tap_write_latency.min_us(),
            max_us: metrics.tap_write_latency.max_us(),
            sum_us: metrics.tap_write_latency.sum_us(),
        },
        tx_bytes_count: metrics.tx_bytes_count,
        tx_malformed_frames: metrics.tx_malformed_frames,
        tx_fails: metrics.tx_fails,
        tx_count: metrics.tx_count,
        tx_packets_count: metrics.tx_packets_count,
        tx_queue_event_count: metrics.tx_queue_event_count,
        tx_rate_limiter_event_count: metrics.tx_rate_limiter_event_count,
        tx_rate_limiter_throttled: metrics.tx_rate_limiter_throttled,
        tx_spoofed_mac_count: metrics.tx_spoofed_mac_count,
        tx_remaining_reqs_count: metrics.tx_remaining_reqs_count,
        ..NetworkMetrics::default()
    }
}

fn add_network_metrics(aggregate: &mut NetworkMetrics, metrics: &NetworkMetrics) {
    aggregate.activate_fails = aggregate
        .activate_fails
        .saturating_add(metrics.activate_fails);
    aggregate.cfg_fails = aggregate.cfg_fails.saturating_add(metrics.cfg_fails);
    aggregate.mac_address_updates = aggregate
        .mac_address_updates
        .saturating_add(metrics.mac_address_updates);
    aggregate.no_rx_avail_buffer = aggregate
        .no_rx_avail_buffer
        .saturating_add(metrics.no_rx_avail_buffer);
    aggregate.no_tx_avail_buffer = aggregate
        .no_tx_avail_buffer
        .saturating_add(metrics.no_tx_avail_buffer);
    aggregate.event_fails = aggregate.event_fails.saturating_add(metrics.event_fails);
    aggregate.rx_queue_event_count = aggregate
        .rx_queue_event_count
        .saturating_add(metrics.rx_queue_event_count);
    aggregate.rx_event_rate_limiter_count = aggregate
        .rx_event_rate_limiter_count
        .saturating_add(metrics.rx_event_rate_limiter_count);
    aggregate.rx_rate_limiter_throttled = aggregate
        .rx_rate_limiter_throttled
        .saturating_add(metrics.rx_rate_limiter_throttled);
    aggregate.rx_tap_event_count = aggregate
        .rx_tap_event_count
        .saturating_add(metrics.rx_tap_event_count);
    aggregate.rx_bytes_count = aggregate
        .rx_bytes_count
        .saturating_add(metrics.rx_bytes_count);
    aggregate.rx_packets_count = aggregate
        .rx_packets_count
        .saturating_add(metrics.rx_packets_count);
    aggregate.rx_fails = aggregate.rx_fails.saturating_add(metrics.rx_fails);
    aggregate.rx_count = aggregate.rx_count.saturating_add(metrics.rx_count);
    aggregate.tap_read_fails = aggregate
        .tap_read_fails
        .saturating_add(metrics.tap_read_fails);
    aggregate.tap_write_fails = aggregate
        .tap_write_fails
        .saturating_add(metrics.tap_write_fails);
    aggregate.tap_write_agg.sum_us = aggregate
        .tap_write_agg
        .sum_us
        .saturating_add(metrics.tap_write_agg.sum_us);
    aggregate.tx_bytes_count = aggregate
        .tx_bytes_count
        .saturating_add(metrics.tx_bytes_count);
    aggregate.tx_malformed_frames = aggregate
        .tx_malformed_frames
        .saturating_add(metrics.tx_malformed_frames);
    aggregate.tx_fails = aggregate.tx_fails.saturating_add(metrics.tx_fails);
    aggregate.tx_count = aggregate.tx_count.saturating_add(metrics.tx_count);
    aggregate.tx_packets_count = aggregate
        .tx_packets_count
        .saturating_add(metrics.tx_packets_count);
    aggregate.tx_queue_event_count = aggregate
        .tx_queue_event_count
        .saturating_add(metrics.tx_queue_event_count);
    aggregate.tx_rate_limiter_event_count = aggregate
        .tx_rate_limiter_event_count
        .saturating_add(metrics.tx_rate_limiter_event_count);
    aggregate.tx_rate_limiter_throttled = aggregate
        .tx_rate_limiter_throttled
        .saturating_add(metrics.tx_rate_limiter_throttled);
    aggregate.tx_spoofed_mac_count = aggregate
        .tx_spoofed_mac_count
        .saturating_add(metrics.tx_spoofed_mac_count);
    aggregate.tx_remaining_reqs_count = aggregate
        .tx_remaining_reqs_count
        .saturating_add(metrics.tx_remaining_reqs_count);
}

fn map_memory_hotplug_latency(metrics: super::MemoryHotplugLatencyMetrics) -> LatencyAggregate {
    LatencyAggregate {
        min_us: metrics.min_us,
        max_us: metrics.max_us,
        sum_us: metrics.sum_us,
    }
}

pub(super) fn serialize_metrics_line(
    line: &FirecrackerMetricsLine,
) -> Result<Vec<u8>, MetricsLineBuildError> {
    serialize_metrics_line_with_reserve(line, |bytes, expected| {
        bytes
            .try_reserve_exact(expected)
            .map_err(|_| MetricsLineBuildError::Allocation)
    })
}

fn serialize_metrics_line_with_reserve(
    line: &FirecrackerMetricsLine,
    reserve: impl FnOnce(&mut Vec<u8>, usize) -> Result<(), MetricsLineBuildError>,
) -> Result<Vec<u8>, MetricsLineBuildError> {
    let mut counter = CountingWriter::default();
    let first = serde_json::to_writer(&mut counter, line);
    if counter.too_long {
        return Err(MetricsLineBuildError::LineTooLong);
    }
    first.map_err(|_| MetricsLineBuildError::Serialization)?;

    let expected = counter.length;
    let mut bytes = Vec::new();
    reserve(&mut bytes, expected)?;
    let mut writer = FixedVecWriter {
        bytes: &mut bytes,
        expected,
        overflowed: false,
    };
    let second = serde_json::to_writer(&mut writer, line);
    if writer.overflowed {
        return Err(MetricsLineBuildError::Serialization);
    }
    second.map_err(|_| MetricsLineBuildError::Serialization)?;
    if bytes.len() != expected {
        return Err(MetricsLineBuildError::Serialization);
    }

    Ok(bytes)
}

#[derive(Debug, Default)]
struct CountingWriter {
    length: usize,
    too_long: bool,
}

impl Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(next) = self.length.checked_add(bytes.len()) else {
            self.too_long = true;
            return Err(io::Error::other("metrics line length overflow"));
        };
        if next > FIRECRACKER_METRICS_MAX_JSON_BYTES {
            self.too_long = true;
            return Err(io::Error::other("metrics line length limit exceeded"));
        }
        self.length = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct FixedVecWriter<'a> {
    bytes: &'a mut Vec<u8>,
    expected: usize,
    overflowed: bool,
}

impl Write for FixedVecWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(next) = self.bytes.len().checked_add(bytes.len()) else {
            self.overflowed = true;
            return Err(io::Error::other("metrics serialization length overflow"));
        };
        if next > self.expected {
            self.overflowed = true;
            return Err(io::Error::other("metrics serialization changed length"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CompiledMetricDescriptor {
    pub(super) id: String,
    pub(super) path: String,
    pub(super) json_type: &'static str,
    pub(super) value_kind: &'static str,
    pub(super) rust_type: &'static str,
}

#[cfg(test)]
pub(super) fn compiled_metric_descriptors() -> Vec<CompiledMetricDescriptor> {
    let mut descriptors = vec![CompiledMetricDescriptor {
        id: "static:utc_timestamp_ms".to_owned(),
        path: "utc_timestamp_ms".to_owned(),
        json_type: "number",
        value_kind: "attempt-timestamp",
        rust_type: "SerializeToUtcTimestampMs",
    }];

    append_static_descriptors::<ApiServerMetrics>(&mut descriptors);
    append_static_descriptors::<BalloonMetrics>(&mut descriptors);
    append_static_descriptors::<BlockMetrics>(&mut descriptors);
    append_static_descriptors::<DeprecatedApiMetrics>(&mut descriptors);
    append_static_descriptors::<GetApiRequestMetrics>(&mut descriptors);
    append_static_descriptors::<I8042Metrics>(&mut descriptors);
    append_static_descriptors::<RtcMetrics>(&mut descriptors);
    append_static_descriptors::<UartMetrics>(&mut descriptors);
    append_static_descriptors::<LatencyMetrics>(&mut descriptors);
    append_static_descriptors::<LoggerMetrics>(&mut descriptors);
    append_static_descriptors::<MmdsMetrics>(&mut descriptors);
    append_static_descriptors::<NetworkMetrics>(&mut descriptors);
    append_static_descriptors::<PatchApiRequestMetrics>(&mut descriptors);
    append_static_descriptors::<PutApiRequestMetrics>(&mut descriptors);
    append_static_descriptors::<SeccompMetrics>(&mut descriptors);
    append_static_descriptors::<VcpuMetrics>(&mut descriptors);
    append_static_descriptors::<VmmMetrics>(&mut descriptors);
    append_static_descriptors::<SignalMetrics>(&mut descriptors);
    append_static_descriptors::<VsockMetrics>(&mut descriptors);
    append_static_descriptors::<EntropyMetrics>(&mut descriptors);
    append_static_descriptors::<PmemMetrics>(&mut descriptors);
    append_static_descriptors::<InterruptMetrics>(&mut descriptors);
    append_static_descriptors::<MemoryHotplugMetrics>(&mut descriptors);
    append_family_descriptors(
        &mut descriptors,
        "dynamic",
        "block_{drive_id}",
        BlockMetrics::FIELDS,
    );
    append_family_descriptors(
        &mut descriptors,
        "dynamic",
        "net_{iface_id}",
        NetworkMetrics::FIELDS,
    );
    append_family_descriptors(
        &mut descriptors,
        "dynamic",
        VhostUserBlockMetrics::ROOT_TEMPLATE,
        VhostUserBlockMetrics::FIELDS,
    );
    descriptors
}

#[cfg(test)]
fn append_static_descriptors<T: MetricFamilySchema>(
    descriptors: &mut Vec<CompiledMetricDescriptor>,
) {
    append_family_descriptors(descriptors, "static", T::ROOT_TEMPLATE, T::FIELDS);
}

#[cfg(test)]
fn append_family_descriptors(
    descriptors: &mut Vec<CompiledMetricDescriptor>,
    scope: &str,
    root: &str,
    fields: &[MetricFieldDeclaration],
) {
    for field in fields {
        match field.kind {
            MetricFieldKind::Incremental => descriptors.push(metric_descriptor(
                scope,
                root,
                field.name,
                "incremental-interval",
                "SharedIncMetric",
            )),
            MetricFieldKind::Store => descriptors.push(metric_descriptor(
                scope,
                root,
                field.name,
                "persistent-store",
                "SharedStoreMetric",
            )),
            MetricFieldKind::Latency => {
                descriptors.push(metric_descriptor(
                    scope,
                    root,
                    &format!("{}.min_us", field.name),
                    "persistent-store",
                    "SharedStoreMetric",
                ));
                descriptors.push(metric_descriptor(
                    scope,
                    root,
                    &format!("{}.max_us", field.name),
                    "persistent-store",
                    "SharedStoreMetric",
                ));
                descriptors.push(metric_descriptor(
                    scope,
                    root,
                    &format!("{}.sum_us", field.name),
                    "incremental-interval",
                    "SharedIncMetric",
                ));
            }
        }
    }
}

#[cfg(test)]
fn metric_descriptor(
    scope: &str,
    root: &str,
    field: &str,
    value_kind: &'static str,
    rust_type: &'static str,
) -> CompiledMetricDescriptor {
    let path = format!("{root}.{field}");
    CompiledMetricDescriptor {
        id: format!("{scope}:{path}"),
        path,
        json_type: "number",
        value_kind,
        rust_type,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::io::Write as _;
    use std::time::Duration;

    use super::*;
    use crate::metrics::MetricsSnapshot;

    #[test]
    fn compiled_schema_matches_checked_firecracker_authority() {
        let authority: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../compat/firecracker/v1.16.0/metrics-schema.json"
        ))
        .expect("checked metrics schema should be valid JSON");
        let fields = authority["source"]["fields"]
            .as_array()
            .expect("checked metrics schema should contain source fields");
        let compiled = compiled_metric_descriptors();

        assert_eq!(compiled.len(), 301);
        assert_eq!(compiled.len(), fields.len());
        for (ordinal, (compiled, authority)) in compiled.iter().zip(fields).enumerate() {
            assert_eq!(authority["ordinal"], ordinal);
            assert_eq!(authority["id"], compiled.id);
            assert_eq!(authority["path"], compiled.path);
            assert_eq!(authority["json_type"], compiled.json_type);
            assert_eq!(authority["value_kind"], compiled.value_kind);
            assert_eq!(authority["rust_type"], compiled.rust_type);
        }
    }

    fn line_with_newline(line: &FirecrackerMetricsLine) -> Vec<u8> {
        let mut bytes = serialize_metrics_line(line).expect("metrics line should serialize");
        bytes.push(b'\n');
        bytes
    }

    fn collect_leaf_paths(prefix: &str, value: &serde_json::Value, paths: &mut BTreeSet<String>) {
        match value {
            serde_json::Value::Object(object) => {
                for (name, value) in object {
                    let path = if prefix.is_empty() {
                        name.clone()
                    } else {
                        format!("{prefix}.{name}")
                    };
                    collect_leaf_paths(&path, value, paths);
                }
            }
            serde_json::Value::Number(_) => {
                paths.insert(prefix.to_owned());
            }
            _ => panic!("canonical metrics leaves must all be JSON numbers"),
        }
    }

    fn assert_exact_static_schema(bytes: &[u8]) {
        assert_eq!(bytes.last(), Some(&b'\n'));
        let value: serde_json::Value = serde_json::from_slice(&bytes[..bytes.len() - 1])
            .expect("fixture should be valid JSON");
        assert_eq!(
            value
                .as_object()
                .expect("metrics root should be an object")
                .len(),
            24
        );

        let mut actual_paths = BTreeSet::new();
        collect_leaf_paths("", &value, &mut actual_paths);
        let expected_paths = compiled_metric_descriptors()
            .into_iter()
            .filter(|descriptor| descriptor.id.starts_with("static:"))
            .map(|descriptor| descriptor.path)
            .collect::<BTreeSet<_>>();
        assert_eq!(actual_paths, expected_paths);
    }

    #[test]
    fn configured_attempt_bounds_accept_exact_limits_and_reject_each_excess() {
        assert_eq!(
            validate_configured_metrics_bounds(
                FIRECRACKER_METRICS_MAX_DYNAMIC_ROOTS,
                MAX_NETWORK_INTERFACE_COUNT,
                FIRECRACKER_METRICS_MAX_IDENTITY_BYTES,
            ),
            Ok(())
        );
        assert_eq!(
            validate_configured_metrics_bounds(
                FIRECRACKER_METRICS_MAX_DYNAMIC_ROOTS + 1,
                MAX_NETWORK_INTERFACE_COUNT,
                FIRECRACKER_METRICS_MAX_IDENTITY_BYTES,
            ),
            Err(MetricsLineBuildError::TooManyConfiguredDevices)
        );
        assert_eq!(
            validate_configured_metrics_bounds(
                FIRECRACKER_METRICS_MAX_DYNAMIC_ROOTS,
                MAX_NETWORK_INTERFACE_COUNT + 1,
                FIRECRACKER_METRICS_MAX_IDENTITY_BYTES,
            ),
            Err(MetricsLineBuildError::TooManyNetworkInterfaces)
        );
        assert_eq!(
            validate_configured_metrics_bounds(
                FIRECRACKER_METRICS_MAX_DYNAMIC_ROOTS,
                MAX_NETWORK_INTERFACE_COUNT,
                FIRECRACKER_METRICS_MAX_IDENTITY_BYTES + 1,
            ),
            Err(MetricsLineBuildError::ConfiguredIdentityBytes)
        );
    }

    #[test]
    fn maximum_line_bound_fits_complete_record_limit() {
        let static_bytes =
            serialize_metrics_line(&FirecrackerMetricsLine::maximum_static_for_test())
                .expect("maximum static metrics should serialize");
        let block_bytes = serde_json::to_vec(&BlockMetrics::maximum_for_test())
            .expect("maximum block metrics should serialize");
        let network_bytes = serde_json::to_vec(&NetworkMetrics::maximum_for_test())
            .expect("maximum network metrics should serialize");
        let vhost_bytes = serde_json::to_vec(&VhostUserBlockMetrics::maximum_for_test())
            .expect("maximum vhost-user metrics should serialize");
        let block_overhead = 4 + "block_".len() + block_bytes.len();
        let vhost_overhead = 4 + "vhost_user_block_".len() + vhost_bytes.len();
        let network_overhead = 4 + "net_".len() + network_bytes.len();
        let nonnetwork_overhead = block_overhead.max(vhost_overhead);
        let upper_bound = static_bytes
            .len()
            .saturating_add(1)
            .saturating_add(FIRECRACKER_METRICS_MAX_IDENTITY_BYTES)
            .saturating_add(
                FIRECRACKER_METRICS_MAX_DYNAMIC_ROOTS.saturating_mul(nonnetwork_overhead),
            )
            .saturating_add(
                MAX_NETWORK_INTERFACE_COUNT
                    .saturating_mul(network_overhead.saturating_sub(nonnetwork_overhead)),
            );

        assert!(network_overhead >= nonnetwork_overhead);
        assert!(upper_bound < FIRECRACKER_METRICS_MAX_LINE_BYTES);
    }

    #[test]
    fn counting_writer_accepts_exact_json_limit_and_rejects_one_more_byte() {
        let mut writer = CountingWriter::default();
        let chunk = [0_u8; 4096];
        for _ in 0..(FIRECRACKER_METRICS_MAX_JSON_BYTES / chunk.len()) {
            writer
                .write_all(&chunk)
                .expect("bytes through the exact limit should count");
        }
        let remainder = FIRECRACKER_METRICS_MAX_JSON_BYTES % chunk.len();
        writer
            .write_all(&chunk[..remainder])
            .expect("final bytes through the exact limit should count");
        assert_eq!(writer.length, FIRECRACKER_METRICS_MAX_JSON_BYTES);
        assert!(!writer.too_long);

        assert!(writer.write_all(&[0]).is_err());
        assert!(writer.too_long);
    }

    #[test]
    fn fixed_writer_rejects_a_second_pass_longer_than_counted() {
        let mut bytes = Vec::with_capacity(3);
        let mut writer = FixedVecWriter {
            bytes: &mut bytes,
            expected: 3,
            overflowed: false,
        };
        writer
            .write_all(b"abc")
            .expect("counted bytes should fit exactly");
        assert!(writer.write_all(b"d").is_err());
        assert!(writer.overflowed);
        assert_eq!(bytes, b"abc");
    }

    #[test]
    fn serialization_reports_injected_exact_reservation_failure() {
        let line = FirecrackerMetricsLine {
            utc_timestamp_ms: 1,
            ..FirecrackerMetricsLine::default()
        };

        assert_eq!(
            serialize_metrics_line_with_reserve(&line, |_, _| {
                Err(MetricsLineBuildError::Allocation)
            }),
            Err(MetricsLineBuildError::Allocation)
        );
    }

    #[test]
    fn clock_rejects_pre_epoch_and_millisecond_overflow_without_panicking() {
        #[derive(Debug)]
        struct FixedClock(SystemTime);

        impl MetricsClock for FixedClock {
            fn now(&self) -> SystemTime {
                self.0
            }
        }

        let before_epoch = FixedClock(UNIX_EPOCH - Duration::from_millis(1));
        assert_eq!(
            unix_timestamp_ms(&before_epoch),
            Err(MetricsLineBuildError::Clock)
        );
        let overflowing =
            FixedClock(UNIX_EPOCH + Duration::from_millis(u64::MAX) + Duration::from_millis(1));
        assert_eq!(
            unix_timestamp_ms(&overflowing),
            Err(MetricsLineBuildError::Clock)
        );
    }

    #[test]
    fn maximum_configured_recipe_has_exact_sorted_exclusive_dynamic_roots() {
        const ORDINARY_COUNT: usize = 485;
        const VHOST_COUNT: usize = 484;
        let ordinary = (0..ORDINARY_COUNT)
            .map(|index| format!("ordinary_{index:03}"))
            .collect::<Vec<_>>();
        let vhost = (0..VHOST_COUNT)
            .map(|index| format!("vhost_{index:03}"))
            .collect::<Vec<_>>();
        let networks = (0..MAX_NETWORK_INTERFACE_COUNT)
            .map(|index| format!("eth_{index:02}"))
            .collect::<Vec<_>>();
        let ordinary_refs = ordinary.iter().map(String::as_str).collect::<Vec<_>>();
        let vhost_refs = vhost.iter().map(String::as_str).collect::<Vec<_>>();
        let network_refs = networks.iter().map(String::as_str).collect::<Vec<_>>();
        let configured =
            ConfiguredMetricsDevices::from_test_ids(&ordinary_refs, &vhost_refs, &network_refs);
        assert_eq!(
            ordinary.len() + vhost.len() + networks.len(),
            FIRECRACKER_METRICS_MAX_DYNAMIC_ROOTS
        );

        let line = build_metrics_line(
            7,
            &MetricsSnapshot::default(),
            &MetricsSnapshot::default(),
            &configured,
        )
        .expect("maximum configured recipe should build");
        let bytes =
            serialize_metrics_line(&line).expect("maximum configured line should serialize");
        let text = std::str::from_utf8(&bytes).expect("metrics JSON should be UTF-8");
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).expect("metrics line should be valid JSON");
        let root = value.as_object().expect("metrics root should be an object");

        assert_eq!(root.len(), 24 + FIRECRACKER_METRICS_MAX_DYNAMIC_ROOTS);
        assert_eq!(root["block_ordinary_000"].as_object().unwrap().len(), 20);
        assert_eq!(root["net_eth_00"].as_object().unwrap().len(), 27);
        assert_eq!(
            root["vhost_user_block_vhost_000"]
                .as_object()
                .unwrap()
                .len(),
            5
        );
        assert!(root.get("block_vhost_000").is_none());
        assert!(root.get("vhost_user_block_ordinary_000").is_none());
        assert_eq!(root["block"]["read_bytes"], 0);
        assert_eq!(root["net"]["rx_bytes_count"], 0);

        let first_block = text.find("\"block_ordinary_000\":").unwrap();
        let last_block = text
            .find(&format!("\"block_ordinary_{:03}\":", ORDINARY_COUNT - 1))
            .unwrap();
        let aggregate_block = text.find("\"block\":").unwrap();
        let first_network = text.find("\"net_eth_00\":").unwrap();
        let last_network = text
            .find(&format!(
                "\"net_eth_{:02}\":",
                MAX_NETWORK_INTERFACE_COUNT - 1
            ))
            .unwrap();
        let aggregate_network = text.find("\"net\":").unwrap();
        let pmem = text.find("\"pmem\":").unwrap();
        let first_vhost = text.find("\"vhost_user_block_vhost_000\":").unwrap();
        let interrupts = text.find("\"interrupts\":").unwrap();
        assert!(first_block < last_block && last_block < aggregate_block);
        assert!(first_network < last_network && last_network < aggregate_network);
        assert!(pmem < first_vhost && first_vhost < interrupts);
    }

    #[test]
    fn canonical_static_fixtures_match_exact_bytes_types_and_paths() {
        let minimal = FirecrackerMetricsLine {
            utc_timestamp_ms: 1,
            ..FirecrackerMetricsLine::default()
        };
        let maximum = FirecrackerMetricsLine::maximum_static_for_test();
        let minimal_bytes = line_with_newline(&minimal);
        let maximum_bytes = line_with_newline(&maximum);

        assert_eq!(
            minimal_bytes,
            include_bytes!("fixtures/minimal.jsonl").as_slice()
        );
        assert_eq!(
            maximum_bytes,
            include_bytes!("fixtures/all-static-nonzero.jsonl").as_slice()
        );
        assert_exact_static_schema(&minimal_bytes);
        assert_exact_static_schema(&maximum_bytes);
    }
}
