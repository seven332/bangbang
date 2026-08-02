use bangbang_runtime::block::DriveIoEngine;
use bangbang_runtime::snapshot_device::{
    SnapshotV1BlockRetryState, SnapshotV1DeviceState, SnapshotV1MmioDeviceMetadata,
    SnapshotV1PlatformDeviceMetadata, SnapshotV1RootBlockState,
};
use bangbang_runtime::snapshot_device_v2::{
    SnapshotV2BlockState, SnapshotV2DeviceGraph, SnapshotV2DeviceRecord,
};
use bangbang_runtime::snapshot_device_v2_5::{
    SnapshotV2MultiBlockDeviceGraph, SnapshotV2MultiBlockDeviceRecord,
};
use bangbang_runtime::snapshot_device_v2_6::{
    SnapshotV2PmemDeviceRecord, SnapshotV2StorageDeviceGraph,
};
use serde::Serialize;
use serde::ser::{SerializeSeq, SerializeStruct};

use super::super::common::GuestRange;
use super::super::fingerprint::{HexU64, Redacted, confidential_bytes};
use super::shared::{
    BlockLimiter, BlockQueue, DeviceKey, DriveCache, DriveEngine, DriveRateConfig,
    LegacyBlockLimiter, LegacyVirtioMmio, MmioRegionView, OptionalDeviceKey, PmemLimiter,
    PmemQueue, PmemRateConfig, Retry, SerialRegisters, Transport, TransportKind, Virtio,
};

pub(super) struct LegacyDevices<'a>(pub(super) &'a SnapshotV1DeviceState);

impl Serialize for LegacyDevices<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("LegacyDevices", 6)?;
        state.serialize_field("root_block", &LegacyRootBlock(value.root_block()))?;
        state.serialize_field("block_retry", &LegacyRetry(value.block_retry()))?;
        state.serialize_field("serial", &LegacySerial(value))?;
        state.serialize_field("vmgenid", &PlatformDevice(value.vmgenid()))?;
        state.serialize_field("vmclock", &PlatformDevice(value.vmclock()))?;
        state.serialize_field("vmclock_abi", &LegacyVmClockAbi(value.vmclock_abi()))?;
        state.end()
    }
}

struct LegacyRootBlock<'a>(&'a SnapshotV1RootBlockState);

impl Serialize for LegacyRootBlock<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let runtime = value.runtime();
        let backing_identity = value.backing_identity();
        let mut state = serializer.serialize_struct("LegacyRootBlock", 16)?;
        state.serialize_field("drive_id", value.drive_id())?;
        state.serialize_field("path", &Redacted)?;
        state.serialize_field("partuuid", &value.partuuid())?;
        state.serialize_field("cache_type", &DriveCache(value.cache_type()))?;
        state.serialize_field("io_engine", &DriveEngine(DriveIoEngine::Sync))?;
        state.serialize_field(
            "rate_limiter_config",
            &DriveRateConfig(value.rate_limiter_config()),
        )?;
        // The guest-visible block ID may be derived from host dev/rdev/inode
        // metadata, so even an equality fingerprint would expose host
        // authority through a guessable oracle.
        state.serialize_field("device_id", &Redacted)?;
        state.serialize_field("capacity_sectors", &value.capacity_sectors())?;
        state.serialize_field("backing_bytes", &backing_identity.len())?;
        state.serialize_field("backing_identity", &Redacted)?;
        state.serialize_field("mmio", &LegacyMmio(value.mmio()))?;
        state.serialize_field("virtio", &LegacyVirtioMmio(runtime.transport()))?;
        state.serialize_field("active_queue", &BlockQueue(runtime.active_queue()))?;
        state.serialize_field("limiter", &LegacyBlockLimiter(runtime.rate_limiter()))?;
        state.serialize_field("read_only", &true)?;
        state.serialize_field("regular_file", &backing_identity.kind().is_regular_file())?;
        state.end()
    }
}

struct LegacyRetry(SnapshotV1BlockRetryState);

impl Serialize for LegacyRetry {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let (disposition, remaining_nanos) = match self.0 {
            SnapshotV1BlockRetryState::None => ("none", None),
            SnapshotV1BlockRetryState::Immediate => ("immediate", None),
            SnapshotV1BlockRetryState::After { remaining_nanos } => {
                ("after", Some(remaining_nanos))
            }
        };
        let mut state = serializer.serialize_struct("LegacyBlockRetry", 2)?;
        state.serialize_field("disposition", disposition)?;
        state.serialize_field("remaining_nanos", &remaining_nanos)?;
        state.end()
    }
}

struct LegacySerial<'a>(&'a SnapshotV1DeviceState);

impl Serialize for LegacySerial<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("LegacySerial", 2)?;
        state.serialize_field("mmio", &LegacyMmio(self.0.serial_mmio()))?;
        state.serialize_field("registers", &SerialRegisters(self.0.serial_state()))?;
        state.end()
    }
}

struct LegacyMmio(SnapshotV1MmioDeviceMetadata);

