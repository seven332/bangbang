use std::fmt;

use bangbang_runtime::entropy::EntropyConfig;
use bangbang_runtime::network::{
    GuestMacAddress, NetworkDeviceProfile, VirtioNetworkFeatureCapabilities,
    VirtioNetworkPacketEnvelope,
};
use bangbang_runtime::serial::SerialMmioCaptureState;
use bangbang_runtime::snapshot_balloon_v2_9::{
    SnapshotV2BalloonAccountingState, SnapshotV2BalloonActiveQueuesState,
    SnapshotV2BalloonContinuationState, SnapshotV2BalloonHintState, SnapshotV2BalloonPfnRange,
    SnapshotV2BalloonQueueState, SnapshotV2BalloonState, SnapshotV2BalloonStatistics,
};
use bangbang_runtime::snapshot_entropy_v2_8::{
    SnapshotV2EntropyBucketState, SnapshotV2EntropyLimiterState, SnapshotV2EntropyQueueState,
    SnapshotV2EntropyRetryState, SnapshotV2EntropyState,
};
use bangbang_runtime::snapshot_memory_hotplug_v2_10::{
    SnapshotV2MemoryHotplugPluggedRange, SnapshotV2MemoryHotplugQueueState,
    SnapshotV2MemoryHotplugState,
};
use bangbang_runtime::snapshot_network_v2_11::{
    SnapshotV2MmdsInterfaceState, SnapshotV2MmdsState, SnapshotV2NetworkBackendClass,
    SnapshotV2NetworkInterfaceState, SnapshotV2NetworkLimiterState, SnapshotV2NetworkLocalState,
    SnapshotV2NetworkQueueState, SnapshotV2NetworkRetryState, SnapshotV2NetworkState,
    SnapshotV2NetworkTokenBucketState,
};
use bangbang_runtime::snapshot_serial_v2_7::{
    SnapshotV2SerialEndpointIntent, SnapshotV2SerialState,
};
use bangbang_runtime::snapshot_vsock_v2_12::{
    SnapshotV2VsockActiveQueuesState, SnapshotV2VsockQueueState, SnapshotV2VsockState,
};
use serde::Serialize;
use serde::ser::{SerializeSeq, SerializeStruct};

use super::super::fingerprint::{HexU8, HexU64, Redacted, RedactedOption, confidential_bytes};
use super::shared::{EntropyRateConfig, SerialRateConfig, SerialRegisters, Transport, Virtio};

pub(super) struct Serial<'a>(pub(super) &'a SnapshotV2SerialState);

impl Serialize for Serial<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("SerialState", 4)?;
        state.serialize_field("compatibility", "v2.7")?;
        state.serialize_field("endpoint", &SerialEndpoint(value.endpoint_intent()))?;
        state.serialize_field("rate_limiter", &SerialRateConfig(value.rate_limiter()))?;
        state.serialize_field("device", &SerialDevice(value.device()))?;
        state.end()
    }
}

struct SerialEndpoint<'a>(&'a SnapshotV2SerialEndpointIntent);

impl Serialize for SerialEndpoint<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let configured = self.0.configured_selector().is_some();
        let kind = if configured {
            "configured-output"
        } else {
            "default-process-stdio"
        };
        let mut state = serializer.serialize_struct("SerialEndpointIntent", 3)?;
        state.serialize_field("kind", kind)?;
        state.serialize_field("selector", &RedactedOption(configured))?;
        state.serialize_field("process_authority", &RedactedOption(!configured))?;
        state.end()
    }
}

struct SerialDevice<'a>(&'a SerialMmioCaptureState);

impl Serialize for SerialDevice<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let receive = value.receive_bytes();
        let mut state = serializer.serialize_struct("SerialDevice", 8)?;
        state.serialize_field("registers", &SerialRegisters(value.legacy_state()))?;
        state.serialize_field(
            "interrupt_identification",
            &HexU8(value.interrupt_identification()),
        )?;
        state.serialize_field("line_status", &HexU8(value.line_status()))?;
        state.serialize_field("modem_status", &HexU8(value.modem_status()))?;
        state.serialize_field("receive_byte_count", &receive.len())?;
        state.serialize_field(
            "receive_bytes",
            &confidential_bytes("devices.serial.receive-buffer", receive),
        )?;
        state.serialize_field(
            "receive_interrupt_intent_pending",
            &value.receive_interrupt_intent_pending(),
        )?;
        state.serialize_field(
            "input_ready_intent_pending",
            &value.input_ready_intent_pending(),
        )?;
        state.end()
    }
}

