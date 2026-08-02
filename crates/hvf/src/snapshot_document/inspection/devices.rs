use serde::Serialize;
use serde::ser::SerializeStruct;

use super::{HvfNativeSnapshotDocument, HvfNativeSnapshotDocumentState};

mod diff;
mod product;
mod shared;
mod storage;

pub(super) struct Devices<'a>(pub(super) &'a HvfNativeSnapshotDocument);

#[cfg(test)]
pub(super) struct StorageGraphForTest<'a>(
    pub(super) &'a bangbang_runtime::snapshot_device_v2_6::SnapshotV2StorageDeviceGraph,
);

#[cfg(test)]
impl Serialize for StorageGraphForTest<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        storage::StorageGraph(self.0).serialize(serializer)
    }
}

#[cfg(test)]
pub(super) struct BalloonForTest<'a>(
    pub(super) &'a bangbang_runtime::snapshot_balloon_v2_9::SnapshotV2BalloonState,
);

#[cfg(test)]
impl Serialize for BalloonForTest<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        product::Balloon(self.0).serialize(serializer)
    }
}

#[cfg(test)]
pub(super) struct MemoryHotplugForTest<'a>(
    pub(super) &'a bangbang_runtime::snapshot_memory_hotplug_v2_10::SnapshotV2MemoryHotplugState,
);

#[cfg(test)]
impl Serialize for MemoryHotplugForTest<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        product::MemoryHotplug(self.0).serialize(serializer)
    }
}

#[cfg(test)]
pub(super) struct NetworkForTest<'a>(
    pub(super) &'a bangbang_runtime::snapshot_network_v2_11::SnapshotV2NetworkState,
);

#[cfg(test)]
impl Serialize for NetworkForTest<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        product::Network(self.0).serialize(serializer)
    }
}

#[cfg(test)]
pub(super) struct DiffLayerForTest<'a>(
    pub(super) &'a bangbang_runtime::snapshot_diff_v2_13::SnapshotV2DiffLayerBinding,
);

#[cfg(test)]
impl Serialize for DiffLayerForTest<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        diff::DiffLayer(self.0).serialize(serializer)
    }
}

impl Serialize for Devices<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let parts = DeviceParts::from_document(self.0);
        let mut state = serializer.serialize_struct("Devices", 10)?;
        state.serialize_field("profile", &super::common::Profile(self.0.profile()))?;
        state.serialize_field("legacy", &parts.legacy)?;
        state.serialize_field("root_block", &parts.root_block)?;
        state.serialize_field("storage", &parts.storage)?;
        state.serialize_field("serial", &parts.serial)?;
        state.serialize_field("entropy", &parts.entropy)?;
        state.serialize_field("balloon", &parts.balloon)?;
        state.serialize_field("memory_hotplug", &parts.memory_hotplug)?;
        state.serialize_field("network", &parts.network)?;
        state.serialize_field("vsock", &parts.vsock)?;
        state.end()
    }
}

struct DeviceParts<'a> {
    legacy: Option<storage::LegacyDevices<'a>>,
    root_block: Option<storage::SingletonBlockGraph<'a>>,
    storage: Option<Storage<'a>>,
    serial: Option<product::Serial<'a>>,
    entropy: Option<product::Entropy<'a>>,
    balloon: Option<product::Balloon<'a>>,
    memory_hotplug: Option<product::MemoryHotplug<'a>>,
    network: Option<product::Network<'a>>,
    vsock: Option<product::Vsock<'a>>,
}

impl DeviceParts<'_> {
    const fn empty() -> Self {
        Self {
            legacy: None,
            root_block: None,
            storage: None,
            serial: None,
            entropy: None,
            balloon: None,
            memory_hotplug: None,
            network: None,
            vsock: None,
        }
    }
}

