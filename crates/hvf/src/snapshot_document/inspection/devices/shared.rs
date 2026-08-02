use bangbang_runtime::block::{
    DriveCacheType, DriveIoEngine, DriveRateLimiterConfig, DriveTokenBucketConfig,
    VirtioBlockQueueState, VirtioBlockRateLimiterState, VirtioBlockTokenBucketState,
};
use bangbang_runtime::entropy::{EntropyRateLimiterConfig, EntropyTokenBucketConfig};
use bangbang_runtime::interrupt::DeviceInterruptStatus;
use bangbang_runtime::mmio::MmioRegion;
use bangbang_runtime::pci::{PciBarAddressSpace, PciBarPrefetchable, PciSbdf};
use bangbang_runtime::pmem::{PmemRateLimiterConfig, PmemTokenBucketConfig, VirtioPmemQueueState};
use bangbang_runtime::serial::{SerialMmioState, SerialRateLimiterConfig};
use bangbang_runtime::snapshot_device_v2::{
    SnapshotV2BlockBucketState, SnapshotV2BlockLimiterState, SnapshotV2DeviceKey,
    SnapshotV2DeviceTransport, SnapshotV2DeviceTransportKind, SnapshotV2InterruptIntent,
    SnapshotV2MmioDeviceState, SnapshotV2PciBarProbeState, SnapshotV2PciDeviceState,
    SnapshotV2PciMsixState, SnapshotV2PciMsixTableEntry, SnapshotV2PciWritableByte,
    SnapshotV2VirtioQueueState, SnapshotV2VirtioState,
};
use bangbang_runtime::snapshot_device_v2_6::{
    SnapshotV2PmemBucketState, SnapshotV2PmemLimiterState,
};
use bangbang_runtime::storage_capture::{StorageDeviceOrigin, StorageRetryState};
use bangbang_runtime::virtio_mmio::{
    VirtioMmioDeviceRegisters, VirtioMmioQueueState, VirtioMmioTransportState,
};
use bangbang_runtime::virtio_pci::VirtioPciEndpointPhase;
use serde::Serialize;
use serde::ser::{SerializeSeq, SerializeStruct};

use super::super::common::GuestRange;
use super::super::fingerprint::{HexU8, HexU32, HexU64};

pub(super) struct DeviceKey(pub(super) SnapshotV2DeviceKey);

impl Serialize for DeviceKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("DeviceKey", 2)?;
        state.serialize_field("kind", &self.0.kind())?;
        state.serialize_field("instance", &self.0.instance())?;
        state.end()
    }
}

pub(super) struct OptionalDeviceKey(pub(super) Option<SnapshotV2DeviceKey>);

impl Serialize for OptionalDeviceKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.0 {
            Some(key) => DeviceKey(key).serialize(serializer),
            None => serializer.serialize_none(),
        }
    }
}

pub(super) struct TransportKind(pub(super) SnapshotV2DeviceTransportKind);

impl Serialize for TransportKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(match self.0 {
            SnapshotV2DeviceTransportKind::Mmio => "mmio",
            SnapshotV2DeviceTransportKind::Pci => "pci",
        })
    }
}

pub(super) struct MmioRegionView(pub(super) MmioRegion);

impl Serialize for MmioRegionView {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("MmioRegion", 2)?;
        state.serialize_field("id", &self.0.id().raw_value())?;
        state.serialize_field("range", &GuestRange(self.0.range()))?;
        state.end()
    }
}

struct PciIdentity(PciSbdf);

impl Serialize for PciIdentity {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("PciIdentity", 4)?;
        state.serialize_field("segment", &self.0.segment())?;
        state.serialize_field("bus", &self.0.bus())?;
        state.serialize_field("device", &self.0.device())?;
        state.serialize_field("function", &self.0.function())?;
        state.end()
    }
}

struct PciAddressSpace(PciBarAddressSpace);

impl Serialize for PciAddressSpace {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(match self.0 {
            PciBarAddressSpace::Memory32 => "memory-32",
            PciBarAddressSpace::Memory64 => "memory-64",
        })
    }
}

struct PciPrefetchable(PciBarPrefetchable);