pub(super) struct Entropy<'a>(pub(super) &'a SnapshotV2EntropyState);

impl Serialize for Entropy<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("EntropyState", 9)?;
        state.serialize_field("compatibility", "v2.8")?;
        state.serialize_field("config", &EntropyConfiguration(value.config()))?;
        state.serialize_field("active_queue", &EntropyQueue(value.active_queue()))?;
        state.serialize_field("limiter", &EntropyLimiter(value.limiter()))?;
        state.serialize_field("retry", &EntropyRetry(value.retry()))?;
        state.serialize_field("pending_work", &value.has_pending_work())?;
        state.serialize_field("virtio", &Virtio(value.virtio()))?;
        state.serialize_field("transport", &Transport(value.transport()))?;
        state.serialize_field("entropy_source_authority", &Redacted)?;
        state.end()
    }
}

struct EntropyConfiguration(EntropyConfig);

impl Serialize for EntropyConfiguration {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("EntropyConfiguration", 1)?;
        state.serialize_field("rate_limiter", &EntropyRateConfig(self.0.rate_limiter()))?;
        state.end()
    }
}

struct EntropyQueue(Option<SnapshotV2EntropyQueueState>);

impl Serialize for EntropyQueue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(value) = self.0 else {
            return serializer.serialize_none();
        };
        let mut state = serializer.serialize_struct("EntropyQueue", 3)?;
        state.serialize_field("next_available", &value.next_available())?;
        state.serialize_field("next_used", &value.next_used())?;
        state.serialize_field("outstanding", &value.outstanding())?;
        state.end()
    }
}

struct EntropyLimiter(SnapshotV2EntropyLimiterState);

impl Serialize for EntropyLimiter {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("EntropyLimiter", 2)?;
        state.serialize_field("bandwidth", &EntropyBucket(self.0.bandwidth()))?;
        state.serialize_field("ops", &EntropyBucket(self.0.ops()))?;
        state.end()
    }
}

struct EntropyBucket(Option<SnapshotV2EntropyBucketState>);

impl Serialize for EntropyBucket {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(value) = self.0 else {
            return serializer.serialize_none();
        };
        let mut state = serializer.serialize_struct("EntropyBucket", 3)?;
        state.serialize_field("budget", &value.budget())?;
        state.serialize_field("remaining_burst", &value.remaining_burst())?;
        state.serialize_field("age_nanos", &value.age_nanos())?;
        state.end()
    }
}

struct EntropyRetry(SnapshotV2EntropyRetryState);

impl Serialize for EntropyRetry {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let (disposition, remaining_nanos) = match self.0 {
            SnapshotV2EntropyRetryState::None => ("none", None),
            SnapshotV2EntropyRetryState::Immediate => ("immediate", None),
            SnapshotV2EntropyRetryState::After { remaining_nanos } => {
                ("after", Some(remaining_nanos))
            }
        };
        let mut state = serializer.serialize_struct("EntropyRetry", 2)?;
        state.serialize_field("disposition", disposition)?;
        state.serialize_field("remaining_nanos", &remaining_nanos)?;
        state.end()
    }
}

pub(super) struct Balloon<'a>(pub(super) &'a SnapshotV2BalloonState);

impl Serialize for Balloon<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let config = value.config();
        let config_space = value.config_space();
        let mut state = serializer.serialize_struct("BalloonState", 8)?;
        state.serialize_field("compatibility", "v2.9")?;
        state.serialize_field("config", &BalloonConfigView(config))?;
        state.serialize_field("config_space", &BalloonConfigSpace(config_space))?;
        state.serialize_field("continuation", &BalloonContinuation(value.continuation()))?;
        state.serialize_field("accounting", &BalloonAccounting(value.accounting()))?;
        state.serialize_field("virtio", &Virtio(value.virtio()))?;
        state.serialize_field("transport", &Transport(value.transport()))?;
        state.serialize_field("host_reclaim_authority", &Redacted)?;
        state.end()
    }
}