impl Serialize for LegacyMmio {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("LegacyMmio", 2)?;
        state.serialize_field("region", &MmioRegionView(self.0.region()))?;
        state.serialize_field("interrupt_line", &self.0.interrupt_line().raw_value())?;
        state.end()
    }
}

struct PlatformDevice(SnapshotV1PlatformDeviceMetadata);

impl Serialize for PlatformDevice {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let fdt = self.0.fdt_region();
        let mut state = serializer.serialize_struct("LegacyPlatformDevice", 4)?;
        state.serialize_field("range", &GuestRange(self.0.range()))?;
        state.serialize_field("fdt_base", &HexU64(fdt.base))?;
        state.serialize_field("fdt_size", &fdt.size)?;
        state.serialize_field("interrupt_line", &self.0.interrupt_line().raw_value())?;
        state.end()
    }
}

struct LegacyVmClockAbi(Option<bangbang_runtime::vmclock::VmClockAbi>);

impl Serialize for LegacyVmClockAbi {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.0 {
            Some(value) => confidential_bytes("devices.v1.vmclock-abi", &value.to_bytes())
                .serialize(serializer),
            None => serializer.serialize_none(),
        }
    }
}

pub(super) struct SingletonBlockGraph<'a>(pub(super) &'a SnapshotV2DeviceGraph);

impl Serialize for SingletonBlockGraph<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("SingletonBlockGraph", 5)?;
        state.serialize_field("compatibility", "v2.4")?;
        state.serialize_field("root_key", &DeviceKey(value.root_key()))?;
        state.serialize_field("transport_kind", &TransportKind(value.transport_kind()))?;
        state.serialize_field("record_is_root", &value.record_is_root())?;
        state.serialize_field("record", &SingletonBlockRecord(value.record()))?;
        state.end()
    }
}

struct SingletonBlockRecord<'a>(&'a SnapshotV2DeviceRecord);

impl Serialize for SingletonBlockRecord<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let config = value.config();
        let mut state = serializer.serialize_struct("SingletonBlockRecord", 5)?;
        state.serialize_field("key", &DeviceKey(value.key()))?;
        state.serialize_field("config", &SingletonBlockConfig(config))?;
        state.serialize_field("block", &Block(value.block()))?;
        state.serialize_field("virtio", &Virtio(value.virtio()))?;
        state.serialize_field("transport", &Transport(value.transport()))?;
        state.end()
    }
}

struct SingletonBlockConfig<'a>(
    &'a bangbang_runtime::snapshot_device_v2::SnapshotV2RootBlockConfig,
);

impl Serialize for SingletonBlockConfig<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("SingletonBlockConfig", 9)?;
        state.serialize_field("drive_id", value.drive_id())?;
        state.serialize_field("partuuid", &value.partuuid())?;
        state.serialize_field("root", &true)?;
        state.serialize_field("read_only", &value.is_read_only())?;
        state.serialize_field("cache_type", &DriveCache(value.cache_type()))?;
        state.serialize_field("io_engine", &DriveEngine(value.io_engine()))?;
        state.serialize_field("rate_limiter", &DriveRateConfig(value.rate_limiter()))?;
        state.serialize_field("regular_file", &true)?;
        state.serialize_field("selector", &Redacted)?;
        state.end()
    }
}

struct Block<'a>(&'a SnapshotV2BlockState);

impl Serialize for Block<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("BlockState", 5)?;
        state.serialize_field("capacity_sectors", &value.capacity_sectors())?;
        // See the native-v1 block-state note above. The same ID flows through
        // every later block profile and must remain literally redacted.
        state.serialize_field("device_id", &Redacted)?;
        state.serialize_field("active_queue", &BlockQueue(value.active_queue()))?;
        state.serialize_field("limiter", &BlockLimiter(value.limiter()))?;
        state.serialize_field("retry", &Retry(value.retry()))?;
        state.end()
    }
}

pub(super) struct MultiBlockGraph<'a>(pub(super) &'a SnapshotV2MultiBlockDeviceGraph);

impl Serialize for MultiBlockGraph<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("MultiBlockGraph", 5)?;
        state.serialize_field("compatibility", "v2.5")?;
        state.serialize_field("root_key", &OptionalDeviceKey(value.root_key()))?;
        state.serialize_field("transport_kind", &TransportKind(value.transport_kind()))?;
        state.serialize_field("record_count", &value.records().len())?;
        state.serialize_field("block_records", &MultiBlockRecords(value.records()))?;
        state.end()
    }
}

struct MultiBlockRecords<'a>(&'a [SnapshotV2MultiBlockDeviceRecord]);

impl Serialize for MultiBlockRecords<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for record in self.0 {
            sequence.serialize_element(&MultiBlockRecord(record))?;
        }
        sequence.end()
    }
}

struct MultiBlockRecord<'a>(&'a SnapshotV2MultiBlockDeviceRecord);