impl Serialize for PciPrefetchable {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_bool(matches!(self.0, PciBarPrefetchable::Yes))
    }
}

struct PciPhase(VirtioPciEndpointPhase);

impl Serialize for PciPhase {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(match self.0 {
            VirtioPciEndpointPhase::Active => "active",
            VirtioPciEndpointPhase::Quiescing => "quiescing",
            VirtioPciEndpointPhase::Released => "released",
        })
    }
}

struct Origin(StorageDeviceOrigin);

impl Serialize for Origin {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(match self.0 {
            StorageDeviceOrigin::Startup => "startup",
            StorageDeviceOrigin::Runtime => "runtime",
        })
    }
}

pub(super) struct Retry(pub(super) StorageRetryState);

impl Serialize for Retry {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let (disposition, remaining_nanos) = match self.0 {
            StorageRetryState::None => ("none", None),
            StorageRetryState::Immediate => ("immediate", None),
            StorageRetryState::After { remaining_nanos } => ("after", Some(remaining_nanos)),
        };
        let mut state = serializer.serialize_struct("StorageRetry", 2)?;
        state.serialize_field("disposition", disposition)?;
        state.serialize_field("remaining_nanos", &remaining_nanos)?;
        state.end()
    }
}

pub(super) struct DriveCache(pub(super) DriveCacheType);

impl Serialize for DriveCache {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(match self.0 {
            DriveCacheType::Unsafe => "unsafe",
            DriveCacheType::Writeback => "writeback",
        })
    }
}

pub(super) struct DriveEngine(pub(super) DriveIoEngine);

impl Serialize for DriveEngine {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(match self.0 {
            DriveIoEngine::Sync => "sync",
            DriveIoEngine::Async => "async",
        })
    }
}

pub(super) struct Virtio<'a>(pub(super) &'a SnapshotV2VirtioState);

impl Serialize for Virtio<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("VirtioState", 9)?;
        state.serialize_field("available_features", &HexU64(value.available_features()))?;
        state.serialize_field("driver_features", &HexU64(value.driver_features()))?;
        state.serialize_field("config_generation", &value.config_generation())?;
        state.serialize_field("status", &HexU32(value.status()))?;
        state.serialize_field("activated", &value.is_activated())?;
        state.serialize_field("queues", &VirtioQueues(value.queues()))?;
        state.serialize_field("pending_notifications", &value.pending_notifications())?;
        state.serialize_field(
            "interrupt_intents",
            &InterruptIntents(value.interrupt_intents()),
        )?;
        state.serialize_field("queue_count", &value.queues().len())?;
        state.end()
    }
}

struct VirtioQueues<'a>(&'a [SnapshotV2VirtioQueueState]);

impl Serialize for VirtioQueues<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for (index, queue) in self.0.iter().enumerate() {
            sequence.serialize_element(&VirtioQueue {
                index,
                queue: *queue,
            })?;
        }
        sequence.end()
    }
}

struct VirtioQueue {
    index: usize,
    queue: SnapshotV2VirtioQueueState,
}

impl Serialize for VirtioQueue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("VirtioQueue", 7)?;
        state.serialize_field("index", &self.index)?;
        state.serialize_field("max_size", &self.queue.max_size())?;
        state.serialize_field("size", &self.queue.size())?;
        state.serialize_field("ready", &self.queue.ready())?;
        state.serialize_field(
            "descriptor_table",
            &HexU64(self.queue.descriptor_table().raw_value()),
        )?;
        state.serialize_field("driver_ring", &HexU64(self.queue.driver_ring().raw_value()))?;
        state.serialize_field("device_ring", &HexU64(self.queue.device_ring().raw_value()))?;
        state.end()
    }
}

struct InterruptIntents<'a>(&'a [SnapshotV2InterruptIntent]);

impl Serialize for InterruptIntents<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for intent in self.0 {
            sequence.serialize_element(&InterruptIntent(*intent))?;
        }
        sequence.end()
    }
}

struct InterruptIntent(SnapshotV2InterruptIntent);