struct BalloonConfigView(bangbang_runtime::balloon::BalloonConfig);

impl Serialize for BalloonConfigView {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("BalloonConfig", 5)?;
        state.serialize_field("amount_mib", &value.amount_mib())?;
        state.serialize_field("deflate_on_oom", &value.deflate_on_oom())?;
        state.serialize_field(
            "stats_polling_interval_s",
            &value.stats_polling_interval_s(),
        )?;
        state.serialize_field("free_page_hinting", &value.free_page_hinting())?;
        state.serialize_field("free_page_reporting", &value.free_page_reporting())?;
        state.end()
    }
}

struct BalloonConfigSpace(bangbang_runtime::balloon::VirtioBalloonConfigSpace);

impl Serialize for BalloonConfigSpace {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("BalloonConfigSpace", 3)?;
        state.serialize_field("num_pages", &value.num_pages())?;
        state.serialize_field("actual_pages", &value.actual_pages())?;
        state.serialize_field("free_page_hint_cmd_id", &value.free_page_hint_cmd_id())?;
        state.end()
    }
}

struct BalloonContinuation<'a>(&'a SnapshotV2BalloonContinuationState);

impl Serialize for BalloonContinuation<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("BalloonContinuation", 5)?;
        state.serialize_field("active_queues", &BalloonActiveQueues(value.active_queues()))?;
        state.serialize_field(
            "stats_polling_interval_s",
            &value.stats_polling_interval_s(),
        )?;
        state.serialize_field("statistics", &BalloonStatistics(value.statistics()))?;
        state.serialize_field(
            "statistics_pending_descriptor_head",
            &value.statistics_pending_descriptor_head(),
        )?;
        state.serialize_field("hinting", &BalloonHint(value.hinting()))?;
        state.end()
    }
}

struct BalloonActiveQueues(Option<SnapshotV2BalloonActiveQueuesState>);

impl Serialize for BalloonActiveQueues {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(value) = self.0 else {
            return serializer.serialize_none();
        };
        let mut state = serializer.serialize_struct("BalloonActiveQueues", 5)?;
        state.serialize_field("inflate", &BalloonQueue(value.inflate()))?;
        state.serialize_field("deflate", &BalloonQueue(value.deflate()))?;
        state.serialize_field("statistics", &OptionalBalloonQueue(value.statistics()))?;
        state.serialize_field(
            "free_page_hinting",
            &OptionalBalloonQueue(value.free_page_hinting()),
        )?;
        state.serialize_field(
            "free_page_reporting",
            &OptionalBalloonQueue(value.free_page_reporting()),
        )?;
        state.end()
    }
}

struct BalloonQueue(SnapshotV2BalloonQueueState);

impl Serialize for BalloonQueue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("BalloonQueue", 3)?;
        state.serialize_field("next_available", &self.0.next_available())?;
        state.serialize_field("next_used", &self.0.next_used())?;
        state.serialize_field("outstanding", &self.0.outstanding())?;
        state.end()
    }
}

struct OptionalBalloonQueue(Option<SnapshotV2BalloonQueueState>);

impl Serialize for OptionalBalloonQueue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.0 {
            Some(value) => BalloonQueue(value).serialize(serializer),
            None => serializer.serialize_none(),
        }
    }
}

struct BalloonStatistics(SnapshotV2BalloonStatistics);

impl Serialize for BalloonStatistics {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let values = self.0.values();
        let mut state = serializer.serialize_struct("BalloonStatistics", 16)?;
        state.serialize_field("swap_in", &values[0])?;
        state.serialize_field("swap_out", &values[1])?;
        state.serialize_field("major_faults", &values[2])?;
        state.serialize_field("minor_faults", &values[3])?;
        state.serialize_field("free_memory", &values[4])?;
        state.serialize_field("total_memory", &values[5])?;
        state.serialize_field("available_memory", &values[6])?;
        state.serialize_field("disk_caches", &values[7])?;
        state.serialize_field("hugetlb_allocations", &values[8])?;
        state.serialize_field("hugetlb_failures", &values[9])?;
        state.serialize_field("oom_kill", &values[10])?;
        state.serialize_field("alloc_stall", &values[11])?;
        state.serialize_field("async_scan", &values[12])?;
        state.serialize_field("direct_scan", &values[13])?;
        state.serialize_field("async_reclaim", &values[14])?;
        state.serialize_field("direct_reclaim", &values[15])?;
        state.end()
    }
}