impl<'a> DeviceParts<'a> {
    fn from_document(document: &'a HvfNativeSnapshotDocument) -> Self {
        let mut parts = Self::empty();
        match &document.state {
            HvfNativeSnapshotDocumentState::V1(bundle) => {
                parts.legacy = Some(storage::LegacyDevices(bundle.state().device()));
            }
            HvfNativeSnapshotDocumentState::V2LegacyPlatform(_) => {}
            HvfNativeSnapshotDocumentState::V2DeviceGraph(state) => {
                parts.root_block = Some(storage::SingletonBlockGraph(state.device_graph()));
            }
            HvfNativeSnapshotDocumentState::V2MultiBlock(state) => {
                parts.storage = Some(Storage::MultiBlock(state.device_graph()));
            }
            HvfNativeSnapshotDocumentState::V2Storage(state) => {
                parts.storage = Some(Storage::Storage(state.device_graph()));
            }
            HvfNativeSnapshotDocumentState::V2Serial(state) => {
                parts.storage = state.device_graph().map(Storage::Storage);
                parts.serial = Some(product::Serial(state.serial()));
            }
            HvfNativeSnapshotDocumentState::V2Entropy(state) => {
                parts.storage = state.device_graph().map(Storage::Storage);
                parts.serial = Some(product::Serial(state.serial()));
                parts.entropy = state.entropy().map(product::Entropy);
            }
            HvfNativeSnapshotDocumentState::V2Balloon(state) => {
                parts.storage = state.device_graph().map(Storage::Storage);
                parts.serial = Some(product::Serial(state.serial()));
                parts.entropy = state.entropy().map(product::Entropy);
                parts.balloon = state.balloon().map(product::Balloon);
            }
            HvfNativeSnapshotDocumentState::V2MemoryHotplug(state) => {
                parts.storage = state.device_graph().map(Storage::Storage);
                parts.serial = Some(product::Serial(state.serial()));
                parts.entropy = state.entropy().map(product::Entropy);
                parts.balloon = state.balloon().map(product::Balloon);
                parts.memory_hotplug = state.memory_hotplug().map(product::MemoryHotplug);
            }
            HvfNativeSnapshotDocumentState::V2Network(state) => {
                parts.storage = state.device_graph().map(Storage::Storage);
                parts.serial = Some(product::Serial(state.serial()));
                parts.entropy = state.entropy().map(product::Entropy);
                parts.balloon = state.balloon().map(product::Balloon);
                parts.memory_hotplug = state.memory_hotplug().map(product::MemoryHotplug);
                parts.network = state.network().map(product::Network);
            }
            HvfNativeSnapshotDocumentState::V2Vsock(state) => {
                parts.storage = state.device_graph().map(Storage::Storage);
                parts.serial = Some(product::Serial(state.serial()));
                parts.entropy = state.entropy().map(product::Entropy);
                parts.balloon = state.balloon().map(product::Balloon);
                parts.memory_hotplug = state.memory_hotplug().map(product::MemoryHotplug);
                parts.network = state.network().map(product::Network);
                parts.vsock = state.vsock().map(product::Vsock);
            }
            HvfNativeSnapshotDocumentState::V2Diff(state) => {
                parts.storage = state.device_graph().map(Storage::Storage);
                parts.serial = Some(product::Serial(state.serial()));
                parts.entropy = state.entropy().map(product::Entropy);
                parts.balloon = state.balloon().map(product::Balloon);
                parts.memory_hotplug = state.memory_hotplug().map(product::MemoryHotplug);
                parts.network = state.network().map(product::Network);
                parts.vsock = state.vsock().map(product::Vsock);
            }
        }
        parts
    }
}

enum Storage<'a> {
    MultiBlock(&'a bangbang_runtime::snapshot_device_v2_5::SnapshotV2MultiBlockDeviceGraph),
    Storage(&'a bangbang_runtime::snapshot_device_v2_6::SnapshotV2StorageDeviceGraph),
}

impl Serialize for Storage<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::MultiBlock(graph) => storage::MultiBlockGraph(graph).serialize(serializer),
            Self::Storage(graph) => storage::StorageGraph(graph).serialize(serializer),
        }
    }
}

pub(super) struct Diff<'a>(pub(super) &'a HvfNativeSnapshotDocument);

impl Serialize for Diff<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match &self.0.state {
            HvfNativeSnapshotDocumentState::V2Diff(state) => {
                diff::DiffLayer(state.layer()).serialize(serializer)
            }
            HvfNativeSnapshotDocumentState::V1(_)
            | HvfNativeSnapshotDocumentState::V2LegacyPlatform(_)
            | HvfNativeSnapshotDocumentState::V2DeviceGraph(_)
            | HvfNativeSnapshotDocumentState::V2MultiBlock(_)
            | HvfNativeSnapshotDocumentState::V2Storage(_)
            | HvfNativeSnapshotDocumentState::V2Serial(_)
            | HvfNativeSnapshotDocumentState::V2Entropy(_)
            | HvfNativeSnapshotDocumentState::V2Balloon(_)
            | HvfNativeSnapshotDocumentState::V2MemoryHotplug(_)
            | HvfNativeSnapshotDocumentState::V2Network(_)
            | HvfNativeSnapshotDocumentState::V2Vsock(_) => serializer.serialize_none(),
        }
    }
}