impl Serialize for InterruptIntent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let (kind, queue_index) = match self.0 {
            SnapshotV2InterruptIntent::Queue { queue_index } => ("queue", Some(queue_index)),
            SnapshotV2InterruptIntent::Configuration => ("configuration", None),
        };
        let mut state = serializer.serialize_struct("InterruptIntent", 2)?;
        state.serialize_field("kind", kind)?;
        state.serialize_field("queue_index", &queue_index)?;
        state.end()
    }
}

pub(super) struct Transport<'a>(pub(super) &'a SnapshotV2DeviceTransport);

impl Serialize for Transport<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.0 {
            SnapshotV2DeviceTransport::Mmio(mmio) => MmioTransport(mmio).serialize(serializer),
            SnapshotV2DeviceTransport::Pci(pci) => PciTransport(pci).serialize(serializer),
        }
    }
}

struct MmioTransport<'a>(&'a SnapshotV2MmioDeviceState);

impl Serialize for MmioTransport<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("VirtioMmioTransport", 7)?;
        state.serialize_field("kind", "mmio")?;
        state.serialize_field("device_feature_select", &value.device_feature_select())?;
        state.serialize_field("driver_feature_select", &value.driver_feature_select())?;
        state.serialize_field("queue_select", &value.queue_select())?;
        state.serialize_field("region", &MmioRegionView(value.region()))?;
        state.serialize_field("interrupt_line", &value.interrupt_line().raw_value())?;
        state.serialize_field("placement", "guest-visible")?;
        state.end()
    }
}

struct PciTransport<'a>(&'a SnapshotV2PciDeviceState);

impl Serialize for PciTransport<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("VirtioPciTransport", 18)?;
        state.serialize_field("kind", "pci")?;
        state.serialize_field("phase", &PciPhase(value.phase()))?;
        state.serialize_field("origin", &Origin(value.origin()))?;
        state.serialize_field("sbdf", &PciIdentity(value.sbdf()))?;
        state.serialize_field("bar_index", &value.bar_index())?;
        state.serialize_field(
            "bar_address_space",
            &PciAddressSpace(value.bar_address_space()),
        )?;
        state.serialize_field(
            "bar_prefetchable",
            &PciPrefetchable(value.bar_prefetchable()),
        )?;
        state.serialize_field("bar_range", &GuestRange(value.bar_range()))?;
        state.serialize_field("device_feature_select", &value.device_feature_select())?;
        state.serialize_field("driver_feature_select", &value.driver_feature_select())?;
        state.serialize_field("queue_select", &value.queue_select())?;
        state.serialize_field("pci_cfg_bar", &value.pci_cfg_bar())?;
        state.serialize_field("pci_cfg_offset", &value.pci_cfg_offset())?;
        state.serialize_field("pci_cfg_length", &value.pci_cfg_length())?;
        state.serialize_field("writable_bytes", &PciWritableBytes(value.writable_bytes()))?;
        state.serialize_field("bar_probes", &PciBarProbes(value.bar_probes()))?;
        state.serialize_field("msix", &PciMsix(value.msix()))?;
        state.serialize_field("placement", "guest-visible")?;
        state.end()
    }
}

struct PciWritableBytes<'a>(&'a [SnapshotV2PciWritableByte]);

impl Serialize for PciWritableBytes<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for byte in self.0 {
            sequence.serialize_element(&PciWritableByte(*byte))?;
        }
        sequence.end()
    }
}

struct PciWritableByte(SnapshotV2PciWritableByte);

impl Serialize for PciWritableByte {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("PciWritableByte", 2)?;
        state.serialize_field("offset", &self.0.offset())?;
        state.serialize_field("value", &HexU8(self.0.value()))?;
        state.end()
    }
}

struct PciBarProbes<'a>(&'a [SnapshotV2PciBarProbeState]);

impl Serialize for PciBarProbes<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for probe in self.0 {
            sequence.serialize_element(&PciBarProbe(*probe))?;
        }
        sequence.end()
    }
}

struct PciBarProbe(SnapshotV2PciBarProbeState);

impl Serialize for PciBarProbe {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("PciBarProbe", 2)?;
        state.serialize_field("index", &self.0.index())?;
        state.serialize_field("pending", &self.0.pending())?;
        state.end()
    }
}