struct BalloonHint(SnapshotV2BalloonHintState);

impl Serialize for BalloonHint {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("BalloonHint", 4)?;
        state.serialize_field("host_cmd", &self.0.host_cmd())?;
        state.serialize_field("guest_cmd", &self.0.guest_cmd())?;
        state.serialize_field("last_cmd", &self.0.last_cmd())?;
        state.serialize_field("acknowledge_on_stop", &self.0.acknowledge_on_stop())?;
        state.end()
    }
}

struct BalloonAccounting<'a>(&'a SnapshotV2BalloonAccountingState);

impl Serialize for BalloonAccounting<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("BalloonAccounting", 3)?;
        state.serialize_field("inflated_page_count", &self.0.inflated_page_count())?;
        state.serialize_field("range_count", &self.0.ranges().len())?;
        state.serialize_field("ranges", &BalloonRanges(self.0.ranges()))?;
        state.end()
    }
}

struct BalloonRanges<'a>(&'a [SnapshotV2BalloonPfnRange]);

impl Serialize for BalloonRanges<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for range in self.0 {
            sequence.serialize_element(&BalloonRange(*range))?;
        }
        sequence.end()
    }
}

struct BalloonRange(SnapshotV2BalloonPfnRange);

impl Serialize for BalloonRange {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("BalloonRange", 2)?;
        state.serialize_field("start_pfn", &self.0.start_pfn())?;
        state.serialize_field("page_count", &self.0.page_count())?;
        state.end()
    }
}

pub(super) struct MemoryHotplug<'a>(pub(super) &'a SnapshotV2MemoryHotplugState);

impl Serialize for MemoryHotplug<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let config = value.config();
        let config_space = value.config_space();
        let bitmap = value.plugged_bitmap();
        let ranges = value.plugged_ranges();
        let configured_block_count = config_space
            .region_size()
            .checked_div(config_space.block_size());
        let mut state = serializer.serialize_struct("MemoryHotplugState", 11)?;
        state.serialize_field("compatibility", "v2.10")?;
        state.serialize_field("config", &MemoryHotplugConfig(config))?;
        state.serialize_field("config_space", &MemoryHotplugConfigSpace(config_space))?;
        state.serialize_field("active_queue", &MemoryHotplugQueue(value.active_queue()))?;
        state.serialize_field("configured_block_count", &configured_block_count)?;
        state.serialize_field("plugged_range_count", &ranges.len())?;
        state.serialize_field(
            "plugged_ranges",
            &MemoryHotplugRanges(value.plugged_ranges()),
        )?;
        state.serialize_field("plugged_bitmap_byte_count", &bitmap.len())?;
        state.serialize_field(
            "plugged_bitmap",
            &confidential_bytes("devices.memory-hotplug.plugged-bitmap", bitmap),
        )?;
        state.serialize_field("virtio", &Virtio(value.virtio()))?;
        state.serialize_field("transport", &Transport(value.transport()))?;
        state.end()
    }
}

struct MemoryHotplugConfig(bangbang_runtime::memory_hotplug::MemoryHotplugConfig);

impl Serialize for MemoryHotplugConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("MemoryHotplugConfig", 3)?;
        state.serialize_field("total_size_mib", &value.total_size_mib())?;
        state.serialize_field("block_size_mib", &value.block_size_mib())?;
        state.serialize_field("slot_size_mib", &value.slot_size_mib())?;
        state.end()
    }
}

struct MemoryHotplugConfigSpace(bangbang_runtime::memory_hotplug::VirtioMemConfigSpace);

impl Serialize for MemoryHotplugConfigSpace {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("MemoryHotplugConfigSpace", 7)?;
        state.serialize_field("block_size", &value.block_size())?;
        state.serialize_field("node_id", &value.node_id())?;
        state.serialize_field("address", &HexU64(value.addr()))?;
        state.serialize_field("region_size", &value.region_size())?;
        state.serialize_field("usable_region_size", &value.usable_region_size())?;
        state.serialize_field("plugged_size", &value.plugged_size())?;
        state.serialize_field("requested_size", &value.requested_size())?;
        state.end()
    }
}