impl Serialize for MultiBlockRecord<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let block = value.block();
        let mut state = serializer.serialize_struct("MultiBlockRecord", 6)?;
        state.serialize_field("key", &DeviceKey(value.key()))?;
        state.serialize_field("config", &MultiBlockConfig(value.config()))?;
        state.serialize_field("backing_bytes", &block.backing_bytes())?;
        state.serialize_field("block", &Block(block.continuation()))?;
        state.serialize_field("virtio", &Virtio(value.virtio()))?;
        state.serialize_field("transport", &Transport(value.transport()))?;
        state.end()
    }
}

struct MultiBlockConfig<'a>(&'a bangbang_runtime::snapshot_device_v2_5::SnapshotV2MultiBlockConfig);

impl Serialize for MultiBlockConfig<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("MultiBlockConfig", 9)?;
        state.serialize_field("drive_id", value.drive_id())?;
        state.serialize_field("partuuid", &value.partuuid())?;
        state.serialize_field("root", &value.is_root())?;
        state.serialize_field("read_only", &value.is_read_only())?;
        state.serialize_field("cache_type", &DriveCache(value.cache_type()))?;
        state.serialize_field("io_engine", &DriveEngine(value.io_engine()))?;
        state.serialize_field("rate_limiter", &DriveRateConfig(value.rate_limiter()))?;
        state.serialize_field("regular_file", &value.is_regular_file())?;
        state.serialize_field("selector", &Redacted)?;
        state.end()
    }
}

pub(super) struct StorageGraph<'a>(pub(super) &'a SnapshotV2StorageDeviceGraph);

impl Serialize for StorageGraph<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("StorageGraph", 7)?;
        state.serialize_field("compatibility", "v2.6")?;
        state.serialize_field("root_key", &OptionalDeviceKey(value.root_key()))?;
        state.serialize_field("transport_kind", &TransportKind(value.transport_kind()))?;
        state.serialize_field("record_count", &value.record_count())?;
        state.serialize_field("block_record_count", &value.block_records().len())?;
        state.serialize_field("pmem_record_count", &value.pmem_records().len())?;
        state.serialize_field("records", &StorageRecords(value))?;
        state.end()
    }
}

struct StorageRecords<'a>(&'a SnapshotV2StorageDeviceGraph);

impl Serialize for StorageRecords<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut sequence = serializer.serialize_seq(Some(value.record_count()))?;
        for record in value.block_records() {
            sequence.serialize_element(&StorageRecord::Block(record))?;
        }
        for record in value.pmem_records() {
            sequence.serialize_element(&StorageRecord::Pmem(record))?;
        }
        sequence.end()
    }
}

enum StorageRecord<'a> {
    Block(&'a SnapshotV2MultiBlockDeviceRecord),
    Pmem(&'a SnapshotV2PmemDeviceRecord),
}

impl Serialize for StorageRecord<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Block(record) => MultiBlockRecord(record).serialize(serializer),
            Self::Pmem(record) => PmemRecord(record).serialize(serializer),
        }
    }
}

struct PmemRecord<'a>(&'a SnapshotV2PmemDeviceRecord);

impl Serialize for PmemRecord<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let pmem = value.pmem();
        let config_space = pmem.config_space();
        let mut state = serializer.serialize_struct("PmemRecord", 12)?;
        state.serialize_field("key", &DeviceKey(value.key()))?;
        state.serialize_field("config", &PmemConfig(value.config()))?;
        state.serialize_field("file_bytes", &pmem.file_bytes())?;
        state.serialize_field("mapped_bytes", &pmem.mapped_bytes())?;
        state.serialize_field("guest_range", &GuestRange(pmem.guest_range()))?;
        state.serialize_field("config_start", &HexU64(config_space.start()))?;
        state.serialize_field("config_size", &config_space.size())?;
        state.serialize_field("active_queue", &PmemQueue(pmem.active_queue()))?;
        state.serialize_field("limiter", &PmemLimiter(pmem.limiter()))?;
        state.serialize_field(
            "pending_rate_limited_queue",
            &pmem.pending_rate_limited_queue(),
        )?;
        state.serialize_field("retry", &Retry(pmem.retry()))?;
        state.serialize_field("virtio_and_transport", &PmemVirtioAndTransport(value))?;
        state.end()
    }
}

struct PmemVirtioAndTransport<'a>(&'a SnapshotV2PmemDeviceRecord);

impl Serialize for PmemVirtioAndTransport<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("PmemVirtioAndTransport", 2)?;
        state.serialize_field("virtio", &Virtio(self.0.virtio()))?;
        state.serialize_field("transport", &Transport(self.0.transport()))?;
        state.end()
    }
}

struct PmemConfig<'a>(&'a bangbang_runtime::snapshot_device_v2_6::SnapshotV2PmemConfig);

impl Serialize for PmemConfig<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("PmemConfig", 7)?;
        state.serialize_field("pmem_id", value.pmem_id())?;
        state.serialize_field("root", &value.is_root())?;
        state.serialize_field("read_only", &value.is_read_only())?;
        state.serialize_field("rate_limiter", &PmemRateConfig(value.rate_limiter()))?;
        state.serialize_field("regular_file", &value.is_regular_file())?;
        state.serialize_field("selector", &Redacted)?;
        state.serialize_field("device_kind", "pmem")?;
        state.end()
    }
}