struct PciMsix<'a>(&'a SnapshotV2PciMsixState);

impl Serialize for PciMsix<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("PciMsix", 8)?;
        state.serialize_field("entries", &PciMsixEntries(value.entries()))?;
        state.serialize_field("pending_words", &HexU64Values(value.pending_words()))?;
        state.serialize_field("enabled", &value.enabled())?;
        state.serialize_field("function_masked", &value.function_masked())?;
        state.serialize_field("config_vector", &value.config_vector())?;
        state.serialize_field("queue_vectors", &value.queue_vectors())?;
        state.serialize_field(
            "pending_transition_observed",
            &value.pending_transition_observed(),
        )?;
        state.serialize_field("entry_count", &value.entries().len())?;
        state.end()
    }
}

struct PciMsixEntries<'a>(&'a [SnapshotV2PciMsixTableEntry]);

impl Serialize for PciMsixEntries<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for entry in self.0 {
            sequence.serialize_element(&PciMsixEntry(*entry))?;
        }
        sequence.end()
    }
}

struct PciMsixEntry(SnapshotV2PciMsixTableEntry);

impl Serialize for PciMsixEntry {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("PciMsixEntry", 4)?;
        state.serialize_field("message_address_low", &HexU32(self.0.message_address_low()))?;
        state.serialize_field(
            "message_address_high",
            &HexU32(self.0.message_address_high()),
        )?;
        state.serialize_field("message_data", &HexU32(self.0.message_data()))?;
        state.serialize_field("vector_control", &HexU32(self.0.vector_control()))?;
        state.end()
    }
}

struct HexU64Values<'a>(&'a [u64]);

impl Serialize for HexU64Values<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for value in self.0 {
            sequence.serialize_element(&HexU64(*value))?;
        }
        sequence.end()
    }
}

pub(super) struct LegacyVirtioMmio<'a>(pub(super) &'a VirtioMmioTransportState);

impl Serialize for LegacyVirtioMmio<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("LegacyVirtioMmio", 8)?;
        state.serialize_field("device", &LegacyVirtioDevice(*value.device_registers()))?;
        state.serialize_field("queue_select", &value.queue_select())?;
        state.serialize_field("queues", &LegacyVirtioQueues(value.queues()))?;
        state.serialize_field("pending_notifications", &value.pending_notifications())?;
        state.serialize_field(
            "interrupt_status",
            &InterruptStatus(value.interrupt_status()),
        )?;
        state.serialize_field("activated", &value.is_device_activated())?;
        state.serialize_field(
            "requires_device_config_write_status",
            &value.requires_device_config_write_status(),
        )?;
        state.serialize_field("queue_count", &value.queues().len())?;
        state.end()
    }
}

struct LegacyVirtioDevice(VirtioMmioDeviceRegisters);

impl Serialize for LegacyVirtioDevice {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("LegacyVirtioDevice", 8)?;
        state.serialize_field("device_id", &value.device_id())?;
        state.serialize_field("vendor_id", &HexU32(value.vendor_id()))?;
        state.serialize_field("device_features", &HexU64(value.device_features()))?;
        state.serialize_field("config_generation", &value.config_generation())?;
        state.serialize_field("device_features_select", &value.device_features_select())?;
        state.serialize_field("driver_features_select", &value.driver_features_select())?;
        state.serialize_field("driver_features", &HexU64(value.driver_features()))?;
        state.serialize_field("status", &HexU32(value.status()))?;
        state.end()
    }
}

struct LegacyVirtioQueues<'a>(&'a [VirtioMmioQueueState]);

impl Serialize for LegacyVirtioQueues<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for (index, queue) in self.0.iter().enumerate() {
            sequence.serialize_element(&LegacyVirtioQueue {
                index,
                queue: *queue,
            })?;
        }
        sequence.end()
    }
}

struct LegacyVirtioQueue {
    index: usize,
    queue: VirtioMmioQueueState,
}