struct MemoryHotplugQueue(Option<SnapshotV2MemoryHotplugQueueState>);

impl Serialize for MemoryHotplugQueue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(value) = self.0 else {
            return serializer.serialize_none();
        };
        let mut state = serializer.serialize_struct("MemoryHotplugQueue", 2)?;
        state.serialize_field("next_available", &value.next_available())?;
        state.serialize_field("next_used", &value.next_used())?;
        state.end()
    }
}

struct MemoryHotplugRanges<'a>(
    bangbang_runtime::snapshot_memory_hotplug_v2_10::SnapshotV2MemoryHotplugPluggedRanges<'a>,
);

impl Serialize for MemoryHotplugRanges<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let ranges = self.0.clone();
        let mut sequence = serializer.serialize_seq(Some(ranges.len()))?;
        for range in ranges {
            sequence.serialize_element(&MemoryHotplugRange(range))?;
        }
        sequence.end()
    }
}

struct MemoryHotplugRange(SnapshotV2MemoryHotplugPluggedRange);

impl Serialize for MemoryHotplugRange {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("MemoryHotplugRange", 2)?;
        state.serialize_field("start_block", &self.0.start_block())?;
        state.serialize_field("block_count", &self.0.block_count())?;
        state.end()
    }
}

pub(super) struct Network<'a>(pub(super) &'a SnapshotV2NetworkState);

impl Serialize for Network<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("NetworkState", 4)?;
        state.serialize_field("compatibility", "v2.11")?;
        state.serialize_field("interface_count", &value.interfaces().len())?;
        state.serialize_field("interfaces", &NetworkInterfaces(value.interfaces()))?;
        state.serialize_field("mmds", &OptionalMmds(value.mmds()))?;
        state.end()
    }
}

struct NetworkInterfaces<'a>(&'a [SnapshotV2NetworkInterfaceState]);

impl Serialize for NetworkInterfaces<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for (index, interface) in self.0.iter().enumerate() {
            sequence.serialize_element(&NetworkInterface { index, interface })?;
        }
        sequence.end()
    }
}

struct NetworkInterface<'a> {
    index: usize,
    interface: &'a SnapshotV2NetworkInterfaceState,
}

impl Serialize for NetworkInterface<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.interface;
        let mut state = serializer.serialize_struct("NetworkInterface", 13)?;
        state.serialize_field("index", &self.index)?;
        state.serialize_field("iface_id", value.iface_id())?;
        state.serialize_field("captured_selector", &Redacted)?;
        state.serialize_field(
            "requested_guest_mac",
            &OptionalGuestMac(value.requested_guest_mac()),
        )?;
        state.serialize_field("requested_mtu", &value.requested_mtu())?;
        state.serialize_field("profile", &NetworkProfile(value.profile()))?;
        state.serialize_field("backend", &NetworkBackend(value.backend()))?;
        state.serialize_field("local", &NetworkLocal(value.local()))?;
        state.serialize_field("virtio", &Virtio(value.virtio()))?;
        state.serialize_field("rx_limiter", &NetworkLimiter(value.rx_limiter()))?;
        state.serialize_field("tx_limiter", &NetworkLimiter(value.tx_limiter()))?;
        state.serialize_field("transport", &Transport(value.transport()))?;
        state.serialize_field("packet_io_authority", &Redacted)?;
        state.end()
    }
}

struct OptionalGuestMac(Option<GuestMacAddress>);

impl Serialize for OptionalGuestMac {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.0 {
            Some(value) => serializer.collect_str(&value),
            None => serializer.serialize_none(),
        }
    }
}

struct NetworkProfile(NetworkDeviceProfile);

impl Serialize for NetworkProfile {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("NetworkProfile", 4)?;
        state.serialize_field("guest_mac", &OptionalGuestMac(value.guest_mac()))?;
        state.serialize_field("mtu", &value.mtu())?;
        state.serialize_field("packet_envelope", &PacketEnvelope(value.packet_envelope()))?;
        state.serialize_field(
            "feature_capabilities",
            &NetworkFeatures(value.feature_capabilities()),
        )?;
        state.end()
    }
}

struct PacketEnvelope(VirtioNetworkPacketEnvelope);

impl Serialize for PacketEnvelope {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(match self.0 {
            VirtioNetworkPacketEnvelope::RawEthernet => "raw-ethernet",
            VirtioNetworkPacketEnvelope::DirectVirtioHeader => "direct-virtio-header",
        })
    }
}

struct NetworkFeatures(VirtioNetworkFeatureCapabilities);

impl Serialize for NetworkFeatures {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("NetworkFeatures", 10)?;
        state.serialize_field("checksum", &value.checksum())?;
        state.serialize_field("guest_checksum", &value.guest_checksum())?;
        state.serialize_field("guest_tso4", &value.guest_tso4())?;
        state.serialize_field("guest_tso6", &value.guest_tso6())?;
        state.serialize_field("guest_ufo", &value.guest_ufo())?;
        state.serialize_field("host_tso4", &value.host_tso4())?;
        state.serialize_field("host_tso6", &value.host_tso6())?;
        state.serialize_field("host_ufo", &value.host_ufo())?;
        state.serialize_field("merged_rx_buffers", &value.merged_rx_buffers())?;
        state.serialize_field("feature_bits", &HexU64(value.feature_bits()))?;
        state.end()
    }
}

struct NetworkBackend(SnapshotV2NetworkBackendClass);

impl Serialize for NetworkBackend {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(match self.0 {
            SnapshotV2NetworkBackendClass::MmdsOnly => "mmds-only",
            SnapshotV2NetworkBackendClass::Vmnet => "vmnet",
        })
    }
}

struct NetworkLocal<'a>(&'a SnapshotV2NetworkLocalState);

impl Serialize for NetworkLocal<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("NetworkLocal", 3)?;
        state.serialize_field("active_rx_queue", &NetworkQueue(self.0.active_rx_queue()))?;
        state.serialize_field("active_tx_queue", &NetworkQueue(self.0.active_tx_queue()))?;
        state.serialize_field("tx_retry", &NetworkRetry(self.0.tx_retry()))?;
        state.end()
    }
}

struct NetworkQueue(Option<SnapshotV2NetworkQueueState>);

impl Serialize for NetworkQueue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(value) = self.0 else {
            return serializer.serialize_none();
        };
        let mut state = serializer.serialize_struct("NetworkQueue", 2)?;
        state.serialize_field("next_available", &value.next_available())?;
        state.serialize_field("next_used", &value.next_used())?;
        state.end()
    }
}

struct NetworkRetry(SnapshotV2NetworkRetryState);

impl Serialize for NetworkRetry {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let (disposition, remaining_nanos) = match self.0 {
            SnapshotV2NetworkRetryState::None => ("none", None),
            SnapshotV2NetworkRetryState::Immediate => ("immediate", None),
            SnapshotV2NetworkRetryState::After { remaining_nanos } => {
                ("after", Some(remaining_nanos))
            }
        };
        let mut state = serializer.serialize_struct("NetworkRetry", 2)?;
        state.serialize_field("disposition", disposition)?;
        state.serialize_field("remaining_nanos", &remaining_nanos)?;
        state.end()
    }
}

struct NetworkLimiter(SnapshotV2NetworkLimiterState);

impl Serialize for NetworkLimiter {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("NetworkLimiter", 2)?;
        state.serialize_field("bandwidth", &NetworkBucket(self.0.bandwidth()))?;
        state.serialize_field("ops", &NetworkBucket(self.0.ops()))?;
        state.end()
    }
}

struct NetworkBucket(Option<SnapshotV2NetworkTokenBucketState>);

impl Serialize for NetworkBucket {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(value) = self.0 else {
            return serializer.serialize_none();
        };
        let mut state = serializer.serialize_struct("NetworkBucket", 6)?;
        state.serialize_field("size", &value.size())?;
        state.serialize_field("configured_burst", &value.configured_burst())?;
        state.serialize_field("refill_time_millis", &value.refill_time_millis())?;
        state.serialize_field("budget", &value.budget())?;
        state.serialize_field("remaining_burst", &value.remaining_burst())?;
        state.serialize_field("age_nanos", &value.age_nanos())?;
        state.end()
    }
}