impl Serialize for LegacyVirtioQueue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("LegacyVirtioQueue", 7)?;
        state.serialize_field("index", &self.index)?;
        state.serialize_field("max_size", &self.queue.max_size())?;
        state.serialize_field("size", &self.queue.size())?;
        state.serialize_field("ready", &self.queue.ready())?;
        state.serialize_field(
            "descriptor_table",
            &HexU64(self.queue.descriptor_table().raw_value()),
        )?;
        state.serialize_field("driver_ring", &HexU64(self.queue.driver_ring().raw_value()))?;
        state.serialize_field("device_ring", &HexU64(self.queue.device_ring().raw_value()))?;
        state.end()
    }
}

struct InterruptStatus(DeviceInterruptStatus);

impl Serialize for InterruptStatus {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_some(&HexU32(self.0.bits()))
    }
}

pub(super) struct SerialRegisters(pub(super) SerialMmioState);

impl Serialize for SerialRegisters {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("SerialRegisters", 6)?;
        state.serialize_field("interrupt_enable", &HexU8(value.interrupt_enable()))?;
        state.serialize_field("line_control", &HexU8(value.line_control()))?;
        state.serialize_field("modem_control", &HexU8(value.modem_control()))?;
        state.serialize_field("scratch", &HexU8(value.scratch()))?;
        state.serialize_field("divisor_latch_low", &HexU8(value.divisor_latch_low()))?;
        state.serialize_field("divisor_latch_high", &HexU8(value.divisor_latch_high()))?;
        state.end()
    }
}

pub(super) struct BlockQueue(pub(super) Option<VirtioBlockQueueState>);

impl Serialize for BlockQueue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(queue) = self.0 else {
            return serializer.serialize_none();
        };
        let mut state = serializer.serialize_struct("BlockQueue", 2)?;
        state.serialize_field("next_available", &queue.next_available())?;
        state.serialize_field("next_used", &queue.next_used())?;
        state.end()
    }
}

pub(super) struct PmemQueue(pub(super) Option<VirtioPmemQueueState>);

impl Serialize for PmemQueue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(queue) = self.0 else {
            return serializer.serialize_none();
        };
        let mut state = serializer.serialize_struct("PmemQueue", 2)?;
        state.serialize_field("next_available", &queue.next_available())?;
        state.serialize_field("next_used", &queue.next_used())?;
        state.end()
    }
}

pub(super) struct DriveRateConfig(pub(super) Option<DriveRateLimiterConfig>);

impl Serialize for DriveRateConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(config) = self.0 else {
            return serializer.serialize_none();
        };
        let mut state = serializer.serialize_struct("DriveRateLimiterConfig", 2)?;
        state.serialize_field("bandwidth", &DriveTokenConfig(config.bandwidth()))?;
        state.serialize_field("ops", &DriveTokenConfig(config.ops()))?;
        state.end()
    }
}

struct DriveTokenConfig(Option<DriveTokenBucketConfig>);

impl Serialize for DriveTokenConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(config) = self.0 else {
            return serializer.serialize_none();
        };
        serialize_token_config(
            serializer,
            config.size(),
            config.one_time_burst(),
            config.refill_time(),
        )
    }
}

pub(super) struct PmemRateConfig(pub(super) Option<PmemRateLimiterConfig>);

impl Serialize for PmemRateConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(config) = self.0 else {
            return serializer.serialize_none();
        };
        let mut state = serializer.serialize_struct("PmemRateLimiterConfig", 2)?;
        state.serialize_field("bandwidth", &PmemTokenConfig(config.bandwidth()))?;
        state.serialize_field("ops", &PmemTokenConfig(config.ops()))?;
        state.end()
    }
}

struct PmemTokenConfig(Option<PmemTokenBucketConfig>);

impl Serialize for PmemTokenConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(config) = self.0 else {
            return serializer.serialize_none();
        };
        serialize_token_config(
            serializer,
            config.size(),
            config.one_time_burst(),
            config.refill_time(),
        )
    }
}

pub(super) struct EntropyRateConfig(pub(super) Option<EntropyRateLimiterConfig>);

impl Serialize for EntropyRateConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(config) = self.0 else {
            return serializer.serialize_none();
        };
        let mut state = serializer.serialize_struct("EntropyRateLimiterConfig", 2)?;
        state.serialize_field("bandwidth", &EntropyTokenConfig(config.bandwidth()))?;
        state.serialize_field("ops", &EntropyTokenConfig(config.ops()))?;
        state.end()
    }
}

struct EntropyTokenConfig(Option<EntropyTokenBucketConfig>);

impl Serialize for EntropyTokenConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(config) = self.0 else {
            return serializer.serialize_none();
        };
        serialize_token_config(
            serializer,
            config.size(),
            config.one_time_burst(),
            config.refill_time(),
        )
    }
}

pub(super) struct SerialRateConfig(pub(super) Option<SerialRateLimiterConfig>);

impl Serialize for SerialRateConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(config) = self.0 else {
            return serializer.serialize_none();
        };
        serialize_token_config(
            serializer,
            config.size(),
            config.one_time_burst(),
            config.refill_time(),
        )
    }
}

fn serialize_token_config<S>(
    serializer: S,
    size: u64,
    one_time_burst: Option<u64>,
    refill_time: u64,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let mut state = serializer.serialize_struct("TokenBucketConfig", 3)?;
    state.serialize_field("size", &size)?;
    state.serialize_field("one_time_burst", &one_time_burst)?;
    state.serialize_field("refill_time", &refill_time)?;
    state.end()
}

pub(super) struct LegacyBlockLimiter(pub(super) VirtioBlockRateLimiterState);

impl Serialize for LegacyBlockLimiter {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("LegacyBlockLimiter", 2)?;
        state.serialize_field("bandwidth", &LegacyBlockBucket(self.0.bandwidth()))?;
        state.serialize_field("ops", &LegacyBlockBucket(self.0.ops()))?;
        state.end()
    }
}

struct LegacyBlockBucket(Option<VirtioBlockTokenBucketState>);

impl Serialize for LegacyBlockBucket {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(bucket) = self.0 else {
            return serializer.serialize_none();
        };
        let mut state = serializer.serialize_struct("LegacyBlockBucket", 4)?;
        state.serialize_field("config", &DriveTokenConfig(Some(bucket.config())))?;
        state.serialize_field("budget", &bucket.budget())?;
        state.serialize_field("remaining_burst", &bucket.remaining_burst())?;
        state.serialize_field("age_nanos", &bucket.age_nanos())?;
        state.end()
    }
}

pub(super) struct BlockLimiter(pub(super) SnapshotV2BlockLimiterState);

impl Serialize for BlockLimiter {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("BlockLimiter", 2)?;
        state.serialize_field("bandwidth", &BlockBucket(self.0.bandwidth()))?;
        state.serialize_field("ops", &BlockBucket(self.0.ops()))?;
        state.end()
    }
}

struct BlockBucket(Option<SnapshotV2BlockBucketState>);

impl Serialize for BlockBucket {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(bucket) = self.0 else {
            return serializer.serialize_none();
        };
        serialize_bucket_state(
            serializer,
            bucket.budget(),
            bucket.remaining_burst(),
            bucket.age_nanos(),
        )
    }
}

pub(super) struct PmemLimiter(pub(super) SnapshotV2PmemLimiterState);

impl Serialize for PmemLimiter {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("PmemLimiter", 2)?;
        state.serialize_field("bandwidth", &PmemBucket(self.0.bandwidth()))?;
        state.serialize_field("ops", &PmemBucket(self.0.ops()))?;
        state.end()
    }
}

struct PmemBucket(Option<SnapshotV2PmemBucketState>);

impl Serialize for PmemBucket {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(bucket) = self.0 else {
            return serializer.serialize_none();
        };
        serialize_bucket_state(
            serializer,
            bucket.budget(),
            bucket.remaining_burst(),
            bucket.age_nanos(),
        )
    }
}

fn serialize_bucket_state<S>(
    serializer: S,
    budget: u64,
    remaining_burst: u64,
    age_nanos: u64,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let mut state = serializer.serialize_struct("TokenBucketState", 3)?;
    state.serialize_field("budget", &budget)?;
    state.serialize_field("remaining_burst", &remaining_burst)?;
    state.serialize_field("age_nanos", &age_nanos)?;
    state.end()
}