struct OptionalMmds<'a>(Option<&'a SnapshotV2MmdsState>);

impl Serialize for OptionalMmds<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.0 {
            Some(value) => Mmds(value).serialize(serializer),
            None => serializer.serialize_none(),
        }
    }
}

struct Mmds<'a>(&'a SnapshotV2MmdsState);

impl Serialize for Mmds<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("Mmds", 6)?;
        state.serialize_field("version", &MmdsVersion(value.version()))?;
        state.serialize_field("ipv4_address", &OptionalIpv4(value.ipv4_address()))?;
        state.serialize_field(
            "effective_ipv4_address",
            &Ipv4(value.effective_ipv4_address()),
        )?;
        state.serialize_field("imds_compat", &value.imds_compat())?;
        state.serialize_field("interface_count", &value.interfaces().len())?;
        state.serialize_field("interfaces", &MmdsInterfaces(value.interfaces()))?;
        state.end()
    }
}

struct MmdsVersion(bangbang_runtime::mmds::MmdsVersion);

impl Serialize for MmdsVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(match self.0 {
            bangbang_runtime::mmds::MmdsVersion::V1 => "v1",
            bangbang_runtime::mmds::MmdsVersion::V2 => "v2",
        })
    }
}

struct OptionalIpv4(Option<std::net::Ipv4Addr>);

impl Serialize for OptionalIpv4 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.0 {
            Some(value) => serializer.collect_str(&value),
            None => serializer.serialize_none(),
        }
    }
}

struct Ipv4(std::net::Ipv4Addr);

impl Serialize for Ipv4 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(&self.0)
    }
}

struct MmdsInterfaces<'a>(&'a [SnapshotV2MmdsInterfaceState]);

impl Serialize for MmdsInterfaces<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for value in self.0 {
            sequence.serialize_element(&MmdsInterface(*value))?;
        }
        sequence.end()
    }
}

struct MmdsInterface(SnapshotV2MmdsInterfaceState);

impl Serialize for MmdsInterface {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("MmdsInterface", 4)?;
        state.serialize_field("interface_index", &self.0.interface_index())?;
        state.serialize_field(
            "local_mac_address",
            &Mac(self.0.local_mac_address().octets()),
        )?;
        state.serialize_field("ipv4_address", &Ipv4(self.0.ipv4_address()))?;
        state.serialize_field("tcp_port", &self.0.tcp_port())?;
        state.end()
    }
}

struct Mac([u8; 6]);

impl fmt::Display for Mac {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d, e, f] = self.0;
        write!(formatter, "{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{f:02x}")
    }
}

impl Serialize for Mac {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(self)
    }
}

pub(super) struct Vsock<'a>(pub(super) &'a SnapshotV2VsockState);

impl Serialize for Vsock<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("VsockState", 8)?;
        state.serialize_field("compatibility", "v2.12")?;
        state.serialize_field("guest_cid", &value.guest_cid())?;
        state.serialize_field("backend_selector", &Redacted)?;
        state.serialize_field("host_local_port_cursor", &Redacted)?;
        state.serialize_field("active_queues", &VsockActiveQueues(value.active_queues()))?;
        state.serialize_field("virtio", &Virtio(value.virtio()))?;
        state.serialize_field("transport", &Transport(value.transport()))?;
        state.serialize_field("socket_authority", &Redacted)?;
        state.end()
    }
}

struct VsockActiveQueues(Option<SnapshotV2VsockActiveQueuesState>);

impl Serialize for VsockActiveQueues {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(value) = self.0 else {
            return serializer.serialize_none();
        };
        let mut state = serializer.serialize_struct("VsockActiveQueues", 3)?;
        state.serialize_field("rx", &VsockQueue(value.rx()))?;
        state.serialize_field("tx", &VsockQueue(value.tx()))?;
        state.serialize_field("event", &VsockQueue(value.event()))?;
        state.end()
    }
}

struct VsockQueue(SnapshotV2VsockQueueState);

impl Serialize for VsockQueue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("VsockQueue", 3)?;
        state.serialize_field("next_available", &self.0.next_available())?;
        state.serialize_field("next_used", &self.0.next_used())?;
        state.serialize_field("event_idx_enabled", &self.0.event_idx_enabled())?;
        state.end()
    }
}
