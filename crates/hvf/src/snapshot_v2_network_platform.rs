//! Host-free exact native-v2 2.11 network platform-product planning.

use std::fmt;

use bangbang_runtime::balloon::BalloonMmioLayout;
use bangbang_runtime::entropy::{EntropyMmioLayout, VIRTIO_RNG_QUEUE_SIZES};
use bangbang_runtime::fdt::{Arm64FdtPciHost, Arm64FdtRegion, Arm64FdtVirtioMmioDevice};
use bangbang_runtime::interrupt::GuestInterruptLine;
use bangbang_runtime::memory::{GuestAddress, GuestMemory, GuestMemoryRange};
use bangbang_runtime::memory_hotplug::{VIRTIO_MEM_QUEUE_SIZES, VirtioMemMmioLayout};
use bangbang_runtime::mmio::{MmioRegion, MmioRegionId};
use bangbang_runtime::network::{
    NetworkMmioLayout, NetworkRateLimiterConfig, NetworkTokenBucketConfig, VIRTIO_NET_QUEUE_COUNT,
    VIRTIO_NET_QUEUE_SIZES,
};
use bangbang_runtime::pci::{Arm64PciAddressPlan, PciSbdf};
use bangbang_runtime::pvtime::ARM64_PVTIME_STRUCTURE_SIZE;
use bangbang_runtime::rtc::{RTC_MMIO_DEVICE_WINDOW_SIZE, RtcMmioLayout};
use bangbang_runtime::serial::SERIAL_MMIO_DEVICE_WINDOW_SIZE;
use bangbang_runtime::snapshot_balloon_v2_9::{
    PreparedSnapshotV2BalloonTransport, SnapshotV2BalloonRestorePlan,
};
use bangbang_runtime::snapshot_device_v2::{
    SnapshotV2DeviceTransport, SnapshotV2DeviceTransportKind, SnapshotV2VirtioQueueState,
};
use bangbang_runtime::snapshot_device_v2_6::PreparedSnapshotV2StorageBundle;
use bangbang_runtime::snapshot_entropy_v2_8::{
    PreparedSnapshotV2EntropyTransport, SnapshotV2EntropyRestorePlan,
};
use bangbang_runtime::snapshot_memory_hotplug_v2_10::PreparedSnapshotV2MemoryHotplugTopology;
use bangbang_runtime::snapshot_memory_v2::SnapshotV2MemoryBinding;
use bangbang_runtime::snapshot_network_restore_v2_11::{
    PreparedSnapshotV2NetworkRestoreInterface, PreparedSnapshotV2NetworkRestoreTopology,
};
use bangbang_runtime::snapshot_network_v2_11::{
    NATIVE_V2_NETWORK_MAX_INTERFACES, SnapshotV2MmdsInterfaceState, SnapshotV2NetworkLimiterState,
    SnapshotV2NetworkTokenBucketState,
};
use bangbang_runtime::snapshot_restore::{
    SnapshotRestoreResourceClass, SnapshotRestoreResourceKey,
};
use bangbang_runtime::storage_capture::StorageDeviceOrigin;
use bangbang_runtime::virtio_mmio::VIRTIO_MMIO_DEVICE_WINDOW_SIZE;
use bangbang_runtime::virtio_pci::{VirtioPciEndpointPhase, VirtioPciMsixState};

use crate::gic::{HvfGicInterruptLineAllocator, HvfGicMsiMetadata};
use crate::memory::{
    HvfSnapshotV2MemoryHotplugMappingPlan, PrepareHvfSnapshotV2MemoryHotplugMappingPlanError,
    prepare_hvf_snapshot_v2_memory_hotplug_mapping_plan,
};
use crate::snapshot_v2::HvfSnapshotV2PlatformState;
use crate::snapshot_v2_balloon_platform::{
    HvfSnapshotV2BalloonMmioEndpointPlan, HvfSnapshotV2BalloonPciEndpointPlan,
    PrepareHvfSnapshotV2BalloonPlatformPlanError,
    prepare_hvf_snapshot_v2_balloon_mmio_endpoint_plan,
    prepare_hvf_snapshot_v2_balloon_pci_endpoint_plan,
};
use crate::snapshot_v2_entropy_platform::register_active_retained_pci_routes;
use crate::snapshot_v2_memory_hotplug_platform::register_active_snapshot_v2_pci_routes;
use crate::snapshot_v2_multi_block_platform::{
    snapshot_v2_pci_endpoint_placement, snapshot_v2_pci_endpoint_route_count,
    valid_snapshot_v2_pci_record,
};
use crate::snapshot_v2_platform::{
    PROCESS_RTC_MMIO_BASE, PROCESS_RTC_MMIO_REGION_ID, PROCESS_SERIAL_MMIO_BASE,
};
use crate::snapshot_v2_storage_platform::{
    HvfSnapshotV2StorageMmioInsertedEndpoint, HvfSnapshotV2StorageMmioPlatformPlan,
    HvfSnapshotV2StorageMmioPlatformPrefix, HvfSnapshotV2StorageMmioProcessConfig,
    HvfSnapshotV2StoragePciPlatformPlan, HvfSnapshotV2StoragePciPlatformPrefix,
    PrepareHvfSnapshotV2StorageMmioPlatformPlanError,
    PrepareHvfSnapshotV2StoragePciPlatformPlanError, mmio_region_conflicts_with_platform,
    prepare_hvf_snapshot_v2_storage_mmio_platform_plan_with_prefix_and_insertion,
    prepare_hvf_snapshot_v2_storage_pci_platform_plan_with_prefix,
    queue_ranges_conflict_with_pci_platform, queue_ranges_conflict_with_platform,
    register_active_pci_routes,
};
use crate::startup::{
    PCI_ENDPOINT_SLOT_COUNT, pci_balloon_restore_gic_msi_configuration,
    pci_entropy_restore_gic_msi_configuration, pci_memory_hotplug_restore_gic_msi_configuration,
    pci_root_restore_gic_msi_configuration, pci_vsock_restore_gic_msi_configuration,
};

const REDACTED: &str = "<redacted>";
const NETWORK_DEVICE_KIND: u32 = 4;
const MIB: u64 = 1024 * 1024;

/// One admitted exact-2.11 network-bearing product shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HvfSnapshotV2NetworkProductKind {
    SerialNetwork,
    SerialStorageNetwork,
    SerialEntropyNetwork,
    SerialStorageEntropyNetwork,
    SerialBalloonNetwork,
    SerialBalloonStorageNetwork,
    SerialBalloonEntropyNetwork,
    SerialBalloonStorageEntropyNetwork,
    SerialNetworkMemoryHotplug,
    SerialStorageNetworkMemoryHotplug,
    SerialEntropyNetworkMemoryHotplug,
    SerialStorageEntropyNetworkMemoryHotplug,
    SerialBalloonNetworkMemoryHotplug,
    SerialBalloonStorageNetworkMemoryHotplug,
    SerialBalloonEntropyNetworkMemoryHotplug,
    SerialBalloonStorageEntropyNetworkMemoryHotplug,
}

struct StaticNetworkProduct {
    binding: SnapshotV2MemoryBinding,
    network: PreparedSnapshotV2NetworkRestoreTopology,
}

struct MemoryHotplugNetworkProduct {
    topology: PreparedSnapshotV2MemoryHotplugTopology,
    memory: GuestMemory,
    network: PreparedSnapshotV2NetworkRestoreTopology,
}

enum HvfSnapshotV2NetworkPreparedProductParts {
    Network(StaticNetworkProduct),
    StorageNetwork {
        base: StaticNetworkProduct,
        storage: PreparedSnapshotV2StorageBundle,
    },
    EntropyNetwork {
        base: StaticNetworkProduct,
        entropy: SnapshotV2EntropyRestorePlan,
    },
    StorageEntropyNetwork {
        base: StaticNetworkProduct,
        storage: PreparedSnapshotV2StorageBundle,
        entropy: SnapshotV2EntropyRestorePlan,
    },
    BalloonNetwork {
        base: StaticNetworkProduct,
        balloon: SnapshotV2BalloonRestorePlan,
    },
    BalloonStorageNetwork {
        base: StaticNetworkProduct,
        balloon: SnapshotV2BalloonRestorePlan,
        storage: PreparedSnapshotV2StorageBundle,
    },
    BalloonEntropyNetwork {
        base: StaticNetworkProduct,
        balloon: SnapshotV2BalloonRestorePlan,
        entropy: SnapshotV2EntropyRestorePlan,
    },
    BalloonStorageEntropyNetwork {
        base: StaticNetworkProduct,
        balloon: SnapshotV2BalloonRestorePlan,
        storage: PreparedSnapshotV2StorageBundle,
        entropy: SnapshotV2EntropyRestorePlan,
    },
    NetworkMemoryHotplug(MemoryHotplugNetworkProduct),
    StorageNetworkMemoryHotplug {
        base: MemoryHotplugNetworkProduct,
        storage: PreparedSnapshotV2StorageBundle,
    },
    EntropyNetworkMemoryHotplug {
        base: MemoryHotplugNetworkProduct,
        entropy: SnapshotV2EntropyRestorePlan,
    },
    StorageEntropyNetworkMemoryHotplug {
        base: MemoryHotplugNetworkProduct,
        storage: PreparedSnapshotV2StorageBundle,
        entropy: SnapshotV2EntropyRestorePlan,
    },
    BalloonNetworkMemoryHotplug {
        base: MemoryHotplugNetworkProduct,
        balloon: SnapshotV2BalloonRestorePlan,
    },
    BalloonStorageNetworkMemoryHotplug {
        base: MemoryHotplugNetworkProduct,
        balloon: SnapshotV2BalloonRestorePlan,
        storage: PreparedSnapshotV2StorageBundle,
    },
    BalloonEntropyNetworkMemoryHotplug {
        base: MemoryHotplugNetworkProduct,
        balloon: SnapshotV2BalloonRestorePlan,
        entropy: SnapshotV2EntropyRestorePlan,
    },
    BalloonStorageEntropyNetworkMemoryHotplug {
        base: MemoryHotplugNetworkProduct,
        balloon: SnapshotV2BalloonRestorePlan,
        storage: PreparedSnapshotV2StorageBundle,
        entropy: SnapshotV2EntropyRestorePlan,
    },
}

/// One closed set of prepared exact-2.11 component continuations.
///
/// Every constructor requires network state. Optional component presence is
/// encoded by the selected tag instead of independently mutable `Option`s.
pub struct HvfSnapshotV2NetworkPreparedProduct {
    parts: HvfSnapshotV2NetworkPreparedProductParts,
}

impl HvfSnapshotV2NetworkPreparedProduct {
    fn static_base(
        binding: SnapshotV2MemoryBinding,
        network: PreparedSnapshotV2NetworkRestoreTopology,
    ) -> StaticNetworkProduct {
        StaticNetworkProduct { binding, network }
    }

    fn hotplug_base(
        topology: PreparedSnapshotV2MemoryHotplugTopology,
        memory: GuestMemory,
        network: PreparedSnapshotV2NetworkRestoreTopology,
    ) -> MemoryHotplugNetworkProduct {
        MemoryHotplugNetworkProduct {
            topology,
            memory,
            network,
        }
    }

    pub fn serial_network(
        binding: SnapshotV2MemoryBinding,
        network: PreparedSnapshotV2NetworkRestoreTopology,
    ) -> Self {
        Self {
            parts: HvfSnapshotV2NetworkPreparedProductParts::Network(Self::static_base(
                binding, network,
            )),
        }
    }

    pub fn serial_storage_network(
        binding: SnapshotV2MemoryBinding,
        network: PreparedSnapshotV2NetworkRestoreTopology,
        storage: PreparedSnapshotV2StorageBundle,
    ) -> Self {
        Self {
            parts: HvfSnapshotV2NetworkPreparedProductParts::StorageNetwork {
                base: Self::static_base(binding, network),
                storage,
            },
        }
    }

    pub fn serial_entropy_network(
        binding: SnapshotV2MemoryBinding,
        network: PreparedSnapshotV2NetworkRestoreTopology,
        entropy: SnapshotV2EntropyRestorePlan,
    ) -> Self {
        Self {
            parts: HvfSnapshotV2NetworkPreparedProductParts::EntropyNetwork {
                base: Self::static_base(binding, network),
                entropy,
            },
        }
    }

    pub fn serial_storage_entropy_network(
        binding: SnapshotV2MemoryBinding,
        network: PreparedSnapshotV2NetworkRestoreTopology,
        storage: PreparedSnapshotV2StorageBundle,
        entropy: SnapshotV2EntropyRestorePlan,
    ) -> Self {
        Self {
            parts: HvfSnapshotV2NetworkPreparedProductParts::StorageEntropyNetwork {
                base: Self::static_base(binding, network),
                storage,
                entropy,
            },
        }
    }

    pub fn serial_balloon_network(
        binding: SnapshotV2MemoryBinding,
        network: PreparedSnapshotV2NetworkRestoreTopology,
        balloon: SnapshotV2BalloonRestorePlan,
    ) -> Self {
        Self {
            parts: HvfSnapshotV2NetworkPreparedProductParts::BalloonNetwork {
                base: Self::static_base(binding, network),
                balloon,
            },
        }
    }

    pub fn serial_balloon_storage_network(
        binding: SnapshotV2MemoryBinding,
        network: PreparedSnapshotV2NetworkRestoreTopology,
        balloon: SnapshotV2BalloonRestorePlan,
        storage: PreparedSnapshotV2StorageBundle,
    ) -> Self {
        Self {
            parts: HvfSnapshotV2NetworkPreparedProductParts::BalloonStorageNetwork {
                base: Self::static_base(binding, network),
                balloon,
                storage,
            },
        }
    }

    pub fn serial_balloon_entropy_network(
        binding: SnapshotV2MemoryBinding,
        network: PreparedSnapshotV2NetworkRestoreTopology,
        balloon: SnapshotV2BalloonRestorePlan,
        entropy: SnapshotV2EntropyRestorePlan,
    ) -> Self {
        Self {
            parts: HvfSnapshotV2NetworkPreparedProductParts::BalloonEntropyNetwork {
                base: Self::static_base(binding, network),
                balloon,
                entropy,
            },
        }
    }

    pub fn serial_balloon_storage_entropy_network(
        binding: SnapshotV2MemoryBinding,
        network: PreparedSnapshotV2NetworkRestoreTopology,
        balloon: SnapshotV2BalloonRestorePlan,
        storage: PreparedSnapshotV2StorageBundle,
        entropy: SnapshotV2EntropyRestorePlan,
    ) -> Self {
        Self {
            parts: HvfSnapshotV2NetworkPreparedProductParts::BalloonStorageEntropyNetwork {
                base: Self::static_base(binding, network),
                balloon,
                storage,
                entropy,
            },
        }
    }

    pub fn serial_network_memory_hotplug(
        topology: PreparedSnapshotV2MemoryHotplugTopology,
        memory: GuestMemory,
        network: PreparedSnapshotV2NetworkRestoreTopology,
    ) -> Self {
        Self {
            parts: HvfSnapshotV2NetworkPreparedProductParts::NetworkMemoryHotplug(
                Self::hotplug_base(topology, memory, network),
            ),
        }
    }

    pub fn serial_storage_network_memory_hotplug(
        topology: PreparedSnapshotV2MemoryHotplugTopology,
        memory: GuestMemory,
        network: PreparedSnapshotV2NetworkRestoreTopology,
        storage: PreparedSnapshotV2StorageBundle,
    ) -> Self {
        Self {
            parts: HvfSnapshotV2NetworkPreparedProductParts::StorageNetworkMemoryHotplug {
                base: Self::hotplug_base(topology, memory, network),
                storage,
            },
        }
    }

    pub fn serial_entropy_network_memory_hotplug(
        topology: PreparedSnapshotV2MemoryHotplugTopology,
        memory: GuestMemory,
        network: PreparedSnapshotV2NetworkRestoreTopology,
        entropy: SnapshotV2EntropyRestorePlan,
    ) -> Self {
        Self {
            parts: HvfSnapshotV2NetworkPreparedProductParts::EntropyNetworkMemoryHotplug {
                base: Self::hotplug_base(topology, memory, network),
                entropy,
            },
        }
    }

    pub fn serial_storage_entropy_network_memory_hotplug(
        topology: PreparedSnapshotV2MemoryHotplugTopology,
        memory: GuestMemory,
        network: PreparedSnapshotV2NetworkRestoreTopology,
        storage: PreparedSnapshotV2StorageBundle,
        entropy: SnapshotV2EntropyRestorePlan,
    ) -> Self {
        Self {
            parts: HvfSnapshotV2NetworkPreparedProductParts::StorageEntropyNetworkMemoryHotplug {
                base: Self::hotplug_base(topology, memory, network),
                storage,
                entropy,
            },
        }
    }

    pub fn serial_balloon_network_memory_hotplug(
        topology: PreparedSnapshotV2MemoryHotplugTopology,
        memory: GuestMemory,
        network: PreparedSnapshotV2NetworkRestoreTopology,
        balloon: SnapshotV2BalloonRestorePlan,
    ) -> Self {
        Self {
            parts: HvfSnapshotV2NetworkPreparedProductParts::BalloonNetworkMemoryHotplug {
                base: Self::hotplug_base(topology, memory, network),
                balloon,
            },
        }
    }

    pub fn serial_balloon_storage_network_memory_hotplug(
        topology: PreparedSnapshotV2MemoryHotplugTopology,
        memory: GuestMemory,
        network: PreparedSnapshotV2NetworkRestoreTopology,
        balloon: SnapshotV2BalloonRestorePlan,
        storage: PreparedSnapshotV2StorageBundle,
    ) -> Self {
        Self {
            parts: HvfSnapshotV2NetworkPreparedProductParts::BalloonStorageNetworkMemoryHotplug {
                base: Self::hotplug_base(topology, memory, network),
                balloon,
                storage,
            },
        }
    }

    pub fn serial_balloon_entropy_network_memory_hotplug(
        topology: PreparedSnapshotV2MemoryHotplugTopology,
        memory: GuestMemory,
        network: PreparedSnapshotV2NetworkRestoreTopology,
        balloon: SnapshotV2BalloonRestorePlan,
        entropy: SnapshotV2EntropyRestorePlan,
    ) -> Self {
        Self {
            parts: HvfSnapshotV2NetworkPreparedProductParts::BalloonEntropyNetworkMemoryHotplug {
                base: Self::hotplug_base(topology, memory, network),
                balloon,
                entropy,
            },
        }
    }

    pub fn serial_balloon_storage_entropy_network_memory_hotplug(
        topology: PreparedSnapshotV2MemoryHotplugTopology,
        memory: GuestMemory,
        network: PreparedSnapshotV2NetworkRestoreTopology,
        balloon: SnapshotV2BalloonRestorePlan,
        storage: PreparedSnapshotV2StorageBundle,
        entropy: SnapshotV2EntropyRestorePlan,
    ) -> Self {
        Self {
            parts: HvfSnapshotV2NetworkPreparedProductParts::BalloonStorageEntropyNetworkMemoryHotplug {
                base: Self::hotplug_base(topology, memory, network),
                balloon,
                storage,
                entropy,
            },
        }
    }

    pub const fn kind(&self) -> HvfSnapshotV2NetworkProductKind {
        match self.parts {
            HvfSnapshotV2NetworkPreparedProductParts::Network(_) => {
                HvfSnapshotV2NetworkProductKind::SerialNetwork
            }
            HvfSnapshotV2NetworkPreparedProductParts::StorageNetwork { .. } => {
                HvfSnapshotV2NetworkProductKind::SerialStorageNetwork
            }
            HvfSnapshotV2NetworkPreparedProductParts::EntropyNetwork { .. } => {
                HvfSnapshotV2NetworkProductKind::SerialEntropyNetwork
            }
            HvfSnapshotV2NetworkPreparedProductParts::StorageEntropyNetwork { .. } => {
                HvfSnapshotV2NetworkProductKind::SerialStorageEntropyNetwork
            }
            HvfSnapshotV2NetworkPreparedProductParts::BalloonNetwork { .. } => {
                HvfSnapshotV2NetworkProductKind::SerialBalloonNetwork
            }
            HvfSnapshotV2NetworkPreparedProductParts::BalloonStorageNetwork { .. } => {
                HvfSnapshotV2NetworkProductKind::SerialBalloonStorageNetwork
            }
            HvfSnapshotV2NetworkPreparedProductParts::BalloonEntropyNetwork { .. } => {
                HvfSnapshotV2NetworkProductKind::SerialBalloonEntropyNetwork
            }
            HvfSnapshotV2NetworkPreparedProductParts::BalloonStorageEntropyNetwork {
                ..
            } => HvfSnapshotV2NetworkProductKind::SerialBalloonStorageEntropyNetwork,
            HvfSnapshotV2NetworkPreparedProductParts::NetworkMemoryHotplug(_) => {
                HvfSnapshotV2NetworkProductKind::SerialNetworkMemoryHotplug
            }
            HvfSnapshotV2NetworkPreparedProductParts::StorageNetworkMemoryHotplug {
                ..
            } => HvfSnapshotV2NetworkProductKind::SerialStorageNetworkMemoryHotplug,
            HvfSnapshotV2NetworkPreparedProductParts::EntropyNetworkMemoryHotplug {
                ..
            } => HvfSnapshotV2NetworkProductKind::SerialEntropyNetworkMemoryHotplug,
            HvfSnapshotV2NetworkPreparedProductParts::StorageEntropyNetworkMemoryHotplug {
                ..
            } => HvfSnapshotV2NetworkProductKind::SerialStorageEntropyNetworkMemoryHotplug,
            HvfSnapshotV2NetworkPreparedProductParts::BalloonNetworkMemoryHotplug {
                ..
            } => HvfSnapshotV2NetworkProductKind::SerialBalloonNetworkMemoryHotplug,
            HvfSnapshotV2NetworkPreparedProductParts::BalloonStorageNetworkMemoryHotplug {
                ..
            } => HvfSnapshotV2NetworkProductKind::SerialBalloonStorageNetworkMemoryHotplug,
            HvfSnapshotV2NetworkPreparedProductParts::BalloonEntropyNetworkMemoryHotplug {
                ..
            } => HvfSnapshotV2NetworkProductKind::SerialBalloonEntropyNetworkMemoryHotplug,
            HvfSnapshotV2NetworkPreparedProductParts::BalloonStorageEntropyNetworkMemoryHotplug {
                ..
            } => HvfSnapshotV2NetworkProductKind::SerialBalloonStorageEntropyNetworkMemoryHotplug,
        }
    }

    /// Returns the number of saved-order network interfaces.
    pub fn interface_count(&self) -> usize {
        self.network().interfaces().len()
    }

    /// Reports whether the product retains an MMDS continuation.
    pub fn has_mmds(&self) -> bool {
        self.network().mmds_state().is_some()
    }

    /// Reports whether the product includes storage.
    pub fn has_storage(&self) -> bool {
        self.storage().is_some()
    }

    /// Reports whether the product includes entropy.
    pub fn has_entropy(&self) -> bool {
        self.entropy().is_some()
    }

    /// Reports whether the product includes balloon.
    pub fn has_balloon(&self) -> bool {
        self.balloon().is_some()
    }

    /// Reports whether the product includes virtio-mem.
    pub fn has_memory_hotplug(&self) -> bool {
        self.memory_hotplug_topology().is_some()
    }

    pub(crate) fn memory_binding(&self) -> &SnapshotV2MemoryBinding {
        match self.base() {
            NetworkProductBase::Static(base) => &base.binding,
            NetworkProductBase::MemoryHotplug(base) => base.topology.memory().binding(),
        }
    }

    pub(crate) fn network(&self) -> &PreparedSnapshotV2NetworkRestoreTopology {
        match self.base() {
            NetworkProductBase::Static(base) => &base.network,
            NetworkProductBase::MemoryHotplug(base) => &base.network,
        }
    }

    pub(crate) fn storage(&self) -> Option<&PreparedSnapshotV2StorageBundle> {
        match &self.parts {
            HvfSnapshotV2NetworkPreparedProductParts::StorageNetwork { storage, .. }
            | HvfSnapshotV2NetworkPreparedProductParts::StorageEntropyNetwork {
                storage,
                ..
            }
            | HvfSnapshotV2NetworkPreparedProductParts::BalloonStorageNetwork {
                storage,
                ..
            }
            | HvfSnapshotV2NetworkPreparedProductParts::BalloonStorageEntropyNetwork {
                storage,
                ..
            }
            | HvfSnapshotV2NetworkPreparedProductParts::StorageNetworkMemoryHotplug {
                storage,
                ..
            }
            | HvfSnapshotV2NetworkPreparedProductParts::StorageEntropyNetworkMemoryHotplug {
                storage,
                ..
            }
            | HvfSnapshotV2NetworkPreparedProductParts::BalloonStorageNetworkMemoryHotplug {
                storage,
                ..
            }
            | HvfSnapshotV2NetworkPreparedProductParts::BalloonStorageEntropyNetworkMemoryHotplug {
                storage,
                ..
            } => Some(storage),
            _ => None,
        }
    }

    pub(crate) fn entropy(&self) -> Option<&SnapshotV2EntropyRestorePlan> {
        match &self.parts {
            HvfSnapshotV2NetworkPreparedProductParts::EntropyNetwork { entropy, .. }
            | HvfSnapshotV2NetworkPreparedProductParts::StorageEntropyNetwork {
                entropy,
                ..
            }
            | HvfSnapshotV2NetworkPreparedProductParts::BalloonEntropyNetwork {
                entropy,
                ..
            }
            | HvfSnapshotV2NetworkPreparedProductParts::BalloonStorageEntropyNetwork {
                entropy,
                ..
            }
            | HvfSnapshotV2NetworkPreparedProductParts::EntropyNetworkMemoryHotplug {
                entropy,
                ..
            }
            | HvfSnapshotV2NetworkPreparedProductParts::StorageEntropyNetworkMemoryHotplug {
                entropy,
                ..
            }
            | HvfSnapshotV2NetworkPreparedProductParts::BalloonEntropyNetworkMemoryHotplug {
                entropy,
                ..
            }
            | HvfSnapshotV2NetworkPreparedProductParts::BalloonStorageEntropyNetworkMemoryHotplug {
                entropy,
                ..
            } => Some(entropy),
            _ => None,
        }
    }

    pub(crate) fn balloon(&self) -> Option<&SnapshotV2BalloonRestorePlan> {
        match &self.parts {
            HvfSnapshotV2NetworkPreparedProductParts::BalloonNetwork { balloon, .. }
            | HvfSnapshotV2NetworkPreparedProductParts::BalloonStorageNetwork {
                balloon,
                ..
            }
            | HvfSnapshotV2NetworkPreparedProductParts::BalloonEntropyNetwork {
                balloon,
                ..
            }
            | HvfSnapshotV2NetworkPreparedProductParts::BalloonStorageEntropyNetwork {
                balloon,
                ..
            }
            | HvfSnapshotV2NetworkPreparedProductParts::BalloonNetworkMemoryHotplug {
                balloon,
                ..
            }
            | HvfSnapshotV2NetworkPreparedProductParts::BalloonStorageNetworkMemoryHotplug {
                balloon,
                ..
            }
            | HvfSnapshotV2NetworkPreparedProductParts::BalloonEntropyNetworkMemoryHotplug {
                balloon,
                ..
            }
            | HvfSnapshotV2NetworkPreparedProductParts::BalloonStorageEntropyNetworkMemoryHotplug {
                balloon,
                ..
            } => Some(balloon),
            _ => None,
        }
    }

    pub(crate) fn memory_hotplug_topology(
        &self,
    ) -> Option<&PreparedSnapshotV2MemoryHotplugTopology> {
        match self.base() {
            NetworkProductBase::Static(_) => None,
            NetworkProductBase::MemoryHotplug(base) => Some(&base.topology),
        }
    }

    pub(crate) fn memory_hotplug_memory(&self) -> Option<&GuestMemory> {
        match self.base() {
            NetworkProductBase::Static(_) => None,
            NetworkProductBase::MemoryHotplug(base) => Some(&base.memory),
        }
    }

    fn base(&self) -> NetworkProductBase<'_> {
        match &self.parts {
            HvfSnapshotV2NetworkPreparedProductParts::Network(base)
            | HvfSnapshotV2NetworkPreparedProductParts::StorageNetwork { base, .. }
            | HvfSnapshotV2NetworkPreparedProductParts::EntropyNetwork { base, .. }
            | HvfSnapshotV2NetworkPreparedProductParts::StorageEntropyNetwork {
                base,
                ..
            }
            | HvfSnapshotV2NetworkPreparedProductParts::BalloonNetwork { base, .. }
            | HvfSnapshotV2NetworkPreparedProductParts::BalloonStorageNetwork {
                base,
                ..
            }
            | HvfSnapshotV2NetworkPreparedProductParts::BalloonEntropyNetwork {
                base,
                ..
            }
            | HvfSnapshotV2NetworkPreparedProductParts::BalloonStorageEntropyNetwork {
                base,
                ..
            } => NetworkProductBase::Static(base),
            HvfSnapshotV2NetworkPreparedProductParts::NetworkMemoryHotplug(base)
            | HvfSnapshotV2NetworkPreparedProductParts::StorageNetworkMemoryHotplug {
                base,
                ..
            }
            | HvfSnapshotV2NetworkPreparedProductParts::EntropyNetworkMemoryHotplug {
                base,
                ..
            }
            | HvfSnapshotV2NetworkPreparedProductParts::StorageEntropyNetworkMemoryHotplug {
                base,
                ..
            }
            | HvfSnapshotV2NetworkPreparedProductParts::BalloonNetworkMemoryHotplug {
                base,
                ..
            }
            | HvfSnapshotV2NetworkPreparedProductParts::BalloonStorageNetworkMemoryHotplug {
                base,
                ..
            }
            | HvfSnapshotV2NetworkPreparedProductParts::BalloonEntropyNetworkMemoryHotplug {
                base,
                ..
            }
            | HvfSnapshotV2NetworkPreparedProductParts::BalloonStorageEntropyNetworkMemoryHotplug {
                base,
                ..
            } => NetworkProductBase::MemoryHotplug(base),
        }
    }
}

enum NetworkProductBase<'a> {
    Static(&'a StaticNetworkProduct),
    MemoryHotplug(&'a MemoryHotplugNetworkProduct),
}

pub(crate) enum HvfSnapshotV2NetworkPreparedMemoryProduct {
    Static {
        binding: SnapshotV2MemoryBinding,
        network: PreparedSnapshotV2NetworkRestoreTopology,
    },
    MemoryHotplug {
        topology: Box<PreparedSnapshotV2MemoryHotplugTopology>,
        memory: GuestMemory,
        network: PreparedSnapshotV2NetworkRestoreTopology,
    },
}

pub(crate) struct HvfSnapshotV2NetworkPreparedOwnerParts {
    pub(crate) kind: HvfSnapshotV2NetworkProductKind,
    pub(crate) memory: HvfSnapshotV2NetworkPreparedMemoryProduct,
    pub(crate) storage: Option<PreparedSnapshotV2StorageBundle>,
    pub(crate) entropy: Option<SnapshotV2EntropyRestorePlan>,
    pub(crate) balloon: Option<SnapshotV2BalloonRestorePlan>,
}

impl HvfSnapshotV2NetworkPreparedProduct {
    pub(crate) fn into_owner_parts(self) -> HvfSnapshotV2NetworkPreparedOwnerParts {
        let kind = self.kind();
        let (memory, storage, entropy, balloon) = match self.parts {
            HvfSnapshotV2NetworkPreparedProductParts::Network(base) => {
                (static_memory_product(base), None, None, None)
            }
            HvfSnapshotV2NetworkPreparedProductParts::StorageNetwork { base, storage } => {
                (static_memory_product(base), Some(storage), None, None)
            }
            HvfSnapshotV2NetworkPreparedProductParts::EntropyNetwork { base, entropy } => {
                (static_memory_product(base), None, Some(entropy), None)
            }
            HvfSnapshotV2NetworkPreparedProductParts::StorageEntropyNetwork {
                base,
                storage,
                entropy,
            } => (
                static_memory_product(base),
                Some(storage),
                Some(entropy),
                None,
            ),
            HvfSnapshotV2NetworkPreparedProductParts::BalloonNetwork { base, balloon } => {
                (static_memory_product(base), None, None, Some(balloon))
            }
            HvfSnapshotV2NetworkPreparedProductParts::BalloonStorageNetwork {
                base,
                balloon,
                storage,
            } => (
                static_memory_product(base),
                Some(storage),
                None,
                Some(balloon),
            ),
            HvfSnapshotV2NetworkPreparedProductParts::BalloonEntropyNetwork {
                base,
                balloon,
                entropy,
            } => (
                static_memory_product(base),
                None,
                Some(entropy),
                Some(balloon),
            ),
            HvfSnapshotV2NetworkPreparedProductParts::BalloonStorageEntropyNetwork {
                base,
                balloon,
                storage,
                entropy,
            } => (
                static_memory_product(base),
                Some(storage),
                Some(entropy),
                Some(balloon),
            ),
            HvfSnapshotV2NetworkPreparedProductParts::NetworkMemoryHotplug(base) => {
                (hotplug_memory_product(base), None, None, None)
            }
            HvfSnapshotV2NetworkPreparedProductParts::StorageNetworkMemoryHotplug {
                base,
                storage,
            } => (hotplug_memory_product(base), Some(storage), None, None),
            HvfSnapshotV2NetworkPreparedProductParts::EntropyNetworkMemoryHotplug {
                base,
                entropy,
            } => (hotplug_memory_product(base), None, Some(entropy), None),
            HvfSnapshotV2NetworkPreparedProductParts::StorageEntropyNetworkMemoryHotplug {
                base,
                storage,
                entropy,
            } => (
                hotplug_memory_product(base),
                Some(storage),
                Some(entropy),
                None,
            ),
            HvfSnapshotV2NetworkPreparedProductParts::BalloonNetworkMemoryHotplug {
                base,
                balloon,
            } => (hotplug_memory_product(base), None, None, Some(balloon)),
            HvfSnapshotV2NetworkPreparedProductParts::BalloonStorageNetworkMemoryHotplug {
                base,
                balloon,
                storage,
            } => (
                hotplug_memory_product(base),
                Some(storage),
                None,
                Some(balloon),
            ),
            HvfSnapshotV2NetworkPreparedProductParts::BalloonEntropyNetworkMemoryHotplug {
                base,
                balloon,
                entropy,
            } => (
                hotplug_memory_product(base),
                None,
                Some(entropy),
                Some(balloon),
            ),
            HvfSnapshotV2NetworkPreparedProductParts::BalloonStorageEntropyNetworkMemoryHotplug {
                base,
                balloon,
                storage,
                entropy,
            } => (
                hotplug_memory_product(base),
                Some(storage),
                Some(entropy),
                Some(balloon),
            ),
        };
        HvfSnapshotV2NetworkPreparedOwnerParts {
            kind,
            memory,
            storage,
            entropy,
            balloon,
        }
    }
}

fn static_memory_product(
    product: StaticNetworkProduct,
) -> HvfSnapshotV2NetworkPreparedMemoryProduct {
    HvfSnapshotV2NetworkPreparedMemoryProduct::Static {
        binding: product.binding,
        network: product.network,
    }
}

fn hotplug_memory_product(
    product: MemoryHotplugNetworkProduct,
) -> HvfSnapshotV2NetworkPreparedMemoryProduct {
    HvfSnapshotV2NetworkPreparedMemoryProduct::MemoryHotplug {
        topology: Box::new(product.topology),
        memory: product.memory,
        network: product.network,
    }
}

impl fmt::Debug for HvfSnapshotV2NetworkPreparedProduct {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2NetworkPreparedProduct")
            .field("kind", &self.kind())
            .field("interface_count", &self.network().interfaces().len())
            .field("state", &REDACTED)
            .finish()
    }
}

/// Canonical destination layouts for one network-aware MMIO process.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfSnapshotV2NetworkMmioProcessConfig {
    balloon_layout: BalloonMmioLayout,
    storage: HvfSnapshotV2StorageMmioProcessConfig,
    network_layout: NetworkMmioLayout,
    entropy_layout: EntropyMmioLayout,
    memory_hotplug_layout: VirtioMemMmioLayout,
}

impl HvfSnapshotV2NetworkMmioProcessConfig {
    pub const fn new(
        balloon_layout: BalloonMmioLayout,
        storage: HvfSnapshotV2StorageMmioProcessConfig,
        network_layout: NetworkMmioLayout,
        entropy_layout: EntropyMmioLayout,
        memory_hotplug_layout: VirtioMemMmioLayout,
    ) -> Self {
        Self {
            balloon_layout,
            storage,
            network_layout,
            entropy_layout,
            memory_hotplug_layout,
        }
    }

    pub const fn balloon_layout(self) -> BalloonMmioLayout {
        self.balloon_layout
    }

    pub const fn storage(self) -> HvfSnapshotV2StorageMmioProcessConfig {
        self.storage
    }

    pub const fn network_layout(self) -> NetworkMmioLayout {
        self.network_layout
    }

    pub const fn entropy_layout(self) -> EntropyMmioLayout {
        self.entropy_layout
    }

    pub const fn memory_hotplug_layout(self) -> VirtioMemMmioLayout {
        self.memory_hotplug_layout
    }
}

impl fmt::Debug for HvfSnapshotV2NetworkMmioProcessConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2NetworkMmioProcessConfig")
            .field("state", &REDACTED)
            .finish()
    }
}

/// One exact saved-order network MMIO endpoint.
pub struct HvfSnapshotV2NetworkMmioEndpointPlan {
    source_index: u16,
    resource_key: SnapshotRestoreResourceKey,
    queue_ranges: [Option<[GuestMemoryRange; 3]>; VIRTIO_NET_QUEUE_COUNT],
    mmds_stack: Option<SnapshotV2MmdsInterfaceState>,
    region: MmioRegion,
    interrupt_line: GuestInterruptLine,
    fdt_device: Arm64FdtVirtioMmioDevice,
}

/// Borrowed process-resource projection used for host-free identity preflight.
#[derive(Clone, Copy)]
#[doc(hidden)]
pub struct HvfSnapshotV2NetworkProcessResourceIdentity<'a> {
    source_index: u16,
    resource_key: &'a SnapshotRestoreResourceKey,
    controller: &'a bangbang_runtime::network::NetworkInterfaceConfig,
    profile: bangbang_runtime::network::NetworkDeviceProfile,
    backend: bangbang_runtime::snapshot_network_v2_11::SnapshotV2NetworkBackendClass,
    mmds_stack: Option<SnapshotV2MmdsInterfaceState>,
}

impl<'a> HvfSnapshotV2NetworkProcessResourceIdentity<'a> {
    pub const fn new(
        source_index: u16,
        resource_key: &'a SnapshotRestoreResourceKey,
        controller: &'a bangbang_runtime::network::NetworkInterfaceConfig,
        profile: bangbang_runtime::network::NetworkDeviceProfile,
        backend: bangbang_runtime::snapshot_network_v2_11::SnapshotV2NetworkBackendClass,
        mmds_stack: Option<SnapshotV2MmdsInterfaceState>,
    ) -> Self {
        Self {
            source_index,
            resource_key,
            controller,
            profile,
            backend,
            mmds_stack,
        }
    }
}

impl fmt::Debug for HvfSnapshotV2NetworkProcessResourceIdentity<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2NetworkProcessResourceIdentity")
            .field("source_index", &self.source_index)
            .field("state", &REDACTED)
            .finish()
    }
}

impl HvfSnapshotV2NetworkMmioEndpointPlan {
    pub const fn source_index(&self) -> u16 {
        self.source_index
    }

    pub const fn resource_key(&self) -> &SnapshotRestoreResourceKey {
        &self.resource_key
    }

    pub const fn queue_ranges(&self) -> &[Option<[GuestMemoryRange; 3]>; VIRTIO_NET_QUEUE_COUNT] {
        &self.queue_ranges
    }

    pub const fn mmds_stack(&self) -> Option<SnapshotV2MmdsInterfaceState> {
        self.mmds_stack
    }

    pub const fn region(&self) -> MmioRegion {
        self.region
    }

    /// Returns the dispatcher identity derived from the exact MMIO region.
    pub const fn dispatcher_region_id(&self) -> MmioRegionId {
        self.region.id()
    }

    pub const fn interrupt_line(&self) -> GuestInterruptLine {
        self.interrupt_line
    }

    pub const fn fdt_device(&self) -> Arm64FdtVirtioMmioDevice {
        self.fdt_device
    }
}

impl fmt::Debug for HvfSnapshotV2NetworkMmioEndpointPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2NetworkMmioEndpointPlan")
            .field("source_index", &self.source_index)
            .field("state", &REDACTED)
            .finish()
    }
}

/// One exact saved-order network PCI endpoint.
pub struct HvfSnapshotV2NetworkPciEndpointPlan {
    source_index: u16,
    resource_key: SnapshotRestoreResourceKey,
    queue_ranges: [Option<[GuestMemoryRange; 3]>; VIRTIO_NET_QUEUE_COUNT],
    mmds_stack: Option<SnapshotV2MmdsInterfaceState>,
    origin: StorageDeviceOrigin,
    sbdf: PciSbdf,
    bar_region_id: MmioRegionId,
    bar_range: GuestMemoryRange,
    route_count: usize,
    queue_vectors: [u16; VIRTIO_NET_QUEUE_COUNT],
    config_vector: u16,
    msi_interrupt_count: u32,
}

impl HvfSnapshotV2NetworkPciEndpointPlan {
    pub const fn source_index(&self) -> u16 {
        self.source_index
    }

    pub const fn resource_key(&self) -> &SnapshotRestoreResourceKey {
        &self.resource_key
    }

    pub const fn queue_ranges(&self) -> &[Option<[GuestMemoryRange; 3]>; VIRTIO_NET_QUEUE_COUNT] {
        &self.queue_ranges
    }

    pub const fn mmds_stack(&self) -> Option<SnapshotV2MmdsInterfaceState> {
        self.mmds_stack
    }

    pub const fn origin(&self) -> StorageDeviceOrigin {
        self.origin
    }

    pub const fn sbdf(&self) -> PciSbdf {
        self.sbdf
    }

    pub const fn bar_region_id(&self) -> MmioRegionId {
        self.bar_region_id
    }

    /// Returns the dispatcher identity derived from the canonical capability BAR.
    pub const fn dispatcher_region_id(&self) -> MmioRegionId {
        self.bar_region_id
    }

    pub const fn bar_range(&self) -> GuestMemoryRange {
        self.bar_range
    }

    pub const fn route_count(&self) -> usize {
        self.route_count
    }

    pub const fn queue_vectors(&self) -> &[u16; VIRTIO_NET_QUEUE_COUNT] {
        &self.queue_vectors
    }

    pub const fn config_vector(&self) -> u16 {
        self.config_vector
    }

    pub const fn msi_interrupt_count(&self) -> u32 {
        self.msi_interrupt_count
    }
}

impl fmt::Debug for HvfSnapshotV2NetworkPciEndpointPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2NetworkPciEndpointPlan")
            .field("source_index", &self.source_index)
            .field("state", &REDACTED)
            .finish()
    }
}

/// Placement of one optional singleton MMIO endpoint in a network product.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfSnapshotV2NetworkAuxiliaryMmioEndpointPlan {
    region: MmioRegion,
    interrupt_line: GuestInterruptLine,
    fdt_device: Arm64FdtVirtioMmioDevice,
}

impl HvfSnapshotV2NetworkAuxiliaryMmioEndpointPlan {
    pub const fn region(self) -> MmioRegion {
        self.region
    }

    pub const fn dispatcher_region_id(self) -> MmioRegionId {
        self.region.id()
    }

    pub const fn interrupt_line(self) -> GuestInterruptLine {
        self.interrupt_line
    }

    pub const fn fdt_device(self) -> Arm64FdtVirtioMmioDevice {
        self.fdt_device
    }
}

impl fmt::Debug for HvfSnapshotV2NetworkAuxiliaryMmioEndpointPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2NetworkAuxiliaryMmioEndpointPlan")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Placement of one optional singleton PCI endpoint in a network product.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfSnapshotV2NetworkAuxiliaryPciEndpointPlan {
    origin: StorageDeviceOrigin,
    sbdf: PciSbdf,
    bar_region_id: MmioRegionId,
    bar_range: GuestMemoryRange,
    route_count: usize,
    msi_interrupt_count: u32,
}

impl HvfSnapshotV2NetworkAuxiliaryPciEndpointPlan {
    pub const fn origin(self) -> StorageDeviceOrigin {
        self.origin
    }

    pub const fn sbdf(self) -> PciSbdf {
        self.sbdf
    }

    pub const fn bar_region_id(self) -> MmioRegionId {
        self.bar_region_id
    }

    pub const fn dispatcher_region_id(self) -> MmioRegionId {
        self.bar_region_id
    }

    pub const fn bar_range(self) -> GuestMemoryRange {
        self.bar_range
    }

    pub const fn route_count(self) -> usize {
        self.route_count
    }

    pub const fn msi_interrupt_count(self) -> u32 {
        self.msi_interrupt_count
    }
}

impl fmt::Debug for HvfSnapshotV2NetworkAuxiliaryPciEndpointPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2NetworkAuxiliaryPciEndpointPlan")
            .field("state", &REDACTED)
            .finish()
    }
}

const FOLLOWING_ENDPOINT_QUEUE_COUNT: usize = 3;

#[derive(Clone, Copy)]
pub(crate) struct HvfSnapshotV2NetworkMmioFollowingEndpointInput {
    pub(crate) region: MmioRegion,
    pub(crate) interrupt_line: GuestInterruptLine,
    pub(crate) queue_ranges: [Option<[GuestMemoryRange; 3]>; FOLLOWING_ENDPOINT_QUEUE_COUNT],
}

#[derive(Clone, Copy)]
pub(crate) struct HvfSnapshotV2NetworkMmioFollowingEndpointPlan {
    pub(crate) placement: HvfSnapshotV2NetworkAuxiliaryMmioEndpointPlan,
    pub(crate) queue_ranges: [Option<[GuestMemoryRange; 3]>; FOLLOWING_ENDPOINT_QUEUE_COUNT],
}

pub(crate) struct HvfSnapshotV2NetworkPciFollowingEndpointInput<'a> {
    pub(crate) origin: StorageDeviceOrigin,
    pub(crate) phase: VirtioPciEndpointPhase,
    pub(crate) sbdf: PciSbdf,
    pub(crate) bar_range: GuestMemoryRange,
    pub(crate) queue_ranges: [Option<[GuestMemoryRange; 3]>; FOLLOWING_ENDPOINT_QUEUE_COUNT],
    pub(crate) msix: &'a VirtioPciMsixState,
}

#[derive(Clone, Copy)]
pub(crate) struct HvfSnapshotV2NetworkPciFollowingEndpointPlan {
    pub(crate) placement: HvfSnapshotV2NetworkAuxiliaryPciEndpointPlan,
    pub(crate) queue_ranges: [Option<[GuestMemoryRange; 3]>; FOLLOWING_ENDPOINT_QUEUE_COUNT],
    pub(crate) queue_vectors: [u16; FOLLOWING_ENDPOINT_QUEUE_COUNT],
    pub(crate) config_vector: u16,
}

/// Complete immutable exact-2.11 MMIO product proof.
pub struct HvfSnapshotV2NetworkMmioPlatformPlan {
    product: HvfSnapshotV2NetworkPreparedProduct,
    mapping: Option<HvfSnapshotV2MemoryHotplugMappingPlan>,
    balloon: Option<HvfSnapshotV2BalloonMmioEndpointPlan>,
    storage: Option<HvfSnapshotV2StorageMmioPlatformPlan>,
    network: Vec<HvfSnapshotV2NetworkMmioEndpointPlan>,
    entropy: Option<HvfSnapshotV2NetworkAuxiliaryMmioEndpointPlan>,
    memory_hotplug: Option<HvfSnapshotV2NetworkAuxiliaryMmioEndpointPlan>,
    serial_interrupt: GuestInterruptLine,
    vmgenid_interrupt: GuestInterruptLine,
    vmclock_interrupt: GuestInterruptLine,
}

impl HvfSnapshotV2NetworkMmioPlatformPlan {
    pub const fn kind(&self) -> HvfSnapshotV2NetworkProductKind {
        self.product.kind()
    }

    pub const fn product(&self) -> &HvfSnapshotV2NetworkPreparedProduct {
        &self.product
    }

    pub const fn mapping(&self) -> Option<&HvfSnapshotV2MemoryHotplugMappingPlan> {
        self.mapping.as_ref()
    }

    pub const fn balloon(&self) -> Option<HvfSnapshotV2BalloonMmioEndpointPlan> {
        self.balloon
    }

    pub const fn storage(&self) -> Option<&HvfSnapshotV2StorageMmioPlatformPlan> {
        self.storage.as_ref()
    }

    pub fn network(&self) -> &[HvfSnapshotV2NetworkMmioEndpointPlan] {
        &self.network
    }

    pub const fn entropy(&self) -> Option<HvfSnapshotV2NetworkAuxiliaryMmioEndpointPlan> {
        self.entropy
    }

    pub const fn memory_hotplug(&self) -> Option<HvfSnapshotV2NetworkAuxiliaryMmioEndpointPlan> {
        self.memory_hotplug
    }

    pub const fn serial_interrupt(&self) -> GuestInterruptLine {
        self.serial_interrupt
    }

    pub const fn vmgenid_interrupt(&self) -> GuestInterruptLine {
        self.vmgenid_interrupt
    }

    pub const fn vmclock_interrupt(&self) -> GuestInterruptLine {
        self.vmclock_interrupt
    }

    /// Proves that a separately retained process resource blueprint still
    /// describes this exact saved-order MMIO product. This performs no host
    /// operation and retains no caller borrow.
    #[doc(hidden)]
    pub fn preflight_process_resource_identity<'a>(
        &self,
        resources: impl ExactSizeIterator<Item = HvfSnapshotV2NetworkProcessResourceIdentity<'a>>,
        mmds_state: Option<&bangbang_runtime::snapshot_network_v2_11::SnapshotV2MmdsState>,
        mmds_controller: Option<&bangbang_runtime::mmds::MmdsConfig>,
    ) -> bool {
        let topology = self.product.network();
        if resources.len() != self.network.len()
            || topology.interfaces().len() != self.network.len()
            || topology.mmds_state() != mmds_state
            || topology.mmds_controller() != mmds_controller
        {
            return false;
        }
        resources.zip(topology.interfaces()).zip(&self.network).all(
            |((resource, interface), endpoint)| {
                resource.source_index == interface.source_index()
                    && resource.source_index == endpoint.source_index()
                    && resource.resource_key == interface.resource_key()
                    && resource.resource_key == endpoint.resource_key()
                    && resource.resource_key.resource_class()
                        == SnapshotRestoreResourceClass::NetworkPacketIo
                    && resource.controller == interface.controller()
                    && resource.profile == interface.portable().profile()
                    && resource.backend == interface.portable().backend()
                    && resource.mmds_stack == interface.mmds_stack()
                    && resource.mmds_stack == endpoint.mmds_stack()
            },
        )
    }
}

impl fmt::Debug for HvfSnapshotV2NetworkMmioPlatformPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2NetworkMmioPlatformPlan")
            .field("kind", &self.kind())
            .field("interface_count", &self.network.len())
            .field("state", &REDACTED)
            .finish()
    }
}

pub(crate) struct HvfSnapshotV2NetworkMmioPlatformOwnerParts {
    pub(crate) product: HvfSnapshotV2NetworkPreparedProduct,
    pub(crate) mapping: Option<HvfSnapshotV2MemoryHotplugMappingPlan>,
    pub(crate) balloon: Option<HvfSnapshotV2BalloonMmioEndpointPlan>,
    pub(crate) storage: Option<HvfSnapshotV2StorageMmioPlatformPlan>,
    pub(crate) network: Vec<HvfSnapshotV2NetworkMmioEndpointPlan>,
    pub(crate) entropy: Option<HvfSnapshotV2NetworkAuxiliaryMmioEndpointPlan>,
    pub(crate) memory_hotplug: Option<HvfSnapshotV2NetworkAuxiliaryMmioEndpointPlan>,
    pub(crate) serial_interrupt: GuestInterruptLine,
    pub(crate) vmgenid_interrupt: GuestInterruptLine,
    pub(crate) vmclock_interrupt: GuestInterruptLine,
}

impl HvfSnapshotV2NetworkMmioPlatformPlan {
    pub(crate) fn into_owner_parts(self) -> HvfSnapshotV2NetworkMmioPlatformOwnerParts {
        HvfSnapshotV2NetworkMmioPlatformOwnerParts {
            product: self.product,
            mapping: self.mapping,
            balloon: self.balloon,
            storage: self.storage,
            network: self.network,
            entropy: self.entropy,
            memory_hotplug: self.memory_hotplug,
            serial_interrupt: self.serial_interrupt,
            vmgenid_interrupt: self.vmgenid_interrupt,
            vmclock_interrupt: self.vmclock_interrupt,
        }
    }
}

/// Complete immutable exact-2.11 PCI product proof.
pub struct HvfSnapshotV2NetworkPciPlatformPlan {
    product: HvfSnapshotV2NetworkPreparedProduct,
    mapping: Option<HvfSnapshotV2MemoryHotplugMappingPlan>,
    balloon: Option<HvfSnapshotV2BalloonPciEndpointPlan>,
    storage: Option<HvfSnapshotV2StoragePciPlatformPlan>,
    network: Vec<HvfSnapshotV2NetworkPciEndpointPlan>,
    entropy: Option<HvfSnapshotV2NetworkAuxiliaryPciEndpointPlan>,
    memory_hotplug: Option<HvfSnapshotV2NetworkAuxiliaryPciEndpointPlan>,
    host: Arm64FdtPciHost,
    msi: HvfGicMsiMetadata,
    endpoint_count: usize,
    route_demand: usize,
    serial_interrupt: GuestInterruptLine,
    vmgenid_interrupt: GuestInterruptLine,
    vmclock_interrupt: GuestInterruptLine,
}

impl HvfSnapshotV2NetworkPciPlatformPlan {
    pub const fn kind(&self) -> HvfSnapshotV2NetworkProductKind {
        self.product.kind()
    }

    pub const fn product(&self) -> &HvfSnapshotV2NetworkPreparedProduct {
        &self.product
    }

    pub const fn mapping(&self) -> Option<&HvfSnapshotV2MemoryHotplugMappingPlan> {
        self.mapping.as_ref()
    }

    pub const fn balloon(&self) -> Option<HvfSnapshotV2BalloonPciEndpointPlan> {
        self.balloon
    }

    pub const fn storage(&self) -> Option<&HvfSnapshotV2StoragePciPlatformPlan> {
        self.storage.as_ref()
    }

    pub fn network(&self) -> &[HvfSnapshotV2NetworkPciEndpointPlan] {
        &self.network
    }

    pub const fn entropy(&self) -> Option<HvfSnapshotV2NetworkAuxiliaryPciEndpointPlan> {
        self.entropy
    }

    pub const fn memory_hotplug(&self) -> Option<HvfSnapshotV2NetworkAuxiliaryPciEndpointPlan> {
        self.memory_hotplug
    }

    pub const fn host(&self) -> Arm64FdtPciHost {
        self.host
    }

    pub const fn msi(&self) -> HvfGicMsiMetadata {
        self.msi
    }

    pub const fn endpoint_count(&self) -> usize {
        self.endpoint_count
    }

    pub const fn route_demand(&self) -> usize {
        self.route_demand
    }

    pub const fn serial_interrupt(&self) -> GuestInterruptLine {
        self.serial_interrupt
    }

    pub const fn vmgenid_interrupt(&self) -> GuestInterruptLine {
        self.vmgenid_interrupt
    }

    pub const fn vmclock_interrupt(&self) -> GuestInterruptLine {
        self.vmclock_interrupt
    }

    /// Proves that a separately retained process resource blueprint still
    /// describes this exact saved-order PCI product. This performs no host
    /// operation and retains no caller borrow.
    #[doc(hidden)]
    pub fn preflight_process_resource_identity<'a>(
        &self,
        resources: impl ExactSizeIterator<Item = HvfSnapshotV2NetworkProcessResourceIdentity<'a>>,
        mmds_state: Option<&bangbang_runtime::snapshot_network_v2_11::SnapshotV2MmdsState>,
        mmds_controller: Option<&bangbang_runtime::mmds::MmdsConfig>,
    ) -> bool {
        let topology = self.product.network();
        if resources.len() != self.network.len()
            || topology.interfaces().len() != self.network.len()
            || topology.mmds_state() != mmds_state
            || topology.mmds_controller() != mmds_controller
        {
            return false;
        }
        resources.zip(topology.interfaces()).zip(&self.network).all(
            |((resource, interface), endpoint)| {
                resource.source_index == interface.source_index()
                    && resource.source_index == endpoint.source_index()
                    && resource.resource_key == interface.resource_key()
                    && resource.resource_key == endpoint.resource_key()
                    && resource.resource_key.resource_class()
                        == SnapshotRestoreResourceClass::NetworkPacketIo
                    && resource.controller == interface.controller()
                    && resource.profile == interface.portable().profile()
                    && resource.backend == interface.portable().backend()
                    && resource.mmds_stack == interface.mmds_stack()
                    && resource.mmds_stack == endpoint.mmds_stack()
            },
        )
    }
}

impl fmt::Debug for HvfSnapshotV2NetworkPciPlatformPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2NetworkPciPlatformPlan")
            .field("kind", &self.kind())
            .field("interface_count", &self.network.len())
            .field("state", &REDACTED)
            .finish()
    }
}

pub(crate) struct HvfSnapshotV2NetworkPciPlatformOwnerParts {
    pub(crate) product: HvfSnapshotV2NetworkPreparedProduct,
    pub(crate) mapping: Option<HvfSnapshotV2MemoryHotplugMappingPlan>,
    pub(crate) balloon: Option<HvfSnapshotV2BalloonPciEndpointPlan>,
    pub(crate) storage: Option<HvfSnapshotV2StoragePciPlatformPlan>,
    pub(crate) network: Vec<HvfSnapshotV2NetworkPciEndpointPlan>,
    pub(crate) entropy: Option<HvfSnapshotV2NetworkAuxiliaryPciEndpointPlan>,
    pub(crate) memory_hotplug: Option<HvfSnapshotV2NetworkAuxiliaryPciEndpointPlan>,
    pub(crate) host: Arm64FdtPciHost,
    pub(crate) msi: HvfGicMsiMetadata,
    pub(crate) endpoint_count: usize,
    pub(crate) route_demand: usize,
    pub(crate) serial_interrupt: GuestInterruptLine,
    pub(crate) vmgenid_interrupt: GuestInterruptLine,
    pub(crate) vmclock_interrupt: GuestInterruptLine,
}

impl HvfSnapshotV2NetworkPciPlatformPlan {
    pub(crate) fn into_owner_parts(self) -> HvfSnapshotV2NetworkPciPlatformOwnerParts {
        HvfSnapshotV2NetworkPciPlatformOwnerParts {
            product: self.product,
            mapping: self.mapping,
            balloon: self.balloon,
            storage: self.storage,
            network: self.network,
            entropy: self.entropy,
            memory_hotplug: self.memory_hotplug,
            host: self.host,
            msi: self.msi,
            endpoint_count: self.endpoint_count,
            route_demand: self.route_demand,
            serial_interrupt: self.serial_interrupt,
            vmgenid_interrupt: self.vmgenid_interrupt,
            vmclock_interrupt: self.vmclock_interrupt,
        }
    }
}

/// Stable cancellation checkpoints before a network platform plan is published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfSnapshotV2NetworkPlatformPlanStage {
    Start,
    Product,
    Interface,
    Components,
    Inventory,
    Completion,
}

/// Redacted rejection from exact-2.11 network platform-product planning.
pub enum PrepareHvfSnapshotV2NetworkPlatformPlanError {
    PlatformProfile,
    Binding,
    Cardinality,
    TransportPolicy,
    Interface,
    Mmds,
    ResourcePlan,
    Placement,
    FixedResource,
    Mapping(Box<PrepareHvfSnapshotV2MemoryHotplugMappingPlanError>),
    PciCapacity {
        count: usize,
        maximum: usize,
    },
    RangeConflict,
    RouteConflict,
    Allocation,
    Cancelled {
        stage: HvfSnapshotV2NetworkPlatformPlanStage,
    },
    Balloon(Box<PrepareHvfSnapshotV2BalloonPlatformPlanError>),
    StorageMmio(Box<PrepareHvfSnapshotV2StorageMmioPlatformPlanError>),
    StoragePci(Box<PrepareHvfSnapshotV2StoragePciPlatformPlanError>),
}

impl fmt::Debug for PrepareHvfSnapshotV2NetworkPlatformPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let category = match self {
            Self::PlatformProfile => "platform-profile",
            Self::Binding => "binding",
            Self::Cardinality => "cardinality",
            Self::TransportPolicy => "transport-policy",
            Self::Interface => "interface",
            Self::Mmds => "mmds",
            Self::ResourcePlan => "resource-plan",
            Self::Placement => "placement",
            Self::FixedResource => "fixed-resource",
            Self::Mapping(_) => "mapping",
            Self::PciCapacity { .. } => "pci-capacity",
            Self::RangeConflict => "range-conflict",
            Self::RouteConflict => "route-conflict",
            Self::Allocation => "allocation",
            Self::Cancelled { .. } => "cancelled",
            Self::Balloon(_) => "balloon",
            Self::StorageMmio(_) => "storage-mmio",
            Self::StoragePci(_) => "storage-pci",
        };
        formatter
            .debug_struct("PrepareHvfSnapshotV2NetworkPlatformPlanError")
            .field("category", &category)
            .field("state", &REDACTED)
            .finish()
    }
}

impl fmt::Display for PrepareHvfSnapshotV2NetworkPlatformPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::PlatformProfile => "native-v2 network platform profile is not canonical",
            Self::Binding => "native-v2 network platform binding is inconsistent",
            Self::Cardinality => "native-v2 network interface cardinality is inconsistent",
            Self::TransportPolicy => "native-v2 network product transport is inconsistent",
            Self::Interface => "native-v2 network interface projection is inconsistent",
            Self::Mmds => "native-v2 network MMDS projection is inconsistent",
            Self::ResourcePlan => "native-v2 network platform resources are inconsistent",
            Self::Placement => "native-v2 network endpoint placement is inconsistent",
            Self::FixedResource => "native-v2 network fixed resources are inconsistent",
            Self::Mapping(_) => "native-v2 network memory-hotplug mapping planning failed",
            Self::PciCapacity { .. } => "native-v2 network PCI endpoint capacity is exceeded",
            Self::RangeConflict => "native-v2 network product ranges overlap another owner",
            Self::RouteConflict => "native-v2 network active interrupt routes are inconsistent",
            Self::Allocation => "native-v2 network temporary inventory allocation failed",
            Self::Cancelled { .. } => "native-v2 network platform planning was cancelled",
            Self::Balloon(_) => "native-v2 network balloon planning failed",
            Self::StorageMmio(_) => "native-v2 network MMIO storage planning failed",
            Self::StoragePci(_) => "native-v2 network PCI storage planning failed",
        })
    }
}

impl std::error::Error for PrepareHvfSnapshotV2NetworkPlatformPlanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Mapping(source) => Some(source),
            Self::Balloon(source) => Some(source),
            Self::StorageMmio(source) => Some(source),
            Self::StoragePci(source) => Some(source),
            Self::PlatformProfile
            | Self::Binding
            | Self::Cardinality
            | Self::TransportPolicy
            | Self::Interface
            | Self::Mmds
            | Self::ResourcePlan
            | Self::Placement
            | Self::FixedResource
            | Self::PciCapacity { .. }
            | Self::RangeConflict
            | Self::RouteConflict
            | Self::Allocation
            | Self::Cancelled { .. } => None,
        }
    }
}

pub(crate) trait NetworkPlatformPlanReserve {
    fn reserve<T>(
        &mut self,
        values: &mut Vec<T>,
        additional: usize,
    ) -> Result<(), PrepareHvfSnapshotV2NetworkPlatformPlanError>;

    fn clone_key(
        &mut self,
        key: &SnapshotRestoreResourceKey,
    ) -> Result<SnapshotRestoreResourceKey, PrepareHvfSnapshotV2NetworkPlatformPlanError>;
}

pub(crate) struct SystemNetworkPlatformPlanReserve;

impl NetworkPlatformPlanReserve for SystemNetworkPlatformPlanReserve {
    fn reserve<T>(
        &mut self,
        values: &mut Vec<T>,
        additional: usize,
    ) -> Result<(), PrepareHvfSnapshotV2NetworkPlatformPlanError> {
        values
            .try_reserve_exact(additional)
            .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::Allocation)
    }

    fn clone_key(
        &mut self,
        key: &SnapshotRestoreResourceKey,
    ) -> Result<SnapshotRestoreResourceKey, PrepareHvfSnapshotV2NetworkPlatformPlanError> {
        key.try_clone()
            .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::Allocation)
    }
}

fn check_cancel(
    is_cancelled: &mut impl FnMut(HvfSnapshotV2NetworkPlatformPlanStage) -> bool,
    stage: HvfSnapshotV2NetworkPlatformPlanStage,
) -> Result<(), PrepareHvfSnapshotV2NetworkPlatformPlanError> {
    if is_cancelled(stage) {
        Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::Cancelled { stage })
    } else {
        Ok(())
    }
}

pub(crate) fn validate_product(
    platform: &HvfSnapshotV2PlatformState,
    product: &HvfSnapshotV2NetworkPreparedProduct,
) -> Result<SnapshotV2DeviceTransportKind, PrepareHvfSnapshotV2NetworkPlatformPlanError> {
    if !platform.machine().fdt().is_product_process_profile()
        || platform.time().rtc_layout()
            != RtcMmioLayout::new(PROCESS_RTC_MMIO_BASE, PROCESS_RTC_MMIO_REGION_ID)
    {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::PlatformProfile);
    }
    if platform.memory() != product.memory_binding() {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::Binding);
    }
    let interfaces = product.network().interfaces();
    if interfaces.len() > NATIVE_V2_NETWORK_MAX_INTERFACES {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::Cardinality);
    }
    let transport_kind = product.network().transport_kind();
    if product
        .storage()
        .is_some_and(|storage| storage.transport_kind() != transport_kind)
        || product
            .entropy()
            .is_some_and(|entropy| entropy.transport_kind() != transport_kind)
        || product
            .balloon()
            .is_some_and(|balloon| balloon.transport_kind() != transport_kind)
        || product
            .memory_hotplug_topology()
            .is_some_and(|topology| topology.state().transport().kind() != transport_kind)
    {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::TransportPolicy);
    }
    for (index, interface) in interfaces.iter().enumerate() {
        let source_index = u16::try_from(index)
            .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::Interface)?;
        let instance = u32::try_from(index)
            .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::Interface)?;
        let key = interface.resource_key();
        let portable = interface.portable();
        let controller = interface.controller();
        if interface.source_index() != source_index
            || key.resource_class() != SnapshotRestoreResourceClass::NetworkPacketIo
            || key.device_key().kind() != NETWORK_DEVICE_KIND
            || key.device_key().instance() != instance
            || key.public_id().as_str() != portable.iface_id()
            || controller.iface_id() != portable.iface_id()
            || controller.guest_mac() != portable.requested_guest_mac()
            || controller.mtu() != portable.requested_mtu()
            || controller.rx_rate_limiter() != limiter_config(portable.rx_limiter())
            || controller.tx_rate_limiter() != limiter_config(portable.tx_limiter())
            || portable.transport().kind() != transport_kind
            || portable.virtio().queues().len() != VIRTIO_NET_QUEUE_COUNT
            || interface
                .mmds_stack()
                .is_some_and(|stack| stack.interface_index() != source_index)
        {
            return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::Interface);
        }
    }
    validate_mmds(product)?;
    Ok(transport_kind)
}

fn limiter_config(limiter: SnapshotV2NetworkLimiterState) -> Option<NetworkRateLimiterConfig> {
    let configured = NetworkRateLimiterConfig::new(
        limiter.bandwidth().map(token_bucket_config),
        limiter.ops().map(token_bucket_config),
    );
    configured.is_configured().then_some(configured)
}

fn token_bucket_config(bucket: SnapshotV2NetworkTokenBucketState) -> NetworkTokenBucketConfig {
    NetworkTokenBucketConfig::new(
        bucket.size(),
        bucket.configured_burst(),
        bucket.refill_time_millis(),
    )
}

fn validate_mmds(
    product: &HvfSnapshotV2NetworkPreparedProduct,
) -> Result<(), PrepareHvfSnapshotV2NetworkPlatformPlanError> {
    let topology = product.network();
    match (topology.mmds_state(), topology.mmds_controller()) {
        (None, None) => {
            if topology
                .interfaces()
                .iter()
                .any(|interface| interface.mmds_stack().is_some())
            {
                return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::Mmds);
            }
        }
        (Some(state), Some(controller)) => {
            if state.version() != controller.version()
                || state.ipv4_address() != controller.ipv4_address()
                || state.imds_compat() != controller.imds_compat()
                || state.interfaces().len() != controller.network_interfaces().len()
            {
                return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::Mmds);
            }
            for (selected, controller_id) in state
                .interfaces()
                .iter()
                .copied()
                .zip(controller.network_interfaces())
            {
                let Some(interface) = topology
                    .interfaces()
                    .get(usize::from(selected.interface_index()))
                else {
                    return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::Mmds);
                };
                if interface.mmds_stack() != Some(selected)
                    || interface.controller().iface_id() != controller_id
                {
                    return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::Mmds);
                }
            }
        }
        _ => return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::Mmds),
    }
    Ok(())
}

fn queue_ranges(
    queue: &SnapshotV2VirtioQueueState,
) -> Result<Option<[GuestMemoryRange; 3]>, PrepareHvfSnapshotV2NetworkPlatformPlanError> {
    if queue.size() == 0 {
        return Ok(None);
    }
    let descriptor_size = u64::from(queue.size())
        .checked_mul(16)
        .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    let available_size = u64::from(queue.size())
        .checked_mul(2)
        .and_then(|size| size.checked_add(6))
        .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    let used_size = u64::from(queue.size())
        .checked_mul(8)
        .and_then(|size| size.checked_add(6))
        .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    Ok(Some([
        GuestMemoryRange::new(queue.descriptor_table(), descriptor_size)
            .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?,
        GuestMemoryRange::new(queue.driver_ring(), available_size)
            .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?,
        GuestMemoryRange::new(queue.device_ring(), used_size)
            .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?,
    ]))
}

fn interface_queue_ranges(
    interface: &PreparedSnapshotV2NetworkRestoreInterface,
) -> Result<
    [Option<[GuestMemoryRange; 3]>; VIRTIO_NET_QUEUE_COUNT],
    PrepareHvfSnapshotV2NetworkPlatformPlanError,
> {
    let [rx, tx] = interface.portable().virtio().queues() else {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::Interface);
    };
    Ok([queue_ranges(rx)?, queue_ranges(tx)?])
}

fn prepare_mapping(
    platform: &HvfSnapshotV2PlatformState,
    product: &HvfSnapshotV2NetworkPreparedProduct,
) -> Result<
    Option<HvfSnapshotV2MemoryHotplugMappingPlan>,
    PrepareHvfSnapshotV2NetworkPlatformPlanError,
> {
    let (Some(topology), Some(memory)) = (
        product.memory_hotplug_topology(),
        product.memory_hotplug_memory(),
    ) else {
        return Ok(None);
    };
    let expected_base_bytes = platform
        .machine()
        .machine()
        .mem_size_mib()
        .checked_mul(MIB)
        .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    prepare_hvf_snapshot_v2_memory_hotplug_mapping_plan(topology, memory, expected_base_bytes)
        .map(Some)
        .map_err(|source| PrepareHvfSnapshotV2NetworkPlatformPlanError::Mapping(Box::new(source)))
}

fn mmio_fdt_device(
    region: MmioRegion,
    interrupt_line: GuestInterruptLine,
) -> Arm64FdtVirtioMmioDevice {
    Arm64FdtVirtioMmioDevice {
        region: Arm64FdtRegion {
            base: region.range().start().raw_value(),
            size: region.range().size(),
        },
        interrupt_line,
    }
}

fn mmio_auxiliary_endpoint(
    platform: &HvfSnapshotV2PlatformState,
    region: MmioRegion,
    interrupt_line: GuestInterruptLine,
    queue_ranges: Option<[GuestMemoryRange; 3]>,
) -> Result<
    HvfSnapshotV2NetworkAuxiliaryMmioEndpointPlan,
    PrepareHvfSnapshotV2NetworkPlatformPlanError,
> {
    if mmio_region_conflicts_with_platform(
        platform,
        region,
        &platform.global().compatibility().gic_metadata(),
    )
    .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?
    {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::FixedResource);
    }
    if queue_ranges_conflict_with_platform(platform, queue_ranges)
        .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?
    {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::RangeConflict);
    }
    Ok(HvfSnapshotV2NetworkAuxiliaryMmioEndpointPlan {
        region,
        interrupt_line,
        fdt_device: mmio_fdt_device(region, interrupt_line),
    })
}

fn prepare_mmio_following_endpoint(
    platform: &HvfSnapshotV2PlatformState,
    input: HvfSnapshotV2NetworkMmioFollowingEndpointInput,
) -> Result<
    HvfSnapshotV2NetworkMmioFollowingEndpointPlan,
    PrepareHvfSnapshotV2NetworkPlatformPlanError,
> {
    let placement = mmio_auxiliary_endpoint(platform, input.region, input.interrupt_line, None)?;
    for ranges in input.queue_ranges.iter().copied().flatten() {
        if queue_ranges_conflict_with_platform(platform, Some(ranges))
            .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?
        {
            return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::RangeConflict);
        }
    }
    Ok(HvfSnapshotV2NetworkMmioFollowingEndpointPlan {
        placement,
        queue_ranges: input.queue_ranges,
    })
}

fn prepare_network_mmio_endpoint(
    platform: &HvfSnapshotV2PlatformState,
    interface: &PreparedSnapshotV2NetworkRestoreInterface,
    layout: NetworkMmioLayout,
    index: usize,
    reserve: &mut impl NetworkPlatformPlanReserve,
) -> Result<HvfSnapshotV2NetworkMmioEndpointPlan, PrepareHvfSnapshotV2NetworkPlatformPlanError> {
    let region = layout
        .region_at(index)
        .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    let SnapshotV2DeviceTransport::Mmio(transport) = interface.portable().transport() else {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::TransportPolicy);
    };
    let queue_ranges = interface_queue_ranges(interface)?;
    if transport.region() != region {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::Placement);
    }
    if mmio_region_conflicts_with_platform(
        platform,
        region,
        &platform.global().compatibility().gic_metadata(),
    )
    .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?
    {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::FixedResource);
    }
    for ranges in queue_ranges.iter().copied().flatten() {
        if queue_ranges_conflict_with_platform(platform, Some(ranges))
            .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?
        {
            return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::RangeConflict);
        }
    }
    let resource_key = reserve.clone_key(interface.resource_key())?;
    let interrupt_line = transport.interrupt_line();
    Ok(HvfSnapshotV2NetworkMmioEndpointPlan {
        source_index: interface.source_index(),
        resource_key,
        queue_ranges,
        mmds_stack: interface.mmds_stack(),
        region,
        interrupt_line,
        fdt_device: mmio_fdt_device(region, interrupt_line),
    })
}

fn prepare_entropy_mmio_endpoint(
    platform: &HvfSnapshotV2PlatformState,
    entropy: &SnapshotV2EntropyRestorePlan,
    layout: EntropyMmioLayout,
) -> Result<
    HvfSnapshotV2NetworkAuxiliaryMmioEndpointPlan,
    PrepareHvfSnapshotV2NetworkPlatformPlanError,
> {
    let region = MmioRegion::new(
        layout.region_id(),
        layout.address(),
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
    )
    .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    let PreparedSnapshotV2EntropyTransport::Mmio(transport) = entropy.transport() else {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::TransportPolicy);
    };
    if transport.region() != region {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::Placement);
    }
    mmio_auxiliary_endpoint(
        platform,
        region,
        transport.interrupt_line(),
        entropy.queue_ranges(),
    )
}

fn prepare_memory_hotplug_mmio_endpoint(
    platform: &HvfSnapshotV2PlatformState,
    topology: &PreparedSnapshotV2MemoryHotplugTopology,
    layout: VirtioMemMmioLayout,
) -> Result<
    HvfSnapshotV2NetworkAuxiliaryMmioEndpointPlan,
    PrepareHvfSnapshotV2NetworkPlatformPlanError,
> {
    let region = MmioRegion::new(
        layout.region_id(),
        layout.address(),
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
    )
    .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    let SnapshotV2DeviceTransport::Mmio(transport) = topology.state().transport() else {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::TransportPolicy);
    };
    if transport.region() != region {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::Placement);
    }
    mmio_auxiliary_endpoint(
        platform,
        region,
        transport.interrupt_line(),
        topology.queue_ranges(),
    )
}

fn validate_mmio_interrupt_sequence(
    platform: &HvfSnapshotV2PlatformState,
    balloon: Option<HvfSnapshotV2BalloonMmioEndpointPlan>,
    storage: Option<&HvfSnapshotV2StorageMmioPlatformPlan>,
    network: &[HvfSnapshotV2NetworkMmioEndpointPlan],
    following: Option<HvfSnapshotV2NetworkMmioFollowingEndpointPlan>,
    entropy: Option<HvfSnapshotV2NetworkAuxiliaryMmioEndpointPlan>,
    memory_hotplug: Option<HvfSnapshotV2NetworkAuxiliaryMmioEndpointPlan>,
) -> Result<
    (GuestInterruptLine, GuestInterruptLine, GuestInterruptLine),
    PrepareHvfSnapshotV2NetworkPlatformPlanError,
> {
    let gic = platform.global().compatibility().gic_metadata();
    if gic.msi.is_some() {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::TransportPolicy);
    }
    let mut allocator = HvfGicInterruptLineAllocator::from_metadata(&gic)
        .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    let mut validate = |expected: GuestInterruptLine| {
        let actual = allocator
            .allocate()
            .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
        if actual == expected {
            Ok(())
        } else {
            Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)
        }
    };
    if let Some(balloon) = balloon {
        validate(balloon.interrupt_line())?;
    }
    if let Some(storage) = storage {
        for record in storage.block_records() {
            validate(record.interrupt_line())?;
        }
    }
    for endpoint in network {
        validate(endpoint.interrupt_line())?;
    }
    if let Some(storage) = storage {
        for record in storage.pmem_records() {
            validate(record.interrupt_line())?;
        }
    }
    if let Some(following) = following {
        validate(following.placement.interrupt_line())?;
    }
    if let Some(entropy) = entropy {
        validate(entropy.interrupt_line())?;
    }
    if let Some(memory_hotplug) = memory_hotplug {
        validate(memory_hotplug.interrupt_line())?;
    }
    let serial = allocator
        .allocate()
        .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    let vmgenid = allocator
        .allocate()
        .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    let vmclock = allocator
        .allocate()
        .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    if storage.is_some_and(|storage| {
        storage.serial_interrupt() != serial
            || storage.vmgenid_interrupt() != vmgenid
            || storage.vmclock_interrupt() != vmclock
    }) || platform.time().vmgenid().interrupt_line() != vmgenid
        || platform.time().vmclock().interrupt_line() != vmclock
    {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan);
    }
    Ok((serial, vmgenid, vmclock))
}

/// Proves one complete network-bearing MMIO product before live ownership.
pub fn prepare_hvf_snapshot_v2_network_mmio_platform_plan(
    platform: &HvfSnapshotV2PlatformState,
    product: HvfSnapshotV2NetworkPreparedProduct,
    process: HvfSnapshotV2NetworkMmioProcessConfig,
) -> Result<HvfSnapshotV2NetworkMmioPlatformPlan, PrepareHvfSnapshotV2NetworkPlatformPlanError> {
    prepare_network_mmio_platform_plan(
        platform,
        product,
        process,
        None,
        &mut SystemNetworkPlatformPlanReserve,
        &mut |_| false,
    )
    .map(|(plan, following)| {
        debug_assert!(following.is_none());
        plan
    })
}

/// Proves one MMIO product with stable owner-free cancellation checkpoints.
pub fn prepare_hvf_snapshot_v2_network_mmio_platform_plan_with_cancel<C>(
    platform: &HvfSnapshotV2PlatformState,
    product: HvfSnapshotV2NetworkPreparedProduct,
    process: HvfSnapshotV2NetworkMmioProcessConfig,
    mut is_cancelled: C,
) -> Result<HvfSnapshotV2NetworkMmioPlatformPlan, PrepareHvfSnapshotV2NetworkPlatformPlanError>
where
    C: FnMut(HvfSnapshotV2NetworkPlatformPlanStage) -> bool,
{
    prepare_network_mmio_platform_plan(
        platform,
        product,
        process,
        None,
        &mut SystemNetworkPlatformPlanReserve,
        &mut is_cancelled,
    )
    .map(|(plan, following)| {
        debug_assert!(following.is_none());
        plan
    })
}

pub(crate) fn prepare_network_mmio_platform_plan(
    platform: &HvfSnapshotV2PlatformState,
    product: HvfSnapshotV2NetworkPreparedProduct,
    process: HvfSnapshotV2NetworkMmioProcessConfig,
    following_input: Option<HvfSnapshotV2NetworkMmioFollowingEndpointInput>,
    reserve: &mut impl NetworkPlatformPlanReserve,
    is_cancelled: &mut impl FnMut(HvfSnapshotV2NetworkPlatformPlanStage) -> bool,
) -> Result<
    (
        HvfSnapshotV2NetworkMmioPlatformPlan,
        Option<HvfSnapshotV2NetworkMmioFollowingEndpointPlan>,
    ),
    PrepareHvfSnapshotV2NetworkPlatformPlanError,
> {
    check_cancel(is_cancelled, HvfSnapshotV2NetworkPlatformPlanStage::Start)?;
    if validate_product(platform, &product)? != SnapshotV2DeviceTransportKind::Mmio {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::TransportPolicy);
    }
    check_cancel(is_cancelled, HvfSnapshotV2NetworkPlatformPlanStage::Product)?;
    let mapping = prepare_mapping(platform, &product)?;
    if let Some(mapping) = &mapping
        && fixed_platform_range_conflict(platform, mapping.reservation().range(), None)?
    {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::FixedResource);
    }
    let balloon = product
        .balloon()
        .map(|balloon| {
            prepare_hvf_snapshot_v2_balloon_mmio_endpoint_plan(
                platform,
                balloon,
                process.balloon_layout(),
            )
            .map_err(|source| {
                PrepareHvfSnapshotV2NetworkPlatformPlanError::Balloon(Box::new(source))
            })
        })
        .transpose()?;
    let entropy = product
        .entropy()
        .map(|entropy| prepare_entropy_mmio_endpoint(platform, entropy, process.entropy_layout()))
        .transpose()?;
    let memory_hotplug = product
        .memory_hotplug_topology()
        .map(|topology| {
            prepare_memory_hotplug_mmio_endpoint(
                platform,
                topology,
                process.memory_hotplug_layout(),
            )
        })
        .transpose()?;
    let following = following_input
        .map(|input| prepare_mmio_following_endpoint(platform, input))
        .transpose()?;

    let interfaces = product.network().interfaces();
    let mut network = Vec::new();
    reserve.reserve(&mut network, interfaces.len())?;
    for (index, interface) in interfaces.iter().enumerate() {
        check_cancel(
            is_cancelled,
            HvfSnapshotV2NetworkPlatformPlanStage::Interface,
        )?;
        network.push(prepare_network_mmio_endpoint(
            platform,
            interface,
            process.network_layout(),
            index,
            reserve,
        )?);
    }
    check_cancel(
        is_cancelled,
        HvfSnapshotV2NetworkPlatformPlanStage::Components,
    )?;

    let mut inserted_endpoints = Vec::new();
    reserve.reserve(&mut inserted_endpoints, network.len())?;
    inserted_endpoints.extend(network.iter().map(|endpoint| {
        HvfSnapshotV2StorageMmioInsertedEndpoint::new(endpoint.region(), endpoint.interrupt_line())
    }));
    let following_count = usize::from(following.is_some())
        .checked_add(usize::from(entropy.is_some()))
        .and_then(|count| count.checked_add(usize::from(memory_hotplug.is_some())))
        .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    let mut following_interrupts = Vec::new();
    reserve.reserve(&mut following_interrupts, following_count)?;
    if let Some(following) = following {
        following_interrupts.push(following.placement.interrupt_line());
    }
    if let Some(entropy) = entropy {
        following_interrupts.push(entropy.interrupt_line());
    }
    if let Some(memory_hotplug) = memory_hotplug {
        following_interrupts.push(memory_hotplug.interrupt_line());
    }
    let prefix = balloon.map_or(HvfSnapshotV2StorageMmioPlatformPrefix::EMPTY, |balloon| {
        HvfSnapshotV2StorageMmioPlatformPrefix::one(balloon.region(), balloon.interrupt_line())
    });
    let storage = product
        .storage()
        .map(|storage| {
            prepare_hvf_snapshot_v2_storage_mmio_platform_plan_with_prefix_and_insertion(
                platform,
                storage,
                process.storage(),
                prefix,
                &inserted_endpoints,
                &following_interrupts,
            )
            .map_err(|source| {
                PrepareHvfSnapshotV2NetworkPlatformPlanError::StorageMmio(Box::new(source))
            })
        })
        .transpose()?;
    let (serial_interrupt, vmgenid_interrupt, vmclock_interrupt) =
        validate_mmio_interrupt_sequence(
            platform,
            balloon,
            storage.as_ref(),
            &network,
            following,
            entropy,
            memory_hotplug,
        )?;
    check_cancel(
        is_cancelled,
        HvfSnapshotV2NetworkPlatformPlanStage::Inventory,
    )?;
    validate_mmio_inventory(
        platform,
        &product,
        mapping.as_ref(),
        balloon,
        storage.as_ref(),
        &network,
        following,
        entropy,
        memory_hotplug,
        reserve,
    )?;
    check_cancel(
        is_cancelled,
        HvfSnapshotV2NetworkPlatformPlanStage::Completion,
    )?;
    Ok((
        HvfSnapshotV2NetworkMmioPlatformPlan {
            product,
            mapping,
            balloon,
            storage,
            network,
            entropy,
            memory_hotplug,
            serial_interrupt,
            vmgenid_interrupt,
            vmclock_interrupt,
        },
        following,
    ))
}

fn checked_queue_range_count(
    product: &HvfSnapshotV2NetworkPreparedProduct,
    network_ranges: impl Iterator<Item = Option<[GuestMemoryRange; 3]>>,
) -> Result<usize, PrepareHvfSnapshotV2NetworkPlatformPlanError> {
    let mut queue_count = product
        .balloon()
        .map_or(0, |balloon| balloon.queue_ranges().len());
    if let Some(storage) = product.storage() {
        queue_count = storage
            .block_bundle()
            .map_or(&[][..], |block| block.records())
            .iter()
            .filter(|record| record.queue_ranges().is_some())
            .count()
            .checked_add(
                storage
                    .pmem_records()
                    .iter()
                    .filter(|record| record.queue_ranges().is_some())
                    .count(),
            )
            .and_then(|storage_count| queue_count.checked_add(storage_count))
            .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    }
    queue_count = queue_count
        .checked_add(usize::from(
            product
                .entropy()
                .is_some_and(|entropy| entropy.queue_ranges().is_some()),
        ))
        .and_then(|count| {
            count.checked_add(usize::from(
                product
                    .memory_hotplug_topology()
                    .is_some_and(|topology| topology.queue_ranges().is_some()),
            ))
        })
        .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    for ranges in network_ranges {
        queue_count = queue_count
            .checked_add(usize::from(ranges.is_some()))
            .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    }
    queue_count
        .checked_mul(3)
        .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)
}

fn append_product_ranges(
    product: &HvfSnapshotV2NetworkPreparedProduct,
    queues: &mut Vec<GuestMemoryRange>,
    pmem: &mut Vec<GuestMemoryRange>,
) {
    if let Some(balloon) = product.balloon() {
        for ranges in balloon.queue_ranges() {
            queues.extend_from_slice(ranges);
        }
    }
    if let Some(storage) = product.storage() {
        for record in storage
            .block_bundle()
            .map_or(&[][..], |block| block.records())
        {
            if let Some(ranges) = record.queue_ranges() {
                queues.extend_from_slice(&ranges);
            }
        }
        for record in storage.pmem_records() {
            if let Some(ranges) = record.queue_ranges() {
                queues.extend_from_slice(&ranges);
            }
            pmem.push(record.prepared_device().guest_range());
        }
    }
    if let Some(ranges) = product
        .entropy()
        .and_then(SnapshotV2EntropyRestorePlan::queue_ranges)
    {
        queues.extend_from_slice(&ranges);
    }
    if let Some(ranges) = product
        .memory_hotplug_topology()
        .and_then(PreparedSnapshotV2MemoryHotplugTopology::queue_ranges)
    {
        queues.extend_from_slice(&ranges);
    }
}

fn base_contains(
    product: &HvfSnapshotV2NetworkPreparedProduct,
    mapping: Option<&HvfSnapshotV2MemoryHotplugMappingPlan>,
    range: GuestMemoryRange,
) -> bool {
    if let Some(mapping) = mapping {
        mapping.static_ranges().iter().any(|base| {
            range.start() >= base.start() && range.end_exclusive() <= base.end_exclusive()
        })
    } else {
        product.memory_binding().extents().iter().any(|extent| {
            let base = extent.range();
            range.start() >= base.start() && range.end_exclusive() <= base.end_exclusive()
        })
    }
}

fn overlaps_base(
    product: &HvfSnapshotV2NetworkPreparedProduct,
    mapping: Option<&HvfSnapshotV2MemoryHotplugMappingPlan>,
    range: GuestMemoryRange,
) -> bool {
    if let Some(mapping) = mapping {
        mapping
            .static_ranges()
            .iter()
            .any(|base| base.overlaps(range))
    } else {
        product
            .memory_binding()
            .extents()
            .iter()
            .any(|extent| extent.range().overlaps(range))
    }
}

fn validate_aggregate_ranges(
    product: &HvfSnapshotV2NetworkPreparedProduct,
    mapping: Option<&HvfSnapshotV2MemoryHotplugMappingPlan>,
    device_ranges: &[GuestMemoryRange],
    queues: &[GuestMemoryRange],
    pmem: &[GuestMemoryRange],
) -> bool {
    let aperture = mapping.map(|mapping| mapping.reservation().range());
    for (index, device) in device_ranges.iter().copied().enumerate() {
        if overlaps_base(product, mapping, device)
            || aperture.is_some_and(|aperture| aperture.overlaps(device))
            || device_ranges
                .iter()
                .copied()
                .take(index)
                .any(|previous| previous.overlaps(device))
            || queues.iter().copied().any(|queue| queue.overlaps(device))
            || pmem.iter().copied().any(|range| range.overlaps(device))
        {
            return false;
        }
    }
    for (index, queue) in queues.iter().copied().enumerate() {
        if !base_contains(product, mapping, queue)
            || aperture.is_some_and(|aperture| aperture.overlaps(queue))
            || queues
                .iter()
                .copied()
                .take(index)
                .any(|previous| previous.overlaps(queue))
            || pmem.iter().copied().any(|range| range.overlaps(queue))
        {
            return false;
        }
    }
    for (index, range) in pmem.iter().copied().enumerate() {
        if overlaps_base(product, mapping, range)
            || aperture.is_some_and(|aperture| aperture.overlaps(range))
            || pmem
                .iter()
                .copied()
                .take(index)
                .any(|previous| previous.overlaps(range))
        {
            return false;
        }
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn validate_mmio_inventory(
    _platform: &HvfSnapshotV2PlatformState,
    product: &HvfSnapshotV2NetworkPreparedProduct,
    mapping: Option<&HvfSnapshotV2MemoryHotplugMappingPlan>,
    balloon: Option<HvfSnapshotV2BalloonMmioEndpointPlan>,
    storage: Option<&HvfSnapshotV2StorageMmioPlatformPlan>,
    network: &[HvfSnapshotV2NetworkMmioEndpointPlan],
    following: Option<HvfSnapshotV2NetworkMmioFollowingEndpointPlan>,
    entropy: Option<HvfSnapshotV2NetworkAuxiliaryMmioEndpointPlan>,
    memory_hotplug: Option<HvfSnapshotV2NetworkAuxiliaryMmioEndpointPlan>,
    reserve: &mut impl NetworkPlatformPlanReserve,
) -> Result<(), PrepareHvfSnapshotV2NetworkPlatformPlanError> {
    let storage_region_count = storage
        .map(|storage| {
            storage
                .block_records()
                .len()
                .checked_add(storage.pmem_records().len())
                .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)
        })
        .transpose()?
        .unwrap_or(0);
    let region_count = usize::from(balloon.is_some())
        .checked_add(storage_region_count)
        .and_then(|count| count.checked_add(network.len()))
        .and_then(|count| count.checked_add(usize::from(following.is_some())))
        .and_then(|count| count.checked_add(usize::from(entropy.is_some())))
        .and_then(|count| count.checked_add(usize::from(memory_hotplug.is_some())))
        .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    let queue_count = checked_queue_range_count(
        product,
        network
            .iter()
            .flat_map(|endpoint| endpoint.queue_ranges().iter().copied())
            .chain(
                following
                    .into_iter()
                    .flat_map(|endpoint| endpoint.queue_ranges),
            ),
    )?;
    let pmem_count = product
        .storage()
        .map_or(0, |storage| storage.pmem_records().len());
    let mut regions = Vec::new();
    let mut device_ranges = Vec::new();
    let mut queues = Vec::new();
    let mut pmem = Vec::new();
    reserve.reserve(&mut regions, region_count)?;
    reserve.reserve(&mut device_ranges, region_count)?;
    reserve.reserve(&mut queues, queue_count)?;
    reserve.reserve(&mut pmem, pmem_count)?;
    if let Some(balloon) = balloon {
        regions.push(balloon.region());
    }
    if let Some(storage) = storage {
        regions.extend(storage.block_records().iter().map(|record| record.region()));
    }
    regions.extend(
        network
            .iter()
            .map(HvfSnapshotV2NetworkMmioEndpointPlan::region),
    );
    if let Some(storage) = storage {
        regions.extend(storage.pmem_records().iter().map(|record| record.region()));
    }
    if let Some(following) = following {
        regions.push(following.placement.region());
    }
    if let Some(entropy) = entropy {
        regions.push(entropy.region());
    }
    if let Some(memory_hotplug) = memory_hotplug {
        regions.push(memory_hotplug.region());
    }
    device_ranges.extend(regions.iter().map(|region| region.range()));
    append_product_ranges(product, &mut queues, &mut pmem);
    for ranges in network
        .iter()
        .flat_map(|endpoint| endpoint.queue_ranges().iter().copied().flatten())
    {
        queues.extend_from_slice(&ranges);
    }
    if let Some(following) = following {
        for ranges in following.queue_ranges.into_iter().flatten() {
            queues.extend_from_slice(&ranges);
        }
    }
    if regions.len() != region_count
        || device_ranges.len() != region_count
        || queues.len() != queue_count
        || pmem.len() != pmem_count
        || regions.iter().enumerate().any(|(index, region)| {
            regions
                .iter()
                .take(index)
                .any(|previous| previous.id() == region.id())
        })
        || !validate_aggregate_ranges(product, mapping, &device_ranges, &queues, &pmem)
    {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::RangeConflict);
    }
    Ok(())
}

fn expected_pci_msi_interrupt_count(
    product: &HvfSnapshotV2NetworkPreparedProduct,
    has_following_endpoint: bool,
) -> Result<u32, PrepareHvfSnapshotV2NetworkPlatformPlanError> {
    let balloon_queue_count = product
        .balloon()
        .map(|balloon| {
            let PreparedSnapshotV2BalloonTransport::Pci(transport) = balloon.transport() else {
                return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::TransportPolicy);
            };
            Ok(transport.device().queue_layout().queue_count())
        })
        .transpose()?;
    let configuration = if has_following_endpoint {
        pci_vsock_restore_gic_msi_configuration(
            balloon_queue_count,
            product.entropy().is_some(),
            product.memory_hotplug_topology().is_some(),
        )
    } else if product.memory_hotplug_topology().is_some() {
        pci_memory_hotplug_restore_gic_msi_configuration(
            balloon_queue_count,
            product.entropy().is_some(),
        )
    } else if let Some(queue_count) = balloon_queue_count {
        pci_balloon_restore_gic_msi_configuration(queue_count, product.entropy().is_some())
    } else if product.entropy().is_some() {
        pci_entropy_restore_gic_msi_configuration()
    } else {
        pci_root_restore_gic_msi_configuration()
    }
    .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    Ok(configuration.interrupt_count().get())
}

fn prepare_network_pci_endpoint(
    platform: &HvfSnapshotV2PlatformState,
    interface: &PreparedSnapshotV2NetworkRestoreInterface,
    address_plan: Arm64PciAddressPlan,
    msi: HvfGicMsiMetadata,
    expected_msi_interrupt_count: u32,
    slot: usize,
    reserve: &mut impl NetworkPlatformPlanReserve,
) -> Result<HvfSnapshotV2NetworkPciEndpointPlan, PrepareHvfSnapshotV2NetworkPlatformPlanError> {
    let placement = snapshot_v2_pci_endpoint_placement(address_plan, slot)
        .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    let SnapshotV2DeviceTransport::Pci(transport) = interface.portable().transport() else {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::TransportPolicy);
    };
    let queue_ranges = interface_queue_ranges(interface)?;
    let route_count = snapshot_v2_pci_endpoint_route_count(VIRTIO_NET_QUEUE_SIZES.len())
        .filter(|count| *count == VIRTIO_NET_QUEUE_COUNT + 1)
        .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    let queue_vectors: [u16; VIRTIO_NET_QUEUE_COUNT] = transport
        .msix()
        .queue_vectors()
        .try_into()
        .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    if msi.interrupt_range.count != expected_msi_interrupt_count
        || !matches!(
            transport.origin(),
            StorageDeviceOrigin::Startup | StorageDeviceOrigin::Runtime
        )
        || !valid_snapshot_v2_pci_record(transport, msi, VIRTIO_NET_QUEUE_COUNT)
    {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan);
    }
    if transport.sbdf() != placement.sbdf || transport.bar_range() != placement.bar_range {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::Placement);
    }
    for ranges in queue_ranges.iter().copied().flatten() {
        if queue_ranges_conflict_with_pci_platform(
            platform,
            Some(ranges),
            &platform.global().compatibility().gic_metadata(),
            address_plan,
        )
        .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?
        {
            return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::RangeConflict);
        }
    }
    Ok(HvfSnapshotV2NetworkPciEndpointPlan {
        source_index: interface.source_index(),
        resource_key: reserve.clone_key(interface.resource_key())?,
        queue_ranges,
        mmds_stack: interface.mmds_stack(),
        origin: transport.origin(),
        sbdf: placement.sbdf,
        bar_region_id: placement.bar_region_id,
        bar_range: placement.bar_range,
        route_count,
        queue_vectors,
        config_vector: transport.msix().config_vector(),
        msi_interrupt_count: expected_msi_interrupt_count,
    })
}

fn prepare_entropy_pci_endpoint(
    platform: &HvfSnapshotV2PlatformState,
    entropy: &SnapshotV2EntropyRestorePlan,
    address_plan: Arm64PciAddressPlan,
    msi: HvfGicMsiMetadata,
    expected_msi_interrupt_count: u32,
    slot: usize,
) -> Result<
    HvfSnapshotV2NetworkAuxiliaryPciEndpointPlan,
    PrepareHvfSnapshotV2NetworkPlatformPlanError,
> {
    let placement = snapshot_v2_pci_endpoint_placement(address_plan, slot)
        .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    let PreparedSnapshotV2EntropyTransport::Pci(transport) = entropy.transport() else {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::TransportPolicy);
    };
    let route_count = snapshot_v2_pci_endpoint_route_count(VIRTIO_RNG_QUEUE_SIZES.len())
        .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    if msi.interrupt_range.count != expected_msi_interrupt_count
        || transport.origin() != StorageDeviceOrigin::Startup
        || transport.retained().phase() != VirtioPciEndpointPhase::Active
    {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan);
    }
    if transport.sbdf() != placement.sbdf || transport.bar_range() != placement.bar_range {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::Placement);
    }
    if queue_ranges_conflict_with_pci_platform(
        platform,
        entropy.queue_ranges(),
        &platform.global().compatibility().gic_metadata(),
        address_plan,
    )
    .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?
    {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::RangeConflict);
    }
    Ok(HvfSnapshotV2NetworkAuxiliaryPciEndpointPlan {
        origin: transport.origin(),
        sbdf: placement.sbdf,
        bar_region_id: placement.bar_region_id,
        bar_range: placement.bar_range,
        route_count,
        msi_interrupt_count: expected_msi_interrupt_count,
    })
}

fn prepare_memory_hotplug_pci_endpoint(
    platform: &HvfSnapshotV2PlatformState,
    topology: &PreparedSnapshotV2MemoryHotplugTopology,
    address_plan: Arm64PciAddressPlan,
    msi: HvfGicMsiMetadata,
    expected_msi_interrupt_count: u32,
    slot: usize,
) -> Result<
    HvfSnapshotV2NetworkAuxiliaryPciEndpointPlan,
    PrepareHvfSnapshotV2NetworkPlatformPlanError,
> {
    let placement = snapshot_v2_pci_endpoint_placement(address_plan, slot)
        .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    let SnapshotV2DeviceTransport::Pci(transport) = topology.state().transport() else {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::TransportPolicy);
    };
    let route_count = snapshot_v2_pci_endpoint_route_count(VIRTIO_MEM_QUEUE_SIZES.len())
        .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    if msi.interrupt_range.count != expected_msi_interrupt_count
        || transport.origin() != StorageDeviceOrigin::Startup
        || transport.phase() != VirtioPciEndpointPhase::Active
    {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan);
    }
    if transport.sbdf() != placement.sbdf || transport.bar_range() != placement.bar_range {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::Placement);
    }
    if queue_ranges_conflict_with_pci_platform(
        platform,
        topology.queue_ranges(),
        &platform.global().compatibility().gic_metadata(),
        address_plan,
    )
    .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?
    {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::RangeConflict);
    }
    Ok(HvfSnapshotV2NetworkAuxiliaryPciEndpointPlan {
        origin: transport.origin(),
        sbdf: placement.sbdf,
        bar_region_id: placement.bar_region_id,
        bar_range: placement.bar_range,
        route_count,
        msi_interrupt_count: expected_msi_interrupt_count,
    })
}

fn prepare_pci_following_endpoint(
    platform: &HvfSnapshotV2PlatformState,
    input: &HvfSnapshotV2NetworkPciFollowingEndpointInput<'_>,
    address_plan: Arm64PciAddressPlan,
    msi: HvfGicMsiMetadata,
    expected_msi_interrupt_count: u32,
    slot: usize,
) -> Result<
    HvfSnapshotV2NetworkPciFollowingEndpointPlan,
    PrepareHvfSnapshotV2NetworkPlatformPlanError,
> {
    let placement = snapshot_v2_pci_endpoint_placement(address_plan, slot)
        .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    let route_count = snapshot_v2_pci_endpoint_route_count(FOLLOWING_ENDPOINT_QUEUE_COUNT)
        .filter(|count| *count == FOLLOWING_ENDPOINT_QUEUE_COUNT + 1)
        .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    let queue_vectors: [u16; FOLLOWING_ENDPOINT_QUEUE_COUNT] = input
        .msix
        .queue_vectors()
        .try_into()
        .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    if msi.interrupt_range.count != expected_msi_interrupt_count
        || input.origin != StorageDeviceOrigin::Startup
        || input.phase != VirtioPciEndpointPhase::Active
        || input.msix.vector_count() != route_count
    {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan);
    }
    if input.sbdf != placement.sbdf || input.bar_range != placement.bar_range {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::Placement);
    }
    for ranges in input.queue_ranges.iter().copied().flatten() {
        if queue_ranges_conflict_with_pci_platform(
            platform,
            Some(ranges),
            &platform.global().compatibility().gic_metadata(),
            address_plan,
        )
        .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?
        {
            return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::RangeConflict);
        }
    }
    Ok(HvfSnapshotV2NetworkPciFollowingEndpointPlan {
        placement: HvfSnapshotV2NetworkAuxiliaryPciEndpointPlan {
            origin: input.origin,
            sbdf: placement.sbdf,
            bar_region_id: placement.bar_region_id,
            bar_range: placement.bar_range,
            route_count,
            msi_interrupt_count: expected_msi_interrupt_count,
        },
        queue_ranges: input.queue_ranges,
        queue_vectors,
        config_vector: input.msix.config_vector(),
    })
}

fn validate_pci_fixed_interrupts(
    platform: &HvfSnapshotV2PlatformState,
    storage: Option<&HvfSnapshotV2StoragePciPlatformPlan>,
) -> Result<
    (GuestInterruptLine, GuestInterruptLine, GuestInterruptLine),
    PrepareHvfSnapshotV2NetworkPlatformPlanError,
> {
    let mut allocator = HvfGicInterruptLineAllocator::from_metadata(
        &platform.global().compatibility().gic_metadata(),
    )
    .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    let serial = allocator
        .allocate()
        .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    let vmgenid = allocator
        .allocate()
        .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    let vmclock = allocator
        .allocate()
        .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    if storage.is_some_and(|storage| {
        storage.serial_interrupt() != serial
            || storage.vmgenid_interrupt() != vmgenid
            || storage.vmclock_interrupt() != vmclock
    }) || platform.time().vmgenid().interrupt_line() != vmgenid
        || platform.time().vmclock().interrupt_line() != vmclock
    {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan);
    }
    Ok((serial, vmgenid, vmclock))
}

/// Proves one complete network-bearing PCI product before live ownership.
pub fn prepare_hvf_snapshot_v2_network_pci_platform_plan(
    platform: &HvfSnapshotV2PlatformState,
    product: HvfSnapshotV2NetworkPreparedProduct,
) -> Result<HvfSnapshotV2NetworkPciPlatformPlan, PrepareHvfSnapshotV2NetworkPlatformPlanError> {
    prepare_network_pci_platform_plan(
        platform,
        product,
        None,
        &mut SystemNetworkPlatformPlanReserve,
        &mut |_| false,
    )
    .map(|(plan, following)| {
        debug_assert!(following.is_none());
        plan
    })
}

/// Proves one PCI product with stable owner-free cancellation checkpoints.
pub fn prepare_hvf_snapshot_v2_network_pci_platform_plan_with_cancel<C>(
    platform: &HvfSnapshotV2PlatformState,
    product: HvfSnapshotV2NetworkPreparedProduct,
    mut is_cancelled: C,
) -> Result<HvfSnapshotV2NetworkPciPlatformPlan, PrepareHvfSnapshotV2NetworkPlatformPlanError>
where
    C: FnMut(HvfSnapshotV2NetworkPlatformPlanStage) -> bool,
{
    prepare_network_pci_platform_plan(
        platform,
        product,
        None,
        &mut SystemNetworkPlatformPlanReserve,
        &mut is_cancelled,
    )
    .map(|(plan, following)| {
        debug_assert!(following.is_none());
        plan
    })
}

pub(crate) fn prepare_network_pci_platform_plan(
    platform: &HvfSnapshotV2PlatformState,
    product: HvfSnapshotV2NetworkPreparedProduct,
    following_input: Option<HvfSnapshotV2NetworkPciFollowingEndpointInput<'_>>,
    reserve: &mut impl NetworkPlatformPlanReserve,
    is_cancelled: &mut impl FnMut(HvfSnapshotV2NetworkPlatformPlanStage) -> bool,
) -> Result<
    (
        HvfSnapshotV2NetworkPciPlatformPlan,
        Option<HvfSnapshotV2NetworkPciFollowingEndpointPlan>,
    ),
    PrepareHvfSnapshotV2NetworkPlatformPlanError,
> {
    check_cancel(is_cancelled, HvfSnapshotV2NetworkPlatformPlanStage::Start)?;
    if validate_product(platform, &product)? != SnapshotV2DeviceTransportKind::Pci {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::TransportPolicy);
    }
    check_cancel(is_cancelled, HvfSnapshotV2NetworkPlatformPlanStage::Product)?;
    let mapping = prepare_mapping(platform, &product)?;
    let address_plan = Arm64PciAddressPlan::firecracker_v1_16()
        .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    let host = Arm64FdtPciHost::from_address_plan(address_plan);
    let msi = platform
        .global()
        .compatibility()
        .gic_metadata()
        .msi
        .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::TransportPolicy)?;
    let expected_msi_interrupt_count =
        expected_pci_msi_interrupt_count(&product, following_input.is_some())?;
    if msi.interrupt_range.count != expected_msi_interrupt_count {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan);
    }
    if let Some(mapping) = &mapping
        && fixed_platform_range_conflict(
            platform,
            mapping.reservation().range(),
            Some(address_plan),
        )?
    {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::FixedResource);
    }

    let balloon = product
        .balloon()
        .map(|balloon| {
            prepare_hvf_snapshot_v2_balloon_pci_endpoint_plan(
                platform,
                balloon,
                address_plan,
                msi,
                expected_msi_interrupt_count,
            )
            .map_err(|source| {
                PrepareHvfSnapshotV2NetworkPlatformPlanError::Balloon(Box::new(source))
            })
        })
        .transpose()?;
    let balloon_count = usize::from(balloon.is_some());
    let block_count = product.storage().map_or(0, |storage| {
        storage
            .block_bundle()
            .map_or(&[][..], |block| block.records())
            .len()
    });
    let pmem_count = product
        .storage()
        .map_or(0, |storage| storage.pmem_records().len());
    let network_count = product.network().interfaces().len();
    let reserved_following = usize::from(following_input.is_some())
        .checked_add(usize::from(product.entropy().is_some()))
        .and_then(|count| {
            count.checked_add(usize::from(product.memory_hotplug_topology().is_some()))
        })
        .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;

    let storage = product
        .storage()
        .map(|storage| {
            prepare_hvf_snapshot_v2_storage_pci_platform_plan_with_prefix(
                platform,
                storage,
                HvfSnapshotV2StoragePciPlatformPrefix::exact_with_inserted_endpoints(
                    balloon_count,
                    network_count,
                    reserved_following,
                    expected_msi_interrupt_count,
                ),
            )
            .map_err(|source| {
                PrepareHvfSnapshotV2NetworkPlatformPlanError::StoragePci(Box::new(source))
            })
        })
        .transpose()?;
    if storage
        .as_ref()
        .is_some_and(|storage| storage.pci().host() != host || storage.pci().msi() != msi)
    {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan);
    }
    let network_start = balloon_count
        .checked_add(block_count)
        .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    let mut network = Vec::new();
    reserve.reserve(&mut network, network_count)?;
    for (index, interface) in product.network().interfaces().iter().enumerate() {
        check_cancel(
            is_cancelled,
            HvfSnapshotV2NetworkPlatformPlanStage::Interface,
        )?;
        let slot = network_start
            .checked_add(index)
            .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
        network.push(prepare_network_pci_endpoint(
            platform,
            interface,
            address_plan,
            msi,
            expected_msi_interrupt_count,
            slot,
            reserve,
        )?);
    }
    let following_slot = network_start
        .checked_add(network_count)
        .and_then(|slot| slot.checked_add(pmem_count))
        .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    let following = following_input
        .as_ref()
        .map(|input| {
            prepare_pci_following_endpoint(
                platform,
                input,
                address_plan,
                msi,
                expected_msi_interrupt_count,
                following_slot,
            )
        })
        .transpose()?;
    let entropy_slot = following_slot
        .checked_add(usize::from(following.is_some()))
        .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    let entropy = product
        .entropy()
        .map(|entropy| {
            prepare_entropy_pci_endpoint(
                platform,
                entropy,
                address_plan,
                msi,
                expected_msi_interrupt_count,
                entropy_slot,
            )
        })
        .transpose()?;
    let memory_hotplug_slot = entropy_slot
        .checked_add(usize::from(entropy.is_some()))
        .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    let memory_hotplug = product
        .memory_hotplug_topology()
        .map(|topology| {
            prepare_memory_hotplug_pci_endpoint(
                platform,
                topology,
                address_plan,
                msi,
                expected_msi_interrupt_count,
                memory_hotplug_slot,
            )
        })
        .transpose()?;
    let endpoint_count = memory_hotplug_slot
        .checked_add(usize::from(memory_hotplug.is_some()))
        .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    validate_pci_endpoint_capacity(endpoint_count)?;
    check_cancel(
        is_cancelled,
        HvfSnapshotV2NetworkPlatformPlanStage::Components,
    )?;

    let route_demand = balloon
        .map_or(0, HvfSnapshotV2BalloonPciEndpointPlan::route_count)
        .checked_add(
            storage
                .as_ref()
                .map_or(0, |storage| storage.pci().route_demand()),
        )
        .and_then(|count| {
            network_count
                .checked_mul(VIRTIO_NET_QUEUE_COUNT + 1)
                .and_then(|network| count.checked_add(network))
        })
        .and_then(|count| {
            count.checked_add(following.map_or(0, |endpoint| endpoint.placement.route_count()))
        })
        .and_then(|count| {
            count.checked_add(
                entropy.map_or(0, HvfSnapshotV2NetworkAuxiliaryPciEndpointPlan::route_count),
            )
        })
        .and_then(|count| {
            count.checked_add(
                memory_hotplug.map_or(0, HvfSnapshotV2NetworkAuxiliaryPciEndpointPlan::route_count),
            )
        })
        .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    if route_demand
        > usize::try_from(msi.interrupt_range.count)
            .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?
    {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::RouteConflict);
    }
    validate_active_pci_routes(
        &product,
        following_input.as_ref(),
        msi,
        route_demand,
        reserve,
    )?;
    let (serial_interrupt, vmgenid_interrupt, vmclock_interrupt) =
        validate_pci_fixed_interrupts(platform, storage.as_ref())?;
    check_cancel(
        is_cancelled,
        HvfSnapshotV2NetworkPlatformPlanStage::Inventory,
    )?;
    validate_pci_inventory(
        &product,
        mapping.as_ref(),
        balloon,
        storage.as_ref(),
        &network,
        following,
        entropy,
        memory_hotplug,
        endpoint_count,
        reserve,
    )?;
    check_cancel(
        is_cancelled,
        HvfSnapshotV2NetworkPlatformPlanStage::Completion,
    )?;
    Ok((
        HvfSnapshotV2NetworkPciPlatformPlan {
            product,
            mapping,
            balloon,
            storage,
            network,
            entropy,
            memory_hotplug,
            host,
            msi,
            endpoint_count,
            route_demand,
            serial_interrupt,
            vmgenid_interrupt,
            vmclock_interrupt,
        },
        following,
    ))
}

fn validate_active_pci_routes(
    product: &HvfSnapshotV2NetworkPreparedProduct,
    following: Option<&HvfSnapshotV2NetworkPciFollowingEndpointInput<'_>>,
    msi: HvfGicMsiMetadata,
    route_demand: usize,
    reserve: &mut impl NetworkPlatformPlanReserve,
) -> Result<(), PrepareHvfSnapshotV2NetworkPlatformPlanError> {
    let mut active_routes = Vec::new();
    reserve.reserve(&mut active_routes, route_demand)?;
    if let Some(balloon) = product.balloon() {
        let PreparedSnapshotV2BalloonTransport::Pci(transport) = balloon.transport() else {
            return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::TransportPolicy);
        };
        let queue_count = transport.device().queue_layout().queue_count();
        let route_count = snapshot_v2_pci_endpoint_route_count(queue_count)
            .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
        if !register_active_retained_pci_routes(
            transport.retained().msix_state(),
            msi,
            queue_count,
            route_count,
            &mut active_routes,
        ) {
            return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::RouteConflict);
        }
    }
    if let Some(storage) = product.storage() {
        for record in storage
            .block_bundle()
            .map_or(&[][..], |block| block.records())
        {
            let SnapshotV2DeviceTransport::Pci(transport) = record.transport() else {
                return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::TransportPolicy);
            };
            if !register_active_pci_routes(transport, &mut active_routes) {
                return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::RouteConflict);
            }
        }
        for record in storage.pmem_records() {
            let SnapshotV2DeviceTransport::Pci(transport) = record.transport() else {
                return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::TransportPolicy);
            };
            if !register_active_pci_routes(transport, &mut active_routes) {
                return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::RouteConflict);
            }
        }
    }
    for interface in product.network().interfaces() {
        let SnapshotV2DeviceTransport::Pci(transport) = interface.portable().transport() else {
            return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::TransportPolicy);
        };
        if !register_active_snapshot_v2_pci_routes(
            transport,
            msi,
            VIRTIO_NET_QUEUE_COUNT,
            VIRTIO_NET_QUEUE_COUNT + 1,
            &mut active_routes,
        ) {
            return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::RouteConflict);
        }
    }
    if let Some(following) = following
        && !register_active_retained_pci_routes(
            following.msix,
            msi,
            FOLLOWING_ENDPOINT_QUEUE_COUNT,
            FOLLOWING_ENDPOINT_QUEUE_COUNT + 1,
            &mut active_routes,
        )
    {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::RouteConflict);
    }
    if let Some(entropy) = product.entropy() {
        let PreparedSnapshotV2EntropyTransport::Pci(transport) = entropy.transport() else {
            return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::TransportPolicy);
        };
        let route_count = snapshot_v2_pci_endpoint_route_count(VIRTIO_RNG_QUEUE_SIZES.len())
            .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
        if !register_active_retained_pci_routes(
            transport.retained().msix_state(),
            msi,
            VIRTIO_RNG_QUEUE_SIZES.len(),
            route_count,
            &mut active_routes,
        ) {
            return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::RouteConflict);
        }
    }
    if let Some(topology) = product.memory_hotplug_topology() {
        let SnapshotV2DeviceTransport::Pci(transport) = topology.state().transport() else {
            return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::TransportPolicy);
        };
        let route_count = snapshot_v2_pci_endpoint_route_count(VIRTIO_MEM_QUEUE_SIZES.len())
            .ok_or(PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
        if !register_active_snapshot_v2_pci_routes(
            transport,
            msi,
            VIRTIO_MEM_QUEUE_SIZES.len(),
            route_count,
            &mut active_routes,
        ) {
            return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::RouteConflict);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_pci_inventory(
    product: &HvfSnapshotV2NetworkPreparedProduct,
    mapping: Option<&HvfSnapshotV2MemoryHotplugMappingPlan>,
    balloon: Option<HvfSnapshotV2BalloonPciEndpointPlan>,
    storage: Option<&HvfSnapshotV2StoragePciPlatformPlan>,
    network: &[HvfSnapshotV2NetworkPciEndpointPlan],
    following: Option<HvfSnapshotV2NetworkPciFollowingEndpointPlan>,
    entropy: Option<HvfSnapshotV2NetworkAuxiliaryPciEndpointPlan>,
    memory_hotplug: Option<HvfSnapshotV2NetworkAuxiliaryPciEndpointPlan>,
    endpoint_count: usize,
    reserve: &mut impl NetworkPlatformPlanReserve,
) -> Result<(), PrepareHvfSnapshotV2NetworkPlatformPlanError> {
    let queue_count = checked_queue_range_count(
        product,
        network
            .iter()
            .flat_map(|endpoint| endpoint.queue_ranges().iter().copied())
            .chain(
                following
                    .into_iter()
                    .flat_map(|endpoint| endpoint.queue_ranges),
            ),
    )?;
    let pmem_count = product
        .storage()
        .map_or(0, |storage| storage.pmem_records().len());
    let mut bar_ids = Vec::new();
    let mut bars = Vec::new();
    let mut queues = Vec::new();
    let mut pmem = Vec::new();
    reserve.reserve(&mut bar_ids, endpoint_count)?;
    reserve.reserve(&mut bars, endpoint_count)?;
    reserve.reserve(&mut queues, queue_count)?;
    reserve.reserve(&mut pmem, pmem_count)?;
    if let Some(balloon) = balloon {
        bar_ids.push(balloon.bar_region_id());
        bars.push(balloon.bar_range());
    }
    if let Some(storage) = storage {
        bar_ids.extend(
            storage
                .pci()
                .block_records()
                .iter()
                .chain(storage.pci().pmem_records())
                .map(|record| record.bar_region_id()),
        );
        bars.extend(
            storage
                .pci()
                .block_records()
                .iter()
                .chain(storage.pci().pmem_records())
                .map(|record| record.bar_range()),
        );
    }
    bar_ids.extend(
        network
            .iter()
            .map(HvfSnapshotV2NetworkPciEndpointPlan::bar_region_id),
    );
    bars.extend(
        network
            .iter()
            .map(HvfSnapshotV2NetworkPciEndpointPlan::bar_range),
    );
    if let Some(following) = following {
        bar_ids.push(following.placement.bar_region_id());
        bars.push(following.placement.bar_range());
    }
    if let Some(entropy) = entropy {
        bar_ids.push(entropy.bar_region_id());
        bars.push(entropy.bar_range());
    }
    if let Some(memory_hotplug) = memory_hotplug {
        bar_ids.push(memory_hotplug.bar_region_id());
        bars.push(memory_hotplug.bar_range());
    }
    append_product_ranges(product, &mut queues, &mut pmem);
    for ranges in network
        .iter()
        .flat_map(|endpoint| endpoint.queue_ranges().iter().copied().flatten())
    {
        queues.extend_from_slice(&ranges);
    }
    if let Some(following) = following {
        for ranges in following.queue_ranges.into_iter().flatten() {
            queues.extend_from_slice(&ranges);
        }
    }
    if bar_ids.len() != endpoint_count
        || bars.len() != endpoint_count
        || queues.len() != queue_count
        || pmem.len() != pmem_count
        || bar_ids
            .iter()
            .enumerate()
            .any(|(index, id)| bar_ids.iter().take(index).any(|previous| previous == id))
        || !validate_aggregate_ranges(product, mapping, &bars, &queues, &pmem)
    {
        return Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::RangeConflict);
    }
    Ok(())
}

fn fixed_platform_range_conflict(
    platform: &HvfSnapshotV2PlatformState,
    range: GuestMemoryRange,
    pci: Option<Arm64PciAddressPlan>,
) -> Result<bool, PrepareHvfSnapshotV2NetworkPlatformPlanError> {
    let fdt = platform.machine().fdt();
    let fdt_range = GuestMemoryRange::new(fdt.address(), u64::from(fdt.size()))
        .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    let serial = GuestMemoryRange::new(PROCESS_SERIAL_MMIO_BASE, SERIAL_MMIO_DEVICE_WINDOW_SIZE)
        .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    let rtc = GuestMemoryRange::new(PROCESS_RTC_MMIO_BASE, RTC_MMIO_DEVICE_WINDOW_SIZE)
        .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    let gic = platform.global().compatibility().gic_metadata();
    let distributor = gic_region_range(gic.distributor)?;
    let redistributor = gic_region_range(gic.redistributor.region)?;
    if [
        fdt_range,
        platform.time().vmgenid().range(),
        platform.time().vmclock().range(),
        serial,
        rtc,
        distributor,
        redistributor,
    ]
    .into_iter()
    .any(|fixed| range.overlaps(fixed))
    {
        return Ok(true);
    }
    let pvtime_size = u64::try_from(ARM64_PVTIME_STRUCTURE_SIZE)
        .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
    for record in platform.time().pvtime_vcpus() {
        let pvtime = GuestMemoryRange::new(record.record_ipa(), pvtime_size)
            .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)?;
        if range.overlaps(pvtime) {
            return Ok(true);
        }
    }
    if let Some(msi) = gic.msi
        && range.overlaps(gic_region_range(msi.region)?)
    {
        return Ok(true);
    }
    if let Some(pci) = pci
        && [pci.ecam_reservation(), pci.bar32(), pci.bar64()]
            .into_iter()
            .any(|fixed| range.overlaps(fixed))
    {
        return Ok(true);
    }
    Ok(false)
}

fn gic_region_range(
    region: crate::gic::HvfGicRegion,
) -> Result<GuestMemoryRange, PrepareHvfSnapshotV2NetworkPlatformPlanError> {
    GuestMemoryRange::new(GuestAddress::new(region.base), region.size)
        .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan)
}

fn validate_pci_endpoint_capacity(
    endpoint_count: usize,
) -> Result<(), PrepareHvfSnapshotV2NetworkPlatformPlanError> {
    if endpoint_count > PCI_ENDPOINT_SLOT_COUNT {
        Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::PciCapacity {
            count: endpoint_count,
            maximum: PCI_ENDPOINT_SLOT_COUNT,
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod fixture_identity_tests {
    use bangbang_runtime::balloon::VIRTIO_BALLOON_MAX_QUEUE_COUNT;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::fdt::ARM64_GICV2M_MSI_SET_SPI_NSR_OFFSET;
    use bangbang_runtime::network::{GuestMacAddress, NetworkDeviceProfile};
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::snapshot::SnapshotNetworkOverride;
    use bangbang_runtime::snapshot_device_v2::{
        SnapshotV2DeviceTransport, SnapshotV2DeviceTransportKind, SnapshotV2MmioDeviceState,
    };
    use bangbang_runtime::snapshot_network_v2_11::{
        NATIVE_V2_NETWORK_INTERFACE_RECORD_HEADER_BYTES,
        NATIVE_V2_NETWORK_INTERFACE_SECTION_ENTRY_BYTES,
        NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION, NATIVE_V2_NETWORK_STATE_HEADER_BYTES,
        SnapshotV2MmdsState, SnapshotV2NetworkBackendClass, SnapshotV2NetworkInterfaceState,
        SnapshotV2NetworkInterfaceStateParts, SnapshotV2NetworkState,
    };
    use bangbang_runtime::snapshot_restore::{
        SnapshotRestorePublicId, SnapshotRestoreResourceClass, SnapshotRestoreResourceKey,
    };
    use bangbang_runtime::snapshot_vsock_restore_v2_12::PreparedSnapshotV2VsockRestoreTopology;
    use bangbang_runtime::snapshot_vsock_v2_12::{
        NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION, SnapshotV2VsockState,
        SnapshotV2VsockStateParts,
    };
    use bangbang_runtime::vsock::{VIRTIO_VSOCK_QUEUE_COUNT, VsockConfigInput, VsockMmioLayout};

    use super::*;
    use crate::gic::{HvfGicInterruptRange, HvfGicRegion};
    use crate::snapshot_v2::{
        HvfSnapshotV2MachineState, HvfSnapshotV2PlatformState, tests::product_storage_fixture,
    };
    use crate::snapshot_v2_memory_hotplug_platform::tests::{
        MaterializedFixture, TestImage, balloon_mmio_plan, balloon_pci_plan, entropy_mmio_plan,
        entropy_pci_plan, materialized_fixture, memory_hotplug_mmio_state,
        memory_hotplug_pci_state, mmio_platform as memory_fixture_mmio_platform,
        pci_platform as memory_fixture_pci_platform, prepared_storage_bundle,
        storage_mmio_graph_with_gap, storage_pci_graph_with_gap,
    };
    use crate::snapshot_v2_multi_block_platform::tests::{
        product_mmio_platform, product_pci_platform,
    };
    use crate::snapshot_v2_vsock_platform::{
        HvfSnapshotV2VsockMmioProcessConfig, HvfSnapshotV2VsockPlatformPlanStage,
        HvfSnapshotV2VsockPreparedEndpoint, HvfSnapshotV2VsockPreparedMemory,
        HvfSnapshotV2VsockPreparedProduct, HvfSnapshotV2VsockPreparedProductParts,
        HvfSnapshotV2VsockProcessResourceIdentity, HvfSnapshotV2VsockProductKind,
        PrepareHvfSnapshotV2VsockPlatformPlanError,
        prepare_hvf_snapshot_v2_vsock_mmio_platform_plan,
        prepare_hvf_snapshot_v2_vsock_mmio_platform_plan_with_cancel,
        prepare_hvf_snapshot_v2_vsock_pci_platform_plan, prepare_vsock_mmio_platform_plan,
        prepare_vsock_pci_platform_plan,
    };
    use crate::startup::pci_vsock_restore_gic_msi_configuration;

    const ACTIVE_PCI_MMDS_HEX: &str =
        include_str!("../../runtime/src/snapshot_network_v2_11/fixtures/active-pci-mmds.hex");
    const INACTIVE_MMIO_HEX: &str =
        include_str!("../../runtime/src/snapshot_network_v2_11/fixtures/inactive-mmio.hex");
    const NETWORK_MMIO_BASE: GuestAddress = GuestAddress::new(0xd003_0000);
    const NETWORK_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(103);
    const NETWORK_QUEUE_BASE: u64 = 0x8000_0000;
    const NETWORK_INTERFACE_QUEUE_STRIDE: u64 = 0x2_0000;
    const NETWORK_QUEUE_STRIDE: u64 = 0x1_0000;
    const DRIVER_RING_OFFSET: u64 = 0x2000;
    const DEVICE_RING_OFFSET: u64 = 0x4000;
    const VSOCK_QUEUE_BASE: u64 = 0x8030_0000;
    const VSOCK_MMIO_BASE: GuestAddress = GuestAddress::new(0x7000_0000);
    const VSOCK_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(2000);
    const VSOCK_SECTION_DIRECTORY_OFFSET: usize = 64;
    const VSOCK_SECTION_ENTRY_BYTES: usize = 32;
    const VSOCK_LOCAL_QUEUE_CURSORS_OFFSET: usize = 20;
    const VSOCK_LOCAL_QUEUE_CURSORS_BYTES: usize = VIRTIO_VSOCK_QUEUE_COUNT * 4;
    const VSOCK_COMMON_FIXED_BYTES: usize = 32;
    const VSOCK_COMMON_QUEUE_BYTES: usize = 32;
    const VSOCK_COMMON_INTERRUPT_INTENT_BYTES: usize = 4;
    const PCI_FIXED_BYTES: usize = 72;
    const PCI_WRITABLE_ENTRY_BYTES: usize = 4;
    const PCI_BAR_PROBE_ENTRY_BYTES: usize = 4;
    const PCI_MSIX_ENTRY_BYTES: usize = 16;
    const PCI_MSI_REGION_BASE: u64 = 0x0800_0000;
    const PCI_MSI_REGION_SIZE: u64 = 0x1_0000;

    #[derive(Clone, Copy)]
    enum MmdsSelection {
        None,
        Subset,
        All,
    }

    fn fixture_bytes(fixture: &str) -> Vec<u8> {
        let compact = fixture.split_ascii_whitespace().collect::<String>();
        compact
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                u8::from_str_radix(
                    std::str::from_utf8(pair).expect("fixture should be ASCII"),
                    16,
                )
                .expect("fixture should contain hexadecimal bytes")
            })
            .collect()
    }

    fn read_u16(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes(
            bytes[offset..offset + 2]
                .try_into()
                .expect("wire u16 should fit"),
        )
    }

    fn read_u64(bytes: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(
            bytes[offset..offset + 8]
                .try_into()
                .expect("wire u64 should fit"),
        )
    }

    fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn record_section_offset(bytes: &[u8], section_index: usize) -> usize {
        let record_entry = NATIVE_V2_NETWORK_STATE_HEADER_BYTES;
        let record_offset = usize::try_from(read_u64(bytes, record_entry + 8))
            .expect("network record offset should fit");
        let section_entry = record_offset
            + NATIVE_V2_NETWORK_INTERFACE_RECORD_HEADER_BYTES
            + section_index * NATIVE_V2_NETWORK_INTERFACE_SECTION_ENTRY_BYTES;
        record_offset
            + usize::try_from(read_u64(bytes, section_entry + 8))
                .expect("network section offset should fit")
    }

    fn relocate_queues(bytes: &mut [u8], common_offset: usize, base: u64) {
        let queue_count = usize::from(read_u16(bytes, common_offset + 26));
        for index in 0..queue_count {
            let queue_offset = common_offset + 32 + index * 32;
            if read_u16(bytes, queue_offset + 2) == 0 {
                continue;
            }
            let queue_base = base
                .checked_add(
                    u64::try_from(index)
                        .expect("queue index should fit")
                        .checked_mul(NETWORK_QUEUE_STRIDE)
                        .expect("queue offset should fit"),
                )
                .expect("queue base should fit");
            write_u64(bytes, queue_offset + 8, queue_base);
            write_u64(bytes, queue_offset + 16, queue_base + DRIVER_RING_OFFSET);
            write_u64(bytes, queue_offset + 24, queue_base + DEVICE_RING_OFFSET);
        }
    }

    fn relocate_pci(
        bytes: &mut [u8],
        transport_offset: usize,
        slot: usize,
        msi: HvfGicMsiMetadata,
        route_offset: u32,
    ) {
        let address_plan =
            Arm64PciAddressPlan::firecracker_v1_16().expect("address plan should validate");
        let placement = snapshot_v2_pci_endpoint_placement(address_plan, slot)
            .expect("network endpoint placement should validate");
        bytes[transport_offset + 11] = placement.sbdf.device();
        write_u64(
            bytes,
            transport_offset + 16,
            placement.bar_range.start().raw_value(),
        );

        let writable_count = usize::from(read_u16(bytes, transport_offset + 42));
        let probe_count = usize::from(read_u16(bytes, transport_offset + 44));
        let entry_count = usize::from(read_u16(bytes, transport_offset + 46));
        let mut entry_offset = transport_offset
            + PCI_FIXED_BYTES
            + writable_count * PCI_WRITABLE_ENTRY_BYTES
            + probe_count * PCI_BAR_PROBE_ENTRY_BYTES;
        let message_address = msi
            .region
            .base
            .checked_add(ARM64_GICV2M_MSI_SET_SPI_NSR_OFFSET)
            .expect("MSI message address should fit");
        let address_low =
            u32::try_from(message_address & u64::from(u32::MAX)).expect("low address should fit");
        let address_high = u32::try_from(message_address >> 32).expect("high address should fit");
        for index in 0..entry_count {
            let message_data = msi
                .interrupt_range
                .base
                .checked_add(route_offset)
                .and_then(|base| {
                    base.checked_add(u32::try_from(index).expect("route index should fit"))
                })
                .expect("message data should fit");
            write_u32(bytes, entry_offset, address_low);
            write_u32(bytes, entry_offset + 4, address_high);
            write_u32(bytes, entry_offset + 8, message_data);
            entry_offset += PCI_MSIX_ENTRY_BYTES;
        }
    }

    fn vsock_section_offset(bytes: &[u8], section_index: usize) -> usize {
        let entry = VSOCK_SECTION_DIRECTORY_OFFSET + section_index * VSOCK_SECTION_ENTRY_BYTES;
        usize::try_from(read_u64(bytes, entry + 8)).expect("vsock section offset should fit")
    }

    fn decode_vsock(bytes: &[u8]) -> SnapshotV2VsockState {
        SnapshotV2VsockState::decode(NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION, bytes)
            .expect("relocated vsock fixture should decode")
    }

    fn zero_active_vsock_queue_cursors(bytes: &mut [u8]) {
        let local_offset = vsock_section_offset(bytes, 0);
        bytes[local_offset + VSOCK_LOCAL_QUEUE_CURSORS_OFFSET
            ..local_offset + VSOCK_LOCAL_QUEUE_CURSORS_OFFSET + VSOCK_LOCAL_QUEUE_CURSORS_BYTES]
            .fill(0);
    }

    fn normalize_active_vsock_common_for_mmio(bytes: &mut Vec<u8>) {
        let common_entry = VSOCK_SECTION_DIRECTORY_OFFSET + VSOCK_SECTION_ENTRY_BYTES;
        let transport_entry = VSOCK_SECTION_DIRECTORY_OFFSET + 2 * VSOCK_SECTION_ENTRY_BYTES;
        let common_offset = vsock_section_offset(bytes, 1);
        let transport_offset = vsock_section_offset(bytes, 2);
        let queue_count = usize::from(read_u16(bytes, common_offset + 26));
        let notification_count = usize::from(read_u16(bytes, common_offset + 28));
        let intent_count = usize::from(read_u16(bytes, common_offset + 30));
        assert_eq!(queue_count, VIRTIO_VSOCK_QUEUE_COUNT);
        assert_eq!(notification_count, VIRTIO_VSOCK_QUEUE_COUNT);
        assert_eq!(intent_count, VIRTIO_VSOCK_QUEUE_COUNT + 1);

        let intents_offset = common_offset
            + VSOCK_COMMON_FIXED_BYTES
            + queue_count * VSOCK_COMMON_QUEUE_BYTES
            + notification_count * 2;
        let configuration_intent_offset =
            intents_offset + VIRTIO_VSOCK_QUEUE_COUNT * VSOCK_COMMON_INTERRUPT_INTENT_BYTES;
        let removed_start = intents_offset + VSOCK_COMMON_INTERRUPT_INTENT_BYTES;
        let removed_end = configuration_intent_offset;
        bytes.drain(removed_start..removed_end);
        write_u16(bytes, common_offset + 30, 2);

        let removed_bytes = u64::try_from(removed_end - removed_start)
            .expect("removed common intent bytes should fit");
        let total_bytes = read_u64(bytes, 24)
            .checked_sub(removed_bytes)
            .expect("shortened vsock payload should fit");
        let common_bytes = read_u64(bytes, common_entry + 16)
            .checked_sub(removed_bytes)
            .expect("shortened common section should fit");
        let transport_offset = u64::try_from(transport_offset)
            .expect("transport offset should fit")
            .checked_sub(removed_bytes)
            .expect("shortened transport offset should fit");
        write_u64(bytes, 24, total_bytes);
        write_u64(bytes, common_entry + 16, common_bytes);
        write_u64(bytes, transport_entry + 8, transport_offset);
    }

    fn active_mmio_vsock_state(interrupt: u32) -> SnapshotV2VsockState {
        let mut bytes = fixture_bytes(include_str!(
            "../../runtime/src/snapshot_vsock_v2_12/fixtures/active-pci.hex"
        ));
        zero_active_vsock_queue_cursors(&mut bytes);
        let common_offset = vsock_section_offset(&bytes, 1);
        relocate_queues(&mut bytes, common_offset, VSOCK_QUEUE_BASE);
        normalize_active_vsock_common_for_mmio(&mut bytes);
        let source = decode_vsock(&bytes);
        let SnapshotV2VsockStateParts {
            guest_cid,
            backend_selector,
            host_local_port_cursor,
            active_queues,
            virtio,
            transport,
        } = source.into_parts();
        let SnapshotV2DeviceTransport::Pci(pci) = transport else {
            panic!("active vsock source should be PCI");
        };
        let region = MmioRegion::new(
            VSOCK_MMIO_REGION_ID,
            VSOCK_MMIO_BASE,
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
        )
        .expect("vsock MMIO region should validate");
        SnapshotV2VsockState::try_from_parts(SnapshotV2VsockStateParts {
            guest_cid,
            backend_selector,
            host_local_port_cursor,
            active_queues,
            virtio,
            transport: SnapshotV2DeviceTransport::Mmio(SnapshotV2MmioDeviceState::from_parts(
                pci.device_feature_select(),
                pci.driver_feature_select(),
                u32::from(pci.queue_select()),
                region,
                GuestInterruptLine::new(interrupt).expect("vsock interrupt should validate"),
            )),
        })
        .expect("active MMIO vsock state should validate")
    }

    fn inactive_mmio_vsock_state(interrupt: u32) -> SnapshotV2VsockState {
        let source = decode_vsock(&fixture_bytes(include_str!(
            "../../runtime/src/snapshot_vsock_v2_12/fixtures/inactive-mmio.hex"
        )));
        let SnapshotV2VsockStateParts {
            guest_cid,
            backend_selector,
            host_local_port_cursor,
            active_queues,
            virtio,
            transport,
        } = source.into_parts();
        let SnapshotV2DeviceTransport::Mmio(mmio) = transport else {
            panic!("inactive vsock source should be MMIO");
        };
        let region = MmioRegion::new(
            VSOCK_MMIO_REGION_ID,
            VSOCK_MMIO_BASE,
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
        )
        .expect("vsock MMIO region should validate");
        SnapshotV2VsockState::try_from_parts(SnapshotV2VsockStateParts {
            guest_cid,
            backend_selector,
            host_local_port_cursor,
            active_queues,
            virtio,
            transport: SnapshotV2DeviceTransport::Mmio(SnapshotV2MmioDeviceState::from_parts(
                mmio.device_feature_select(),
                mmio.driver_feature_select(),
                mmio.queue_select(),
                region,
                GuestInterruptLine::new(interrupt).expect("vsock interrupt should validate"),
            )),
        })
        .expect("inactive MMIO vsock state should validate")
    }

    fn active_pci_vsock_state(
        slot: usize,
        msi: HvfGicMsiMetadata,
        route_offset: u32,
    ) -> SnapshotV2VsockState {
        let mut bytes = fixture_bytes(include_str!(
            "../../runtime/src/snapshot_vsock_v2_12/fixtures/active-pci.hex"
        ));
        zero_active_vsock_queue_cursors(&mut bytes);
        let common_offset = vsock_section_offset(&bytes, 1);
        relocate_queues(&mut bytes, common_offset, VSOCK_QUEUE_BASE);
        let transport_offset = vsock_section_offset(&bytes, 2);
        relocate_pci(&mut bytes, transport_offset, slot, msi, route_offset);
        decode_vsock(&bytes)
    }

    fn prepared_vsock_endpoint(
        state: SnapshotV2VsockState,
        memory: &GuestMemory,
    ) -> (
        HvfSnapshotV2VsockPreparedEndpoint,
        SnapshotRestoreResourceKey,
    ) {
        let transport = state.transport().kind();
        let topology =
            PreparedSnapshotV2VsockRestoreTopology::prepare(state, None, transport, memory)
                .expect("vsock topology should prepare");
        let (request, state) = topology.into_parts();
        let (resource_key, _selectors, config, _overridden) = request.into_parts();
        (
            HvfSnapshotV2VsockPreparedEndpoint::new(state, config),
            resource_key,
        )
    }

    fn exact_vsock_binding_keys(
        storage: Option<&bangbang_runtime::snapshot_device_v2_6::SnapshotV2StorageDeviceGraph>,
        network: &PreparedSnapshotV2NetworkRestoreTopology,
        vsock_key: Option<SnapshotRestoreResourceKey>,
    ) -> Vec<SnapshotRestoreResourceKey> {
        let mut keys = Vec::new();
        if let Some(storage) = storage {
            keys.extend(storage.block_records().iter().map(|record| {
                SnapshotRestoreResourceKey::new(
                    record.key(),
                    SnapshotRestorePublicId::try_from(record.config().drive_id())
                        .expect("block public ID should validate"),
                    SnapshotRestoreResourceClass::BlockBacking,
                )
            }));
            keys.extend(storage.pmem_records().iter().map(|record| {
                SnapshotRestoreResourceKey::new(
                    record.key(),
                    SnapshotRestorePublicId::try_from(record.config().pmem_id())
                        .expect("pmem public ID should validate"),
                    SnapshotRestoreResourceClass::PmemBacking,
                )
            }));
        }
        keys.extend(network.interfaces().iter().map(|interface| {
            interface
                .resource_key()
                .try_clone()
                .expect("network resource key should copy")
        }));
        keys.extend(vsock_key);
        keys.sort_unstable();
        keys
    }

    fn vsock_mmio_process_config() -> HvfSnapshotV2VsockMmioProcessConfig {
        HvfSnapshotV2VsockMmioProcessConfig::new(
            mmio_process_config(),
            VsockMmioLayout::new(VSOCK_MMIO_BASE, VSOCK_MMIO_REGION_ID),
        )
    }

    fn vsock_kind(mask: u8) -> HvfSnapshotV2VsockProductKind {
        HvfSnapshotV2VsockProductKind::from_presence(
            mask & 1 != 0,
            mask & 2 != 0,
            mask & 4 != 0,
            mask & 8 != 0,
            mask & 16 != 0,
            mask & 32 != 0,
        )
    }

    fn mmio_vsock_only_parts(
        active: bool,
        interrupt_offset: u32,
    ) -> (
        HvfSnapshotV2PlatformState,
        HvfSnapshotV2VsockPreparedProductParts,
        HvfSnapshotV2VsockMmioProcessConfig,
        TestImage,
    ) {
        let MaterializedFixture {
            platform,
            topology,
            memory,
            _image,
        } = materialized_fixture(memory_hotplug_mmio_state(33));
        let platform = memory_fixture_mmio_platform(platform, topology.state(), 1);
        let first_interrupt = platform
            .global()
            .compatibility()
            .gic_metadata()
            .spi_interrupt_range
            .base;
        let interrupt = first_interrupt
            .checked_add(interrupt_offset)
            .expect("vsock interrupt should fit");
        let state = if active {
            active_mmio_vsock_state(interrupt)
        } else {
            inactive_mmio_vsock_state(interrupt)
        };
        let (vsock, key) = prepared_vsock_endpoint(state, &memory);
        let network =
            PreparedSnapshotV2NetworkRestoreTopology::empty(SnapshotV2DeviceTransportKind::Mmio);
        (
            platform.clone(),
            HvfSnapshotV2VsockPreparedProductParts {
                kind: vsock_kind(0b100000),
                memory: HvfSnapshotV2VsockPreparedMemory::Static(platform.memory().clone()),
                storage: None,
                entropy: None,
                balloon: None,
                network,
                vsock: Some(vsock),
                serial_resource_present: false,
                binding_keys: vec![key],
            },
            vsock_mmio_process_config(),
            _image,
        )
    }

    fn mmio_network_vsock_parts(
        interface_count: usize,
    ) -> (
        HvfSnapshotV2PlatformState,
        HvfSnapshotV2VsockPreparedProductParts,
        HvfSnapshotV2VsockMmioProcessConfig,
        SnapshotV2NetworkState,
        TestImage,
    ) {
        let device_count = interface_count
            .checked_add(1)
            .expect("network and vsock device count should fit");
        let MaterializedFixture {
            platform,
            topology,
            memory,
            _image,
        } = materialized_fixture(memory_hotplug_mmio_state(
            32_u32
                .checked_add(u32::try_from(device_count).expect("device count should fit"))
                .expect("following interrupt should fit"),
        ));
        let platform = memory_fixture_mmio_platform(platform, topology.state(), device_count);
        let first_interrupt = platform
            .global()
            .compatibility()
            .gic_metadata()
            .spi_interrupt_range
            .base;
        let network_state = network_state(
            SnapshotV2DeviceTransportKind::Mmio,
            interface_count,
            false,
            MmdsSelection::None,
            first_interrupt,
            None,
        );
        let network = prepare_topology(network_state.clone());
        let vsock_interrupt = first_interrupt
            .checked_add(u32::try_from(interface_count).expect("interface count should fit"))
            .expect("vsock interrupt should fit");
        let (vsock, key) =
            prepared_vsock_endpoint(active_mmio_vsock_state(vsock_interrupt), &memory);
        let binding_keys = exact_vsock_binding_keys(None, &network, Some(key));
        (
            platform.clone(),
            HvfSnapshotV2VsockPreparedProductParts {
                kind: vsock_kind(0b110000),
                memory: HvfSnapshotV2VsockPreparedMemory::Static(platform.memory().clone()),
                storage: None,
                entropy: None,
                balloon: None,
                network,
                vsock: Some(vsock),
                serial_resource_present: false,
                binding_keys,
            },
            vsock_mmio_process_config(),
            network_state,
            _image,
        )
    }

    fn pci_vsock_only_parts(
        slot: usize,
        route_offset: u32,
        interrupt_count: Option<u32>,
    ) -> (
        HvfSnapshotV2PlatformState,
        HvfSnapshotV2VsockPreparedProductParts,
        HvfGicMsiMetadata,
        TestImage,
    ) {
        let expected_interrupt_count = pci_vsock_restore_gic_msi_configuration(None, false, false)
            .expect("vsock MSI configuration should validate")
            .interrupt_count()
            .get();
        let msi = test_msi(interrupt_count.unwrap_or(expected_interrupt_count));
        let MaterializedFixture {
            platform,
            topology,
            memory,
            _image,
        } = materialized_fixture(memory_hotplug_pci_state(0, msi, 0));
        let platform =
            memory_fixture_pci_platform(platform, topology.state(), msi.interrupt_range.count);
        let (vsock, key) =
            prepared_vsock_endpoint(active_pci_vsock_state(slot, msi, route_offset), &memory);
        let network =
            PreparedSnapshotV2NetworkRestoreTopology::empty(SnapshotV2DeviceTransportKind::Pci);
        (
            platform.clone(),
            HvfSnapshotV2VsockPreparedProductParts {
                kind: vsock_kind(0b100000),
                memory: HvfSnapshotV2VsockPreparedMemory::Static(platform.memory().clone()),
                storage: None,
                entropy: None,
                balloon: None,
                network,
                vsock: Some(vsock),
                serial_resource_present: false,
                binding_keys: vec![key],
            },
            msi,
            _image,
        )
    }

    fn pci_network_vsock_parts(
        interface_count: usize,
        duplicate_vsock_routes: bool,
    ) -> (
        HvfSnapshotV2PlatformState,
        HvfSnapshotV2VsockPreparedProductParts,
        HvfGicMsiMetadata,
        SnapshotV2NetworkState,
        TestImage,
    ) {
        let interrupt_count = pci_vsock_restore_gic_msi_configuration(None, false, false)
            .expect("network and vsock MSI configuration should validate")
            .interrupt_count()
            .get();
        let msi = test_msi(interrupt_count);
        let MaterializedFixture {
            platform,
            topology,
            memory,
            _image,
        } = materialized_fixture(memory_hotplug_pci_state(0, msi, 0));
        let platform = memory_fixture_pci_platform(platform, topology.state(), interrupt_count);
        let network_state = network_state(
            SnapshotV2DeviceTransportKind::Pci,
            interface_count,
            true,
            MmdsSelection::None,
            0,
            Some((0, msi, 0, false)),
        );
        let network = prepare_topology(network_state.clone());
        let route_offset = if duplicate_vsock_routes {
            0
        } else {
            u32::try_from(interface_count * (VIRTIO_NET_QUEUE_COUNT + 1))
                .expect("network route count should fit")
        };
        let (vsock, key) = prepared_vsock_endpoint(
            active_pci_vsock_state(interface_count, msi, route_offset),
            &memory,
        );
        let binding_keys = exact_vsock_binding_keys(None, &network, Some(key));
        (
            platform.clone(),
            HvfSnapshotV2VsockPreparedProductParts {
                kind: vsock_kind(0b110000),
                memory: HvfSnapshotV2VsockPreparedMemory::Static(platform.memory().clone()),
                storage: None,
                entropy: None,
                balloon: None,
                network,
                vsock: Some(vsock),
                serial_resource_present: false,
                binding_keys,
            },
            msi,
            network_state,
            _image,
        )
    }

    fn decode_network(fixture: &str) -> SnapshotV2NetworkState {
        SnapshotV2NetworkState::decode(
            NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION,
            &fixture_bytes(fixture),
        )
        .expect("network fixture should decode")
    }

    fn active_source(
        index: usize,
        pci: Option<(usize, HvfGicMsiMetadata, u32)>,
    ) -> SnapshotV2NetworkInterfaceState {
        let mut bytes = fixture_bytes(ACTIVE_PCI_MMDS_HEX);
        let common_offset = record_section_offset(&bytes, 2);
        let queue_base = NETWORK_QUEUE_BASE
            .checked_add(
                u64::try_from(index)
                    .expect("interface index should fit")
                    .checked_mul(NETWORK_INTERFACE_QUEUE_STRIDE)
                    .expect("interface queue offset should fit"),
            )
            .expect("interface queue base should fit");
        relocate_queues(&mut bytes, common_offset, queue_base);
        if let Some((slot, msi, route_offset)) = pci {
            let transport_offset = record_section_offset(&bytes, 4);
            relocate_pci(&mut bytes, transport_offset, slot, msi, route_offset);
        }
        SnapshotV2NetworkState::decode(NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION, &bytes)
            .expect("relocated active network fixture should decode")
            .interfaces()[0]
            .clone()
    }

    fn network_state(
        transport: SnapshotV2DeviceTransportKind,
        interface_count: usize,
        active: bool,
        mmds_selection: MmdsSelection,
        first_interrupt: u32,
        pci: Option<(usize, HvfGicMsiMetadata, u32, bool)>,
    ) -> SnapshotV2NetworkState {
        assert!((1..=NATIVE_V2_NETWORK_MAX_INTERFACES).contains(&interface_count));
        assert!(
            !matches!(mmds_selection, MmdsSelection::Subset) || interface_count > 1,
            "a strict MMDS subset needs at least two interfaces",
        );
        let inactive = decode_network(INACTIVE_MMIO_HEX);
        let inactive_source = &inactive.interfaces()[0];
        let SnapshotV2DeviceTransport::Mmio(inactive_mmio) = inactive_source.transport() else {
            panic!("inactive fixture should use MMIO");
        };
        let backend = if matches!(mmds_selection, MmdsSelection::All) {
            SnapshotV2NetworkBackendClass::MmdsOnly
        } else {
            SnapshotV2NetworkBackendClass::Vmnet
        };
        let interfaces = (0..interface_count)
            .map(|index| {
                let pci_projection =
                    pci.map(|(first_slot, msi, first_route_offset, duplicate_routes)| {
                        let route_offset = if duplicate_routes {
                            first_route_offset
                        } else {
                            first_route_offset
                                .checked_add(
                                    u32::try_from(index * (VIRTIO_NET_QUEUE_COUNT + 1))
                                        .expect("network route offset should fit"),
                                )
                                .expect("network route offset should fit")
                        };
                        (first_slot + index, msi, route_offset)
                    });
                let active_projection = (active || transport == SnapshotV2DeviceTransportKind::Pci)
                    .then(|| active_source(index, pci_projection));
                let source = active_projection
                    .as_ref()
                    .filter(|_| active)
                    .unwrap_or(inactive_source);
                let index_u64 = u64::try_from(index).expect("interface index should fit");
                let guest_mac = GuestMacAddress::from_bytes([
                    0x02,
                    0,
                    0,
                    0,
                    0x60,
                    u8::try_from(index).expect("interface MAC index should fit"),
                ]);
                let profile = NetworkDeviceProfile::new(Some(guest_mac), source.requested_mtu())
                    .with_packet_envelope(source.profile().packet_envelope())
                    .with_feature_capabilities(source.profile().feature_capabilities());
                let projected_transport = match transport {
                    SnapshotV2DeviceTransportKind::Mmio => {
                        let region = MmioRegion::new(
                            MmioRegionId::new(
                                NETWORK_MMIO_REGION_ID
                                    .raw_value()
                                    .checked_add(index_u64)
                                    .expect("network region ID should fit"),
                            ),
                            GuestAddress::new(
                                NETWORK_MMIO_BASE
                                    .raw_value()
                                    .checked_add(
                                        index_u64
                                            .checked_mul(VIRTIO_MMIO_DEVICE_WINDOW_SIZE)
                                            .expect("network MMIO offset should fit"),
                                    )
                                    .expect("network MMIO address should fit"),
                            ),
                            VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
                        )
                        .expect("network MMIO region should validate");
                        SnapshotV2DeviceTransport::Mmio(SnapshotV2MmioDeviceState::from_parts(
                            inactive_mmio.device_feature_select(),
                            inactive_mmio.driver_feature_select(),
                            inactive_mmio.queue_select(),
                            region,
                            GuestInterruptLine::new(
                                first_interrupt
                                    .checked_add(
                                        u32::try_from(index)
                                            .expect("network interrupt index should fit"),
                                    )
                                    .expect("network interrupt should fit"),
                            )
                            .expect("network interrupt should validate"),
                        ))
                    }
                    SnapshotV2DeviceTransportKind::Pci => active_projection
                        .as_ref()
                        .expect("PCI placement source should exist")
                        .transport()
                        .clone(),
                };
                SnapshotV2NetworkInterfaceState::try_from_parts(
                    SnapshotV2NetworkInterfaceStateParts {
                        iface_id: format!("eth{index}"),
                        captured_selector: format!("captured{index}"),
                        requested_guest_mac: Some(guest_mac),
                        requested_mtu: source.requested_mtu(),
                        profile,
                        backend,
                        local: source.local().clone(),
                        virtio: source.virtio().clone(),
                        rx_limiter: source.rx_limiter(),
                        tx_limiter: source.tx_limiter(),
                        transport: projected_transport,
                    },
                )
                .expect("projected network interface should validate")
            })
            .collect::<Vec<_>>();

        let source_mmds = decode_network(ACTIVE_PCI_MMDS_HEX)
            .mmds()
            .expect("active fixture should retain MMDS")
            .clone();
        let selected = match mmds_selection {
            MmdsSelection::None => Vec::new(),
            MmdsSelection::Subset => vec![0],
            MmdsSelection::All => (0..interface_count).collect(),
        };
        let mmds = (!selected.is_empty()).then(|| {
            let source_stack = source_mmds.interfaces()[0];
            SnapshotV2MmdsState::new(
                source_mmds.version(),
                source_mmds.ipv4_address(),
                source_mmds.imds_compat(),
                selected
                    .into_iter()
                    .map(|index| {
                        SnapshotV2MmdsInterfaceState::new(
                            u16::try_from(index).expect("MMDS interface index should fit"),
                            source_stack.local_mac_address(),
                            source_stack.ipv4_address(),
                            source_stack.tcp_port(),
                        )
                    })
                    .collect(),
            )
        });
        SnapshotV2NetworkState::try_new(interfaces, mmds)
            .expect("aggregate network state should validate")
    }

    fn prepare_topology(state: SnapshotV2NetworkState) -> PreparedSnapshotV2NetworkRestoreTopology {
        let overrides = state
            .interfaces()
            .iter()
            .map(|interface| SnapshotNetworkOverride::new(interface.iface_id(), "vmnet:shared"))
            .collect::<Vec<_>>();
        PreparedSnapshotV2NetworkRestoreTopology::prepare(state, &overrides)
            .expect("network topology should prepare")
    }

    fn track_dirty_pages(platform: HvfSnapshotV2PlatformState) -> HvfSnapshotV2PlatformState {
        let (memory, machine, global, topology, vcpus, time) = platform.into_parts();
        let tracked_machine = HvfSnapshotV2MachineState::try_new(
            machine.machine().with_track_dirty_pages(true),
            machine.boot().clone(),
            machine.fdt(),
            machine.cpu_template().cloned(),
        )
        .expect("tracked machine should validate");
        HvfSnapshotV2PlatformState::try_new(memory, tracked_machine, global, topology, vcpus, time)
            .expect("tracked platform should validate")
    }

    fn test_msi(interrupt_count: u32) -> HvfGicMsiMetadata {
        HvfGicMsiMetadata {
            region: HvfGicRegion {
                base: PCI_MSI_REGION_BASE,
                size: PCI_MSI_REGION_SIZE,
            },
            interrupt_range: HvfGicInterruptRange {
                base: 35,
                count: interrupt_count,
            },
        }
    }

    fn mmio_process_config() -> HvfSnapshotV2NetworkMmioProcessConfig {
        HvfSnapshotV2NetworkMmioProcessConfig::new(
            BalloonMmioLayout::new(GuestAddress::new(0x1000_0000), MmioRegionId::new(1)),
            HvfSnapshotV2StorageMmioProcessConfig::new(
                BlockMmioLayout::new(GuestAddress::new(0xd100_0000), MmioRegionId::new(200)),
                PmemMmioLayout::new(GuestAddress::new(0xd000_0000), MmioRegionId::new(100)),
            ),
            NetworkMmioLayout::new(NETWORK_MMIO_BASE, NETWORK_MMIO_REGION_ID),
            EntropyMmioLayout::new(GuestAddress::new(0xd001_0000), MmioRegionId::new(101)),
            VirtioMemMmioLayout::new(GuestAddress::new(0xd002_0000), MmioRegionId::new(102)),
        )
    }

    fn mmio_product(
        interface_count: usize,
        active: bool,
        mmds_selection: MmdsSelection,
    ) -> (
        HvfSnapshotV2PlatformState,
        HvfSnapshotV2NetworkPreparedProduct,
    ) {
        let platform = track_dirty_pages(product_mmio_platform(interface_count));
        let first_interrupt = platform
            .global()
            .compatibility()
            .gic_metadata()
            .spi_interrupt_range
            .base;
        let topology = prepare_topology(network_state(
            SnapshotV2DeviceTransportKind::Mmio,
            interface_count,
            active,
            mmds_selection,
            first_interrupt,
            None,
        ));
        let product = HvfSnapshotV2NetworkPreparedProduct::serial_network(
            platform.memory().clone(),
            topology,
        );
        (platform, product)
    }

    fn pci_product(
        interface_count: usize,
        active: bool,
        mmds_selection: MmdsSelection,
        duplicate_routes: bool,
        first_slot: usize,
    ) -> (
        HvfSnapshotV2PlatformState,
        HvfSnapshotV2NetworkPreparedProduct,
    ) {
        let platform = track_dirty_pages(product_pci_platform());
        let msi = platform
            .global()
            .compatibility()
            .gic_metadata()
            .msi
            .expect("PCI platform should retain MSI metadata");
        let topology = prepare_topology(network_state(
            SnapshotV2DeviceTransportKind::Pci,
            interface_count,
            active,
            mmds_selection,
            0,
            Some((first_slot, msi, 0, duplicate_routes)),
        ));
        let product = HvfSnapshotV2NetworkPreparedProduct::serial_network(
            platform.memory().clone(),
            topology,
        );
        (platform, product)
    }

    fn prepared_product(
        binding: SnapshotV2MemoryBinding,
        network: PreparedSnapshotV2NetworkRestoreTopology,
        balloon: Option<SnapshotV2BalloonRestorePlan>,
        storage: Option<PreparedSnapshotV2StorageBundle>,
        entropy: Option<SnapshotV2EntropyRestorePlan>,
        memory_hotplug: Option<(PreparedSnapshotV2MemoryHotplugTopology, GuestMemory)>,
    ) -> HvfSnapshotV2NetworkPreparedProduct {
        match (memory_hotplug, balloon, storage, entropy) {
            (None, None, None, None) => {
                HvfSnapshotV2NetworkPreparedProduct::serial_network(binding, network)
            }
            (None, None, Some(storage), None) => {
                HvfSnapshotV2NetworkPreparedProduct::serial_storage_network(
                    binding, network, storage,
                )
            }
            (None, None, None, Some(entropy)) => {
                HvfSnapshotV2NetworkPreparedProduct::serial_entropy_network(
                    binding, network, entropy,
                )
            }
            (None, None, Some(storage), Some(entropy)) => {
                HvfSnapshotV2NetworkPreparedProduct::serial_storage_entropy_network(
                    binding, network, storage, entropy,
                )
            }
            (None, Some(balloon), None, None) => {
                HvfSnapshotV2NetworkPreparedProduct::serial_balloon_network(
                    binding, network, balloon,
                )
            }
            (None, Some(balloon), Some(storage), None) => {
                HvfSnapshotV2NetworkPreparedProduct::serial_balloon_storage_network(
                    binding, network, balloon, storage,
                )
            }
            (None, Some(balloon), None, Some(entropy)) => {
                HvfSnapshotV2NetworkPreparedProduct::serial_balloon_entropy_network(
                    binding, network, balloon, entropy,
                )
            }
            (None, Some(balloon), Some(storage), Some(entropy)) => {
                HvfSnapshotV2NetworkPreparedProduct::serial_balloon_storage_entropy_network(
                    binding, network, balloon, storage, entropy,
                )
            }
            (Some((topology, memory)), None, None, None) => {
                drop(binding);
                HvfSnapshotV2NetworkPreparedProduct::serial_network_memory_hotplug(
                    topology, memory, network,
                )
            }
            (Some((topology, memory)), None, Some(storage), None) => {
                drop(binding);
                HvfSnapshotV2NetworkPreparedProduct::serial_storage_network_memory_hotplug(
                    topology, memory, network, storage,
                )
            }
            (Some((topology, memory)), None, None, Some(entropy)) => {
                drop(binding);
                HvfSnapshotV2NetworkPreparedProduct::serial_entropy_network_memory_hotplug(
                    topology, memory, network, entropy,
                )
            }
            (Some((topology, memory)), None, Some(storage), Some(entropy)) => {
                drop(binding);
                HvfSnapshotV2NetworkPreparedProduct::serial_storage_entropy_network_memory_hotplug(
                    topology, memory, network, storage, entropy,
                )
            }
            (Some((topology, memory)), Some(balloon), None, None) => {
                drop(binding);
                HvfSnapshotV2NetworkPreparedProduct::serial_balloon_network_memory_hotplug(
                    topology, memory, network, balloon,
                )
            }
            (Some((topology, memory)), Some(balloon), Some(storage), None) => {
                drop(binding);
                HvfSnapshotV2NetworkPreparedProduct::serial_balloon_storage_network_memory_hotplug(
                    topology, memory, network, balloon, storage,
                )
            }
            (Some((topology, memory)), Some(balloon), None, Some(entropy)) => {
                drop(binding);
                HvfSnapshotV2NetworkPreparedProduct::serial_balloon_entropy_network_memory_hotplug(
                    topology, memory, network, balloon, entropy,
                )
            }
            (Some((topology, memory)), Some(balloon), Some(storage), Some(entropy)) => {
                drop(binding);
                HvfSnapshotV2NetworkPreparedProduct::
                    serial_balloon_storage_entropy_network_memory_hotplug(
                        topology, memory, network, balloon, storage, entropy,
                    )
            }
        }
    }

    fn expected_kind(
        has_balloon: bool,
        has_storage: bool,
        has_entropy: bool,
        has_memory_hotplug: bool,
    ) -> HvfSnapshotV2NetworkProductKind {
        match (has_memory_hotplug, has_balloon, has_storage, has_entropy) {
            (false, false, false, false) => HvfSnapshotV2NetworkProductKind::SerialNetwork,
            (false, false, true, false) => HvfSnapshotV2NetworkProductKind::SerialStorageNetwork,
            (false, false, false, true) => HvfSnapshotV2NetworkProductKind::SerialEntropyNetwork,
            (false, false, true, true) => {
                HvfSnapshotV2NetworkProductKind::SerialStorageEntropyNetwork
            }
            (false, true, false, false) => HvfSnapshotV2NetworkProductKind::SerialBalloonNetwork,
            (false, true, true, false) => {
                HvfSnapshotV2NetworkProductKind::SerialBalloonStorageNetwork
            }
            (false, true, false, true) => {
                HvfSnapshotV2NetworkProductKind::SerialBalloonEntropyNetwork
            }
            (false, true, true, true) => {
                HvfSnapshotV2NetworkProductKind::SerialBalloonStorageEntropyNetwork
            }
            (true, false, false, false) => {
                HvfSnapshotV2NetworkProductKind::SerialNetworkMemoryHotplug
            }
            (true, false, true, false) => {
                HvfSnapshotV2NetworkProductKind::SerialStorageNetworkMemoryHotplug
            }
            (true, false, false, true) => {
                HvfSnapshotV2NetworkProductKind::SerialEntropyNetworkMemoryHotplug
            }
            (true, false, true, true) => {
                HvfSnapshotV2NetworkProductKind::SerialStorageEntropyNetworkMemoryHotplug
            }
            (true, true, false, false) => {
                HvfSnapshotV2NetworkProductKind::SerialBalloonNetworkMemoryHotplug
            }
            (true, true, true, false) => {
                HvfSnapshotV2NetworkProductKind::SerialBalloonStorageNetworkMemoryHotplug
            }
            (true, true, false, true) => {
                HvfSnapshotV2NetworkProductKind::SerialBalloonEntropyNetworkMemoryHotplug
            }
            (true, true, true, true) => {
                HvfSnapshotV2NetworkProductKind::SerialBalloonStorageEntropyNetworkMemoryHotplug
            }
        }
    }

    #[test]
    fn one_and_sixteen_mmio_interfaces_close_layout_identity_queues_and_mmds() {
        for (interface_count, active, mmds_selection, expect_mmds) in [
            (1, false, MmdsSelection::None, false),
            (16, true, MmdsSelection::All, true),
        ] {
            let (platform, product) = mmio_product(interface_count, active, mmds_selection);
            for (index, interface) in product.network().interfaces().iter().enumerate() {
                for ranges in interface_queue_ranges(interface)
                    .expect("network queue ranges should project")
                    .into_iter()
                    .flatten()
                {
                    assert!(
                        !queue_ranges_conflict_with_platform(&platform, Some(ranges))
                            .expect("queue/platform relationship should validate"),
                        "MMIO interface {index} queue {ranges:?} conflicts with the fixed platform",
                    );
                }
            }
            let first_interrupt = platform
                .global()
                .compatibility()
                .gic_metadata()
                .spi_interrupt_range
                .base;
            let plan = prepare_hvf_snapshot_v2_network_mmio_platform_plan(
                &platform,
                product,
                mmio_process_config(),
            )
            .expect("network MMIO platform should plan");

            assert_eq!(plan.kind(), HvfSnapshotV2NetworkProductKind::SerialNetwork);
            assert_eq!(plan.network().len(), interface_count);
            assert_eq!(plan.product().has_mmds(), expect_mmds);
            for (index, endpoint) in plan.network().iter().enumerate() {
                assert_eq!(endpoint.source_index(), u16::try_from(index).unwrap());
                assert_eq!(
                    endpoint.resource_key().public_id().as_str(),
                    format!("eth{index}"),
                );
                assert_eq!(
                    endpoint.region(),
                    mmio_process_config()
                        .network_layout()
                        .region_at(index)
                        .expect("expected network region should project"),
                );
                assert_eq!(
                    endpoint.interrupt_line().raw_value(),
                    first_interrupt + u32::try_from(index).unwrap(),
                );
                assert_eq!(endpoint.mmds_stack().is_some(), expect_mmds);
                assert_eq!(endpoint.queue_ranges().iter().all(Option::is_some), active,);
            }
            assert_eq!(
                plan.serial_interrupt().raw_value(),
                first_interrupt + u32::try_from(interface_count).unwrap(),
            );
            assert_eq!(
                plan.vmgenid_interrupt().raw_value(),
                plan.serial_interrupt().raw_value() + 1,
            );
            assert_eq!(
                plan.vmclock_interrupt().raw_value(),
                plan.serial_interrupt().raw_value() + 2,
            );
        }
    }

    #[test]
    fn one_and_sixteen_pci_interfaces_close_slots_vectors_routes_and_mmds() {
        for (interface_count, mmds_selection, selected_count) in
            [(1, MmdsSelection::All, 1), (16, MmdsSelection::Subset, 1)]
        {
            let (platform, product) = pci_product(interface_count, true, mmds_selection, false, 0);
            let address_plan =
                Arm64PciAddressPlan::firecracker_v1_16().expect("address plan should validate");
            for (index, interface) in product.network().interfaces().iter().enumerate() {
                for ranges in interface_queue_ranges(interface)
                    .expect("network queue ranges should project")
                    .into_iter()
                    .flatten()
                {
                    assert!(
                        !queue_ranges_conflict_with_pci_platform(
                            &platform,
                            Some(ranges),
                            &platform.global().compatibility().gic_metadata(),
                            address_plan,
                        )
                        .expect("queue/PCI relationship should validate"),
                        "PCI interface {index} queue {ranges:?} conflicts with the fixed platform",
                    );
                }
            }
            let plan = prepare_hvf_snapshot_v2_network_pci_platform_plan(&platform, product)
                .expect("network PCI platform should plan");

            assert_eq!(plan.kind(), HvfSnapshotV2NetworkProductKind::SerialNetwork);
            assert_eq!(plan.endpoint_count(), interface_count);
            assert_eq!(
                plan.route_demand(),
                interface_count * (VIRTIO_NET_QUEUE_COUNT + 1),
            );
            assert_eq!(plan.network().len(), interface_count);
            assert_eq!(
                plan.network()
                    .iter()
                    .filter(|endpoint| endpoint.mmds_stack().is_some())
                    .count(),
                selected_count,
            );
            for (index, endpoint) in plan.network().iter().enumerate() {
                let placement = snapshot_v2_pci_endpoint_placement(address_plan, index)
                    .expect("expected endpoint placement should exist");
                assert_eq!(endpoint.source_index(), u16::try_from(index).unwrap());
                assert_eq!(endpoint.sbdf(), placement.sbdf);
                assert_eq!(endpoint.bar_range(), placement.bar_range);
                assert_eq!(endpoint.bar_region_id(), placement.bar_region_id);
                assert_eq!(endpoint.route_count(), VIRTIO_NET_QUEUE_COUNT + 1);
                assert_eq!(endpoint.queue_vectors(), &[1, 2]);
                assert_eq!(endpoint.config_vector(), 0);
                assert_eq!(
                    endpoint.msi_interrupt_count(),
                    plan.msi().interrupt_range.count
                );
                assert!(endpoint.queue_ranges().iter().all(Option::is_some));
            }
        }
    }

    #[test]
    fn all_sixteen_mmio_product_tags_close_canonical_component_order() {
        let storage_graph = product_storage_fixture(SnapshotV2DeviceTransportKind::Mmio);
        let storage_block_count = storage_graph.block_records().len();
        let storage_pmem_count = storage_graph.pmem_records().len();
        for mask in 0_u8..16 {
            let has_storage = mask & 1 != 0;
            let has_entropy = mask & 2 != 0;
            let has_balloon = mask & 4 != 0;
            let has_memory_hotplug = mask & 8 != 0;
            let network_count = if mask == 15 { 16 } else { 1 };
            let block_count = usize::from(has_storage) * storage_block_count;
            let pmem_count = usize::from(has_storage) * storage_pmem_count;
            let first_interrupt = 32_u32;
            let storage_interrupt = first_interrupt + u32::from(has_balloon);
            let network_interrupt = storage_interrupt
                + u32::try_from(block_count).expect("block interrupt count should fit");
            let pmem_interrupt =
                network_interrupt + u32::try_from(network_count).expect("network count should fit");
            let entropy_interrupt = pmem_interrupt
                + u32::try_from(pmem_count).expect("pmem interrupt count should fit");
            let memory_hotplug_interrupt = entropy_interrupt + u32::from(has_entropy);
            let device_count = usize::from(has_balloon)
                + block_count
                + network_count
                + pmem_count
                + usize::from(has_entropy)
                + usize::from(has_memory_hotplug);

            let fixture = materialized_fixture(memory_hotplug_mmio_state(memory_hotplug_interrupt));
            let MaterializedFixture {
                platform,
                topology,
                memory,
                _image,
            } = fixture;
            let platform = memory_fixture_mmio_platform(platform, topology.state(), device_count);
            assert_eq!(
                platform
                    .global()
                    .compatibility()
                    .gic_metadata()
                    .spi_interrupt_range
                    .base,
                first_interrupt,
            );
            let network = prepare_topology(network_state(
                SnapshotV2DeviceTransportKind::Mmio,
                network_count,
                network_count == 1,
                MmdsSelection::All,
                network_interrupt,
                None,
            ));
            let balloon = has_balloon.then(|| balloon_mmio_plan(&memory, first_interrupt));
            let entropy = has_entropy.then(|| entropy_mmio_plan(&memory, entropy_interrupt));
            let (storage, _backings) = if has_storage {
                let graph = storage_mmio_graph_with_gap(storage_interrupt, network_count);
                let (bundle, backings) = prepared_storage_bundle(graph);
                (Some(bundle), backings)
            } else {
                (None, Vec::new())
            };
            let binding = platform.memory().clone();
            let memory_hotplug = has_memory_hotplug.then_some((topology, memory));
            let product =
                prepared_product(binding, network, balloon, storage, entropy, memory_hotplug);
            let plan = prepare_hvf_snapshot_v2_network_mmio_platform_plan(
                &platform,
                product,
                mmio_process_config(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "MMIO balloon={has_balloon} storage={has_storage} entropy={has_entropy} memory-hotplug={has_memory_hotplug} failed: {error:?}"
                )
            });

            assert_eq!(
                plan.kind(),
                expected_kind(has_balloon, has_storage, has_entropy, has_memory_hotplug,),
            );
            assert_eq!(plan.balloon().is_some(), has_balloon);
            assert_eq!(plan.storage().is_some(), has_storage);
            assert_eq!(plan.entropy().is_some(), has_entropy);
            assert_eq!(plan.memory_hotplug().is_some(), has_memory_hotplug);
            assert_eq!(plan.mapping().is_some(), has_memory_hotplug);
            assert_eq!(plan.network().len(), network_count);
            assert_eq!(
                plan.network()[0].interrupt_line().raw_value(),
                network_interrupt,
            );
            if let Some(storage) = plan.storage() {
                for (index, record) in storage.block_records().iter().enumerate() {
                    assert_eq!(
                        record.interrupt_line().raw_value(),
                        storage_interrupt + u32::try_from(index).unwrap(),
                    );
                }
                for (index, record) in storage.pmem_records().iter().enumerate() {
                    assert_eq!(
                        record.interrupt_line().raw_value(),
                        pmem_interrupt + u32::try_from(index).unwrap(),
                    );
                }
            }
            assert_eq!(
                plan.entropy()
                    .map(|endpoint| endpoint.interrupt_line().raw_value()),
                has_entropy.then_some(entropy_interrupt),
            );
            assert_eq!(
                plan.memory_hotplug()
                    .map(|endpoint| endpoint.interrupt_line().raw_value()),
                has_memory_hotplug.then_some(memory_hotplug_interrupt),
            );
            assert_eq!(
                plan.serial_interrupt().raw_value(),
                first_interrupt + u32::try_from(device_count).unwrap(),
            );
            let owner_parts = plan.into_owner_parts();
            assert_eq!(owner_parts.storage.is_some(), has_storage);
            assert_eq!(owner_parts.entropy.is_some(), has_entropy);
            assert_eq!(owner_parts.memory_hotplug.is_some(), has_memory_hotplug);
            let product_parts = owner_parts.product.into_owner_parts();
            assert_eq!(product_parts.storage.is_some(), has_storage);
            assert_eq!(product_parts.entropy.is_some(), has_entropy);
            assert_eq!(product_parts.balloon.is_some(), has_balloon);
            assert_eq!(
                matches!(
                    product_parts.memory,
                    HvfSnapshotV2NetworkPreparedMemoryProduct::MemoryHotplug { .. }
                ),
                has_memory_hotplug
            );
        }
    }

    #[test]
    fn all_sixty_four_mmio_vsock_products_close_presence_order_and_queue_ranges() {
        let storage_shape = product_storage_fixture(SnapshotV2DeviceTransportKind::Mmio);
        let storage_block_count = storage_shape.block_records().len();
        let storage_pmem_count = storage_shape.pmem_records().len();
        for mask in 0_u8..64 {
            let has_storage = mask & 1 != 0;
            let has_entropy = mask & 2 != 0;
            let has_balloon = mask & 4 != 0;
            let has_memory_hotplug = mask & 8 != 0;
            let has_network = mask & 16 != 0;
            let has_vsock = mask & 32 != 0;
            let network_count = usize::from(has_network);
            let block_count = usize::from(has_storage) * storage_block_count;
            let pmem_count = usize::from(has_storage) * storage_pmem_count;
            let first_interrupt = 32_u32;
            let storage_interrupt = first_interrupt + u32::from(has_balloon);
            let network_interrupt = storage_interrupt
                + u32::try_from(block_count).expect("block interrupt count should fit");
            let pmem_interrupt =
                network_interrupt + u32::try_from(network_count).expect("network count should fit");
            let vsock_interrupt = pmem_interrupt
                + u32::try_from(pmem_count).expect("pmem interrupt count should fit");
            let entropy_interrupt = vsock_interrupt + u32::from(has_vsock);
            let memory_hotplug_interrupt = entropy_interrupt + u32::from(has_entropy);
            let device_count = usize::from(has_balloon)
                + block_count
                + network_count
                + pmem_count
                + usize::from(has_vsock)
                + usize::from(has_entropy)
                + usize::from(has_memory_hotplug);

            let MaterializedFixture {
                platform,
                topology,
                memory,
                _image,
            } = materialized_fixture(memory_hotplug_mmio_state(memory_hotplug_interrupt));
            let platform = memory_fixture_mmio_platform(platform, topology.state(), device_count);
            let network = if has_network {
                prepare_topology(network_state(
                    SnapshotV2DeviceTransportKind::Mmio,
                    1,
                    false,
                    MmdsSelection::None,
                    network_interrupt,
                    None,
                ))
            } else {
                PreparedSnapshotV2NetworkRestoreTopology::empty(SnapshotV2DeviceTransportKind::Mmio)
            };
            let balloon = has_balloon.then(|| balloon_mmio_plan(&memory, first_interrupt));
            let entropy = has_entropy.then(|| entropy_mmio_plan(&memory, entropy_interrupt));
            let (storage_graph, storage, _backings) = if has_storage {
                let graph = storage_mmio_graph_with_gap(storage_interrupt, network_count);
                let (bundle, backings) = prepared_storage_bundle(graph.clone());
                (Some(graph), Some(bundle), backings)
            } else {
                (None, None, Vec::new())
            };
            let (vsock, vsock_key) = if has_vsock {
                let (endpoint, key) =
                    prepared_vsock_endpoint(active_mmio_vsock_state(vsock_interrupt), &memory);
                (Some(endpoint), Some(key))
            } else {
                (None, None)
            };
            let binding_keys =
                exact_vsock_binding_keys(storage_graph.as_ref(), &network, vsock_key);
            let prepared_memory = if has_memory_hotplug {
                HvfSnapshotV2VsockPreparedMemory::MemoryHotplug {
                    topology: Box::new(topology),
                    memory,
                }
            } else {
                HvfSnapshotV2VsockPreparedMemory::Static(platform.memory().clone())
            };
            let kind = HvfSnapshotV2VsockProductKind::from_presence(
                has_storage,
                has_entropy,
                has_balloon,
                has_memory_hotplug,
                has_network,
                has_vsock,
            );
            let product = HvfSnapshotV2VsockPreparedProduct::try_from_parts(
                HvfSnapshotV2VsockPreparedProductParts {
                    kind,
                    memory: prepared_memory,
                    storage,
                    entropy,
                    balloon,
                    network,
                    vsock,
                    serial_resource_present: false,
                    binding_keys,
                },
            )
            .unwrap_or_else(|error| panic!("MMIO product mask {mask:#08b} failed: {error:?}"));
            let plan = prepare_hvf_snapshot_v2_vsock_mmio_platform_plan(
                &platform,
                product,
                vsock_mmio_process_config(),
            )
            .unwrap_or_else(|error| panic!("MMIO plan mask {mask:#08b} failed: {error:?}"));

            assert_eq!(plan.kind(), kind);
            assert_eq!(plan.balloon().is_some(), has_balloon);
            assert_eq!(plan.storage().is_some(), has_storage);
            assert_eq!(plan.network().len(), network_count);
            assert_eq!(plan.vsock().is_some(), has_vsock);
            assert_eq!(plan.entropy().is_some(), has_entropy);
            assert_eq!(plan.memory_hotplug().is_some(), has_memory_hotplug);
            assert_eq!(plan.mapping().is_some(), has_memory_hotplug);
            if let Some(vsock) = plan.vsock() {
                assert_eq!(vsock.region().range().start(), VSOCK_MMIO_BASE);
                assert_eq!(vsock.region().id(), VSOCK_MMIO_REGION_ID);
                assert_eq!(vsock.dispatcher_region_id(), VSOCK_MMIO_REGION_ID);
                assert_eq!(vsock.interrupt_line().raw_value(), vsock_interrupt);
                assert_eq!(vsock.fdt_device().region.base, VSOCK_MMIO_BASE.raw_value());
                assert_eq!(
                    vsock.fdt_device().region.size,
                    VIRTIO_MMIO_DEVICE_WINDOW_SIZE
                );
                assert!(
                    vsock.queue_ranges().iter().all(Option::is_some),
                    "all active vsock queue ranges should be retained",
                );
            }
            assert_eq!(
                plan.entropy()
                    .map(|endpoint| endpoint.interrupt_line().raw_value()),
                has_entropy.then_some(entropy_interrupt),
            );
            assert_eq!(
                plan.memory_hotplug()
                    .map(|endpoint| endpoint.interrupt_line().raw_value()),
                has_memory_hotplug.then_some(memory_hotplug_interrupt),
            );
            assert_eq!(
                plan.serial_interrupt().raw_value(),
                first_interrupt + u32::try_from(device_count).unwrap(),
            );
        }
    }

    #[test]
    fn all_sixteen_pci_product_tags_close_block_network_pmem_order_and_routes() {
        let storage_graph = product_storage_fixture(SnapshotV2DeviceTransportKind::Pci);
        let storage_record_count = storage_graph.record_count();
        let storage_block_count = storage_graph.block_records().len();
        let storage_pmem_count = storage_graph.pmem_records().len();
        for mask in 0_u8..16 {
            let has_storage = mask & 1 != 0;
            let has_entropy = mask & 2 != 0;
            let has_balloon = mask & 4 != 0;
            let has_memory_hotplug = mask & 8 != 0;
            let network_count = if mask == 15 { 16 } else { 1 };
            let block_count = usize::from(has_storage) * storage_block_count;
            let pmem_count = usize::from(has_storage) * storage_pmem_count;
            let storage_count = usize::from(has_storage) * storage_record_count;
            let storage_slot = usize::from(has_balloon);
            let network_slot = storage_slot + block_count;
            let entropy_slot = network_slot + network_count + pmem_count;
            let memory_hotplug_slot = entropy_slot + usize::from(has_entropy);
            let endpoint_count = memory_hotplug_slot + usize::from(has_memory_hotplug);

            let balloon_routes = usize::from(has_balloon) * (VIRTIO_BALLOON_MAX_QUEUE_COUNT + 1);
            let block_routes = block_count * 2;
            let storage_routes = storage_count * 2;
            let network_routes = network_count * (VIRTIO_NET_QUEUE_COUNT + 1);
            let entropy_routes = usize::from(has_entropy) * (VIRTIO_RNG_QUEUE_SIZES.len() + 1);
            let memory_hotplug_routes =
                usize::from(has_memory_hotplug) * (VIRTIO_MEM_QUEUE_SIZES.len() + 1);
            let storage_route_offset =
                u32::try_from(balloon_routes).expect("storage route offset should fit");
            let network_route_offset = u32::try_from(balloon_routes + block_routes)
                .expect("network route offset should fit");
            let entropy_route_offset =
                u32::try_from(balloon_routes + storage_routes + network_routes)
                    .expect("entropy route offset should fit");
            let memory_hotplug_route_offset =
                u32::try_from(balloon_routes + storage_routes + network_routes + entropy_routes)
                    .expect("memory-hotplug route offset should fit");
            let route_demand = balloon_routes
                + storage_routes
                + network_routes
                + entropy_routes
                + memory_hotplug_routes;
            let expected_msi_interrupt_count = if has_memory_hotplug {
                pci_memory_hotplug_restore_gic_msi_configuration(
                    has_balloon.then_some(VIRTIO_BALLOON_MAX_QUEUE_COUNT),
                    has_entropy,
                )
            } else if has_balloon {
                pci_balloon_restore_gic_msi_configuration(
                    VIRTIO_BALLOON_MAX_QUEUE_COUNT,
                    has_entropy,
                )
            } else if has_entropy {
                pci_entropy_restore_gic_msi_configuration()
            } else {
                pci_root_restore_gic_msi_configuration()
            }
            .expect("PCI MSI configuration should validate")
            .interrupt_count()
            .get();
            let msi = test_msi(expected_msi_interrupt_count);

            let memory_hotplug_state =
                memory_hotplug_pci_state(memory_hotplug_slot, msi, memory_hotplug_route_offset);
            let fixture = materialized_fixture(memory_hotplug_state);
            let MaterializedFixture {
                platform,
                topology,
                memory,
                _image,
            } = fixture;
            let platform = memory_fixture_pci_platform(
                platform,
                topology.state(),
                expected_msi_interrupt_count,
            );
            let network = prepare_topology(network_state(
                SnapshotV2DeviceTransportKind::Pci,
                network_count,
                network_count == 1,
                MmdsSelection::All,
                0,
                Some((network_slot, msi, network_route_offset, false)),
            ));
            let balloon = has_balloon.then(|| balloon_pci_plan(0, msi, 0));
            let entropy =
                has_entropy.then(|| entropy_pci_plan(entropy_slot, msi, entropy_route_offset));
            let (storage, _backings) = if has_storage {
                let graph = storage_pci_graph_with_gap(
                    storage_slot,
                    network_count,
                    msi,
                    storage_route_offset,
                );
                let (bundle, backings) = prepared_storage_bundle(graph);
                (Some(bundle), backings)
            } else {
                (None, Vec::new())
            };
            let binding = platform.memory().clone();
            let memory_hotplug = has_memory_hotplug.then_some((topology, memory));
            let product =
                prepared_product(binding, network, balloon, storage, entropy, memory_hotplug);
            let plan =
                prepare_hvf_snapshot_v2_network_pci_platform_plan(&platform, product)
                    .unwrap_or_else(|error| {
                        panic!(
                            "PCI balloon={has_balloon} storage={has_storage} entropy={has_entropy} memory-hotplug={has_memory_hotplug} failed: {error:?}"
                        )
                    });
            let address_plan =
                Arm64PciAddressPlan::firecracker_v1_16().expect("address plan should validate");

            assert_eq!(
                plan.kind(),
                expected_kind(has_balloon, has_storage, has_entropy, has_memory_hotplug,),
            );
            assert_eq!(plan.balloon().is_some(), has_balloon);
            assert_eq!(plan.storage().is_some(), has_storage);
            assert_eq!(plan.entropy().is_some(), has_entropy);
            assert_eq!(plan.memory_hotplug().is_some(), has_memory_hotplug);
            assert_eq!(plan.mapping().is_some(), has_memory_hotplug);
            assert_eq!(plan.endpoint_count(), endpoint_count);
            assert_eq!(plan.route_demand(), route_demand);
            assert_eq!(plan.network().len(), network_count);
            assert_eq!(
                plan.network()[0].sbdf(),
                snapshot_v2_pci_endpoint_placement(address_plan, network_slot)
                    .expect("network placement should exist")
                    .sbdf,
            );
            if let Some(storage) = plan.storage() {
                for (index, record) in storage.pci().block_records().iter().enumerate() {
                    assert_eq!(
                        record.sbdf(),
                        snapshot_v2_pci_endpoint_placement(address_plan, storage_slot + index)
                            .expect("block placement should exist")
                            .sbdf,
                    );
                }
                for (index, record) in storage.pci().pmem_records().iter().enumerate() {
                    assert_eq!(
                        record.sbdf(),
                        snapshot_v2_pci_endpoint_placement(
                            address_plan,
                            network_slot + network_count + index,
                        )
                        .expect("pmem placement should exist")
                        .sbdf,
                    );
                }
            }
            assert_eq!(
                plan.entropy().map(|endpoint| endpoint.sbdf()),
                has_entropy.then(|| {
                    snapshot_v2_pci_endpoint_placement(address_plan, entropy_slot)
                        .expect("entropy placement should exist")
                        .sbdf
                }),
            );
            assert_eq!(
                plan.memory_hotplug().map(|endpoint| endpoint.sbdf()),
                has_memory_hotplug.then(|| {
                    snapshot_v2_pci_endpoint_placement(address_plan, memory_hotplug_slot)
                        .expect("memory-hotplug placement should exist")
                        .sbdf
                }),
            );
        }
    }

    #[test]
    fn all_sixty_four_pci_vsock_products_close_presence_order_vectors_and_routes() {
        let storage_shape = product_storage_fixture(SnapshotV2DeviceTransportKind::Pci);
        let storage_record_count = storage_shape.record_count();
        let storage_block_count = storage_shape.block_records().len();
        let storage_pmem_count = storage_shape.pmem_records().len();
        for mask in 0_u8..64 {
            let has_storage = mask & 1 != 0;
            let has_entropy = mask & 2 != 0;
            let has_balloon = mask & 4 != 0;
            let has_memory_hotplug = mask & 8 != 0;
            let has_network = mask & 16 != 0;
            let has_vsock = mask & 32 != 0;
            let network_count = usize::from(has_network);
            let block_count = usize::from(has_storage) * storage_block_count;
            let pmem_count = usize::from(has_storage) * storage_pmem_count;
            let storage_count = usize::from(has_storage) * storage_record_count;
            let storage_slot = usize::from(has_balloon);
            let network_slot = storage_slot + block_count;
            let vsock_slot = network_slot + network_count + pmem_count;
            let entropy_slot = vsock_slot + usize::from(has_vsock);
            let memory_hotplug_slot = entropy_slot + usize::from(has_entropy);
            let endpoint_count = memory_hotplug_slot + usize::from(has_memory_hotplug);

            let balloon_routes = usize::from(has_balloon) * (VIRTIO_BALLOON_MAX_QUEUE_COUNT + 1);
            let block_routes = block_count * 2;
            let storage_routes = storage_count * 2;
            let network_routes = network_count * (VIRTIO_NET_QUEUE_COUNT + 1);
            let vsock_routes = usize::from(has_vsock) * (VIRTIO_VSOCK_QUEUE_COUNT + 1);
            let entropy_routes = usize::from(has_entropy) * (VIRTIO_RNG_QUEUE_SIZES.len() + 1);
            let memory_hotplug_routes =
                usize::from(has_memory_hotplug) * (VIRTIO_MEM_QUEUE_SIZES.len() + 1);
            let storage_route_offset =
                u32::try_from(balloon_routes).expect("storage route offset should fit");
            let network_route_offset = u32::try_from(balloon_routes + block_routes)
                .expect("network route offset should fit");
            let vsock_route_offset =
                u32::try_from(balloon_routes + storage_routes + network_routes)
                    .expect("vsock route offset should fit");
            let entropy_route_offset =
                u32::try_from(balloon_routes + storage_routes + network_routes + vsock_routes)
                    .expect("entropy route offset should fit");
            let memory_hotplug_route_offset = u32::try_from(
                balloon_routes + storage_routes + network_routes + vsock_routes + entropy_routes,
            )
            .expect("memory-hotplug route offset should fit");
            let route_demand = balloon_routes
                + storage_routes
                + network_routes
                + vsock_routes
                + entropy_routes
                + memory_hotplug_routes;
            let balloon_queue_count = has_balloon.then_some(VIRTIO_BALLOON_MAX_QUEUE_COUNT);
            let expected_msi_interrupt_count = if has_vsock {
                pci_vsock_restore_gic_msi_configuration(
                    balloon_queue_count,
                    has_entropy,
                    has_memory_hotplug,
                )
            } else if has_memory_hotplug {
                pci_memory_hotplug_restore_gic_msi_configuration(balloon_queue_count, has_entropy)
            } else if has_balloon {
                pci_balloon_restore_gic_msi_configuration(
                    VIRTIO_BALLOON_MAX_QUEUE_COUNT,
                    has_entropy,
                )
            } else if has_entropy {
                pci_entropy_restore_gic_msi_configuration()
            } else {
                pci_root_restore_gic_msi_configuration()
            }
            .expect("PCI MSI configuration should validate")
            .interrupt_count()
            .get();
            let msi = test_msi(expected_msi_interrupt_count);

            let MaterializedFixture {
                platform,
                topology,
                memory,
                _image,
            } = materialized_fixture(memory_hotplug_pci_state(
                memory_hotplug_slot,
                msi,
                memory_hotplug_route_offset,
            ));
            let platform = memory_fixture_pci_platform(
                platform,
                topology.state(),
                expected_msi_interrupt_count,
            );
            let network = if has_network {
                prepare_topology(network_state(
                    SnapshotV2DeviceTransportKind::Pci,
                    1,
                    true,
                    MmdsSelection::All,
                    0,
                    Some((network_slot, msi, network_route_offset, false)),
                ))
            } else {
                PreparedSnapshotV2NetworkRestoreTopology::empty(SnapshotV2DeviceTransportKind::Pci)
            };
            let balloon = has_balloon.then(|| balloon_pci_plan(0, msi, 0));
            let entropy =
                has_entropy.then(|| entropy_pci_plan(entropy_slot, msi, entropy_route_offset));
            let (storage_graph, storage, _backings) = if has_storage {
                let graph = storage_pci_graph_with_gap(
                    storage_slot,
                    network_count,
                    msi,
                    storage_route_offset,
                );
                let (bundle, backings) = prepared_storage_bundle(graph.clone());
                (Some(graph), Some(bundle), backings)
            } else {
                (None, None, Vec::new())
            };
            let (vsock, vsock_key) = if has_vsock {
                let (endpoint, key) = prepared_vsock_endpoint(
                    active_pci_vsock_state(vsock_slot, msi, vsock_route_offset),
                    &memory,
                );
                (Some(endpoint), Some(key))
            } else {
                (None, None)
            };
            let binding_keys =
                exact_vsock_binding_keys(storage_graph.as_ref(), &network, vsock_key);
            let prepared_memory = if has_memory_hotplug {
                HvfSnapshotV2VsockPreparedMemory::MemoryHotplug {
                    topology: Box::new(topology),
                    memory,
                }
            } else {
                HvfSnapshotV2VsockPreparedMemory::Static(platform.memory().clone())
            };
            let kind = HvfSnapshotV2VsockProductKind::from_presence(
                has_storage,
                has_entropy,
                has_balloon,
                has_memory_hotplug,
                has_network,
                has_vsock,
            );
            let product = HvfSnapshotV2VsockPreparedProduct::try_from_parts(
                HvfSnapshotV2VsockPreparedProductParts {
                    kind,
                    memory: prepared_memory,
                    storage,
                    entropy,
                    balloon,
                    network,
                    vsock,
                    serial_resource_present: false,
                    binding_keys,
                },
            )
            .unwrap_or_else(|error| panic!("PCI product mask {mask:#08b} failed: {error:?}"));
            let plan = prepare_hvf_snapshot_v2_vsock_pci_platform_plan(&platform, product)
                .unwrap_or_else(|error| match error {
                    PrepareHvfSnapshotV2VsockPlatformPlanError::Network(source) => {
                        panic!("PCI plan mask {mask:#08b} failed: {source:?}")
                    }
                    error => panic!("PCI plan mask {mask:#08b} failed: {error:?}"),
                });
            let address_plan =
                Arm64PciAddressPlan::firecracker_v1_16().expect("address plan should validate");

            assert_eq!(plan.kind(), kind);
            assert_eq!(plan.balloon().is_some(), has_balloon);
            assert_eq!(plan.storage().is_some(), has_storage);
            assert_eq!(plan.network().len(), network_count);
            assert_eq!(plan.vsock().is_some(), has_vsock);
            assert_eq!(plan.entropy().is_some(), has_entropy);
            assert_eq!(plan.memory_hotplug().is_some(), has_memory_hotplug);
            assert_eq!(plan.mapping().is_some(), has_memory_hotplug);
            assert_eq!(plan.endpoint_count(), endpoint_count);
            assert_eq!(plan.route_demand(), route_demand);
            if let Some(vsock) = plan.vsock() {
                let placement = snapshot_v2_pci_endpoint_placement(address_plan, vsock_slot)
                    .expect("vsock placement should validate");
                assert_eq!(vsock.origin(), StorageDeviceOrigin::Startup);
                assert_eq!(vsock.sbdf(), placement.sbdf);
                assert_eq!(vsock.bar_range(), placement.bar_range);
                assert_eq!(vsock.bar_region_id(), placement.bar_region_id);
                assert_eq!(vsock.dispatcher_region_id(), placement.bar_region_id);
                assert_eq!(vsock.route_count(), VIRTIO_VSOCK_QUEUE_COUNT + 1);
                assert_eq!(vsock.queue_vectors(), &[1, 2, 3]);
                assert_eq!(vsock.config_vector(), 0);
                assert_eq!(vsock.msi_interrupt_count(), expected_msi_interrupt_count);
                assert!(vsock.queue_ranges().iter().all(Option::is_some));
            }
            assert_eq!(
                plan.entropy().map(|endpoint| endpoint.sbdf()),
                has_entropy.then(|| {
                    snapshot_v2_pci_endpoint_placement(address_plan, entropy_slot)
                        .expect("entropy placement should exist")
                        .sbdf
                }),
            );
            assert_eq!(
                plan.memory_hotplug().map(|endpoint| endpoint.sbdf()),
                has_memory_hotplug.then(|| {
                    snapshot_v2_pci_endpoint_placement(address_plan, memory_hotplug_slot)
                        .expect("memory-hotplug placement should exist")
                        .sbdf
                }),
            );
        }
    }

    #[test]
    fn vsock_products_reject_presence_transport_config_and_manifest_substitution() {
        for bit in 0..6 {
            let (_platform, mut parts, _process, _image) = mmio_vsock_only_parts(false, 0);
            parts.kind = vsock_kind(0b100000 ^ (1 << bit));
            assert!(matches!(
                HvfSnapshotV2VsockPreparedProduct::try_from_parts(parts),
                Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Product),
            ));
        }

        let (_platform, mut parts, _process, _image) = mmio_vsock_only_parts(false, 0);
        parts.vsock = None;
        assert!(matches!(
            HvfSnapshotV2VsockPreparedProduct::try_from_parts(parts),
            Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Product),
        ));

        let (_platform, mut parts, _process, _state, _image) = mmio_network_vsock_parts(1);
        parts.kind = vsock_kind(0b100000);
        assert!(matches!(
            HvfSnapshotV2VsockPreparedProduct::try_from_parts(parts),
            Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Product),
        ));

        let (_platform, mut mmio, _process, _mmio_image) = mmio_vsock_only_parts(false, 0);
        let (_platform, mut pci, _msi, _pci_image) = pci_vsock_only_parts(0, 0, None);
        mmio.vsock = pci.vsock.take();
        assert!(matches!(
            HvfSnapshotV2VsockPreparedProduct::try_from_parts(mmio),
            Err(PrepareHvfSnapshotV2VsockPlatformPlanError::TransportPolicy),
        ));

        let (_platform, mut parts, _process, _image) = mmio_vsock_only_parts(false, 0);
        let endpoint = parts.vsock.take().expect("vsock endpoint should exist");
        let (state, config) = endpoint.into_parts();
        let wrong_config = VsockConfigInput::new(
            config.guest_cid().saturating_add(1),
            config.uds_path().to_string_lossy().into_owned(),
        )
        .validate()
        .expect("substituted vsock config should validate");
        parts.vsock = Some(HvfSnapshotV2VsockPreparedEndpoint::new(state, wrong_config));
        assert!(matches!(
            HvfSnapshotV2VsockPreparedProduct::try_from_parts(parts),
            Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Vsock),
        ));

        let (_platform, mut parts, _process, _image) = mmio_vsock_only_parts(false, 0);
        let key = &parts.binding_keys[0];
        parts.binding_keys[0] = SnapshotRestoreResourceKey::new(
            key.device_key(),
            SnapshotRestorePublicId::try_from(key.public_id().as_str())
                .expect("vsock public ID should copy"),
            SnapshotRestoreResourceClass::NetworkPacketIo,
        );
        assert!(matches!(
            HvfSnapshotV2VsockPreparedProduct::try_from_parts(parts),
            Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Manifest),
        ));

        let (_platform, mut parts, _process, _image) = mmio_vsock_only_parts(false, 0);
        let key = &parts.binding_keys[0];
        parts.binding_keys[0] = SnapshotRestoreResourceKey::new(
            key.device_key(),
            SnapshotRestorePublicId::try_from("not-vsock0")
                .expect("substituted public ID should validate"),
            key.resource_class(),
        );
        assert!(matches!(
            HvfSnapshotV2VsockPreparedProduct::try_from_parts(parts),
            Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Manifest),
        ));

        let (_platform, mut missing, _process, _state, _image) = mmio_network_vsock_parts(1);
        missing.binding_keys.pop();
        assert!(matches!(
            HvfSnapshotV2VsockPreparedProduct::try_from_parts(missing),
            Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Manifest),
        ));

        let (_platform, mut extra, _process, _state, _image) = mmio_network_vsock_parts(1);
        let duplicate = extra.binding_keys[1]
            .try_clone()
            .expect("resource key should copy");
        extra.binding_keys.push(duplicate);
        assert!(matches!(
            HvfSnapshotV2VsockPreparedProduct::try_from_parts(extra),
            Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Manifest),
        ));

        let (_platform, mut reordered, _process, _state, _image) = mmio_network_vsock_parts(1);
        reordered.binding_keys.swap(0, 1);
        assert!(matches!(
            HvfSnapshotV2VsockPreparedProduct::try_from_parts(reordered),
            Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Manifest),
        ));

        let (_platform, mut wrong_kind, _process, _state, _image) = mmio_network_vsock_parts(1);
        wrong_kind.binding_keys[1] = wrong_kind.binding_keys[0]
            .try_clone()
            .expect("network key should copy");
        assert!(matches!(
            HvfSnapshotV2VsockPreparedProduct::try_from_parts(wrong_kind),
            Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Manifest),
        ));
    }

    #[test]
    fn mmio_vsock_plans_close_active_inactive_placement_ranges_and_host_exclusion() {
        let (platform, parts, process, _image) = mmio_vsock_only_parts(true, 0);
        let socket_path = parts
            .vsock
            .as_ref()
            .expect("vsock endpoint should exist")
            .config()
            .uds_path()
            .to_path_buf();
        let existed_before = socket_path.exists();
        let product = HvfSnapshotV2VsockPreparedProduct::try_from_parts(parts)
            .expect("active MMIO product should validate");
        let plan = prepare_hvf_snapshot_v2_vsock_mmio_platform_plan(&platform, product, process)
            .expect("active MMIO vsock should plan");
        let endpoint = plan.vsock().expect("vsock endpoint should be present");
        assert_eq!(endpoint.region().range().start(), VSOCK_MMIO_BASE);
        assert_eq!(endpoint.region().id(), VSOCK_MMIO_REGION_ID);
        assert_eq!(endpoint.dispatcher_region_id(), VSOCK_MMIO_REGION_ID);
        assert_eq!(
            endpoint.fdt_device().region.base,
            VSOCK_MMIO_BASE.raw_value()
        );
        assert_eq!(
            endpoint.fdt_device().region.size,
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE
        );
        for (index, ranges) in endpoint.queue_ranges().iter().enumerate() {
            let base = VSOCK_QUEUE_BASE
                + u64::try_from(index).expect("queue index should fit") * NETWORK_QUEUE_STRIDE;
            assert_eq!(
                *ranges,
                Some([
                    GuestMemoryRange::new(GuestAddress::new(base), 4096)
                        .expect("descriptor range should validate"),
                    GuestMemoryRange::new(GuestAddress::new(base + DRIVER_RING_OFFSET), 518)
                        .expect("available range should validate"),
                    GuestMemoryRange::new(GuestAddress::new(base + DEVICE_RING_OFFSET), 2054)
                        .expect("used range should validate"),
                ])
            );
        }
        assert_eq!(socket_path.exists(), existed_before);

        let (platform, parts, process, _image) = mmio_vsock_only_parts(false, 0);
        let product = HvfSnapshotV2VsockPreparedProduct::try_from_parts(parts)
            .expect("inactive MMIO product should validate");
        let plan = prepare_hvf_snapshot_v2_vsock_mmio_platform_plan(&platform, product, process)
            .expect("inactive MMIO vsock should plan");
        assert!(
            plan.vsock()
                .expect("inactive vsock endpoint should be present")
                .queue_ranges()
                .iter()
                .all(Option::is_none)
        );

        let (platform, parts, process, _image) = mmio_vsock_only_parts(false, 0);
        let wrong_process = HvfSnapshotV2VsockMmioProcessConfig::new(
            process.network(),
            VsockMmioLayout::new(
                GuestAddress::new(
                    VSOCK_MMIO_BASE
                        .raw_value()
                        .checked_add(VIRTIO_MMIO_DEVICE_WINDOW_SIZE)
                        .expect("wrong placement should fit"),
                ),
                VSOCK_MMIO_REGION_ID,
            ),
        );
        let product = HvfSnapshotV2VsockPreparedProduct::try_from_parts(parts)
            .expect("misplaced process product should validate");
        assert!(matches!(
            prepare_hvf_snapshot_v2_vsock_mmio_platform_plan(&platform, product, wrong_process,),
            Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Placement),
        ));

        let (platform, parts, process, _image) = mmio_vsock_only_parts(false, 1);
        let product = HvfSnapshotV2VsockPreparedProduct::try_from_parts(parts)
            .expect("wrong-interrupt product should validate locally");
        let error = prepare_hvf_snapshot_v2_vsock_mmio_platform_plan(&platform, product, process)
            .expect_err("wrong MMIO interrupt should fail");
        let detail = match &error {
            PrepareHvfSnapshotV2VsockPlatformPlanError::Network(source) => {
                format!("{source:?}")
            }
            _ => format!("{error:?}"),
        };
        assert!(
            matches!(
                error,
                PrepareHvfSnapshotV2VsockPlatformPlanError::Network(source)
                    if matches!(
                        *source,
                        PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan
                    )
            ),
            "unexpected wrong-interrupt failure: {detail}",
        );
    }

    #[test]
    fn mmio_vsock_suffix_queue_conflicts_share_the_aggregate_inventory_gate() {
        let MaterializedFixture {
            platform,
            topology,
            memory: _,
            _image,
        } = materialized_fixture(memory_hotplug_mmio_state(34));
        let platform = memory_fixture_mmio_platform(platform, topology.state(), 2);
        let first_interrupt = platform
            .global()
            .compatibility()
            .gic_metadata()
            .spi_interrupt_range
            .base;
        let network = prepare_topology(network_state(
            SnapshotV2DeviceTransportKind::Mmio,
            1,
            true,
            MmdsSelection::None,
            first_interrupt,
            None,
        ));
        let duplicate_queue = interface_queue_ranges(&network.interfaces()[0])
            .expect("network queue ranges should project")[0];
        assert!(duplicate_queue.is_some());
        let product =
            HvfSnapshotV2NetworkPreparedProduct::serial_network(platform.memory().clone(), network);
        let following = HvfSnapshotV2NetworkMmioFollowingEndpointInput {
            region: MmioRegion::new(
                VSOCK_MMIO_REGION_ID,
                VSOCK_MMIO_BASE,
                VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
            )
            .expect("vsock region should validate"),
            interrupt_line: GuestInterruptLine::new(
                first_interrupt
                    .checked_add(1)
                    .expect("vsock interrupt should fit"),
            )
            .expect("vsock interrupt should validate"),
            queue_ranges: [duplicate_queue, None, None],
        };
        assert!(matches!(
            prepare_network_mmio_platform_plan(
                &platform,
                product,
                mmio_process_config(),
                Some(following),
                &mut SystemNetworkPlatformPlanReserve,
                &mut |_| false,
            ),
            Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::RangeConflict),
        ));
    }

    #[test]
    fn pci_vsock_plans_reject_wrong_slot_capacity_and_cross_family_routes() {
        let (platform, parts, msi, _image) = pci_vsock_only_parts(0, 0, None);
        let product = HvfSnapshotV2VsockPreparedProduct::try_from_parts(parts)
            .expect("PCI vsock product should validate");
        let plan = prepare_hvf_snapshot_v2_vsock_pci_platform_plan(&platform, product)
            .expect("PCI vsock should plan");
        let endpoint = plan.vsock().expect("PCI vsock endpoint should be present");
        assert_eq!(endpoint.origin(), StorageDeviceOrigin::Startup);
        assert_eq!(endpoint.queue_vectors(), &[1, 2, 3]);
        assert_eq!(endpoint.config_vector(), 0);
        assert_eq!(endpoint.route_count(), VIRTIO_VSOCK_QUEUE_COUNT + 1);
        assert_eq!(endpoint.msi_interrupt_count(), msi.interrupt_range.count);

        let (platform, parts, _msi, _image) = pci_vsock_only_parts(1, 0, None);
        let product = HvfSnapshotV2VsockPreparedProduct::try_from_parts(parts)
            .expect("misplaced PCI product should validate locally");
        assert!(matches!(
            prepare_hvf_snapshot_v2_vsock_pci_platform_plan(&platform, product),
            Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Network(source))
                if matches!(
                    *source,
                    PrepareHvfSnapshotV2NetworkPlatformPlanError::Placement
                )
        ));

        let expected_interrupt_count = pci_vsock_restore_gic_msi_configuration(None, false, false)
            .expect("vsock MSI configuration should validate")
            .interrupt_count()
            .get();
        let (platform, parts, _msi, _image) = pci_vsock_only_parts(
            0,
            0,
            Some(
                expected_interrupt_count
                    .checked_sub(1)
                    .expect("wrong MSI capacity should remain nonzero"),
            ),
        );
        let product = HvfSnapshotV2VsockPreparedProduct::try_from_parts(parts)
            .expect("wrong-capacity product should validate locally");
        assert!(matches!(
            prepare_hvf_snapshot_v2_vsock_pci_platform_plan(&platform, product),
            Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Network(source))
                if matches!(
                    *source,
                    PrepareHvfSnapshotV2NetworkPlatformPlanError::ResourcePlan
                )
        ));

        let (platform, parts, _msi, _state, _image) = pci_network_vsock_parts(1, true);
        let product = HvfSnapshotV2VsockPreparedProduct::try_from_parts(parts)
            .expect("duplicate-route product should validate locally");
        assert!(matches!(
            prepare_hvf_snapshot_v2_vsock_pci_platform_plan(&platform, product),
            Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Network(source))
                if matches!(
                    *source,
                    PrepareHvfSnapshotV2NetworkPlatformPlanError::RouteConflict
                )
        ));

        let (platform, parts, _msi, _state, _image) = pci_network_vsock_parts(16, false);
        let product = HvfSnapshotV2VsockPreparedProduct::try_from_parts(parts)
            .expect("maximum-network PCI product should validate");
        let plan = prepare_hvf_snapshot_v2_vsock_pci_platform_plan(&platform, product)
            .expect("maximum-network PCI vsock product should plan");
        assert_eq!(plan.network().len(), 16);
        assert_eq!(plan.endpoint_count(), 17);
        assert_eq!(
            plan.route_demand(),
            16 * (VIRTIO_NET_QUEUE_COUNT + 1) + VIRTIO_VSOCK_QUEUE_COUNT + 1
        );
    }

    #[test]
    fn exact_pci_endpoint_capacity_accepts_thirty_one_and_rejects_thirty_two() {
        assert_eq!(PCI_ENDPOINT_SLOT_COUNT, 31);
        assert!(validate_pci_endpoint_capacity(PCI_ENDPOINT_SLOT_COUNT).is_ok());
        assert!(matches!(
            validate_pci_endpoint_capacity(
                PCI_ENDPOINT_SLOT_COUNT
                    .checked_add(1)
                    .expect("one-over endpoint count should fit"),
            ),
            Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::PciCapacity {
                count,
                maximum,
            }) if count == 32 && maximum == 31
        ));
    }

    #[test]
    fn every_vsock_cancellation_stage_is_stable_and_redacted() {
        for cancelled in [
            HvfSnapshotV2VsockPlatformPlanStage::Start,
            HvfSnapshotV2VsockPlatformPlanStage::Product,
            HvfSnapshotV2VsockPlatformPlanStage::Interface,
            HvfSnapshotV2VsockPlatformPlanStage::Vsock,
            HvfSnapshotV2VsockPlatformPlanStage::Components,
            HvfSnapshotV2VsockPlatformPlanStage::Inventory,
            HvfSnapshotV2VsockPlatformPlanStage::Completion,
        ] {
            let (platform, parts, process, _state, _image) = mmio_network_vsock_parts(1);
            let product = HvfSnapshotV2VsockPreparedProduct::try_from_parts(parts)
                .expect("cancellation product should validate");
            let error = prepare_hvf_snapshot_v2_vsock_mmio_platform_plan_with_cancel(
                &platform,
                product,
                process,
                |stage| stage == cancelled,
            )
            .expect_err("injected exact-2.12 cancellation should fail");
            assert!(matches!(
                error,
                PrepareHvfSnapshotV2VsockPlatformPlanError::Cancelled { stage }
                    if stage == cancelled
            ));
            let diagnostics = format!("{error:?} {error}");
            assert!(diagnostics.contains(REDACTED));
            assert!(!diagnostics.contains("vmnet:shared"));
        }
    }

    #[test]
    fn vsock_process_preflight_closes_network_endpoint_and_complete_manifest_identity() {
        let (platform, parts, process, state, _image) = mmio_network_vsock_parts(1);
        let socket_path = parts
            .vsock
            .as_ref()
            .expect("vsock endpoint should exist")
            .config()
            .uds_path()
            .to_string_lossy()
            .into_owned();
        let product = HvfSnapshotV2VsockPreparedProduct::try_from_parts(parts)
            .expect("preflight product should validate");
        let plan = prepare_hvf_snapshot_v2_vsock_mmio_platform_plan(&platform, product, process)
            .expect("preflight product should plan");
        let process_topology = prepare_topology(state);
        let identities = || {
            process_topology.interfaces().iter().map(|interface| {
                HvfSnapshotV2NetworkProcessResourceIdentity::new(
                    interface.source_index(),
                    interface.resource_key(),
                    interface.controller(),
                    interface.portable().profile(),
                    interface.portable().backend(),
                    interface.mmds_stack(),
                )
            })
        };
        let endpoint = plan.vsock().expect("vsock endpoint should be present");
        let vsock_identity = HvfSnapshotV2VsockProcessResourceIdentity::new(
            endpoint.resource_key(),
            endpoint.config(),
        );
        assert!(plan.preflight_process_resource_identity(
            identities(),
            process_topology.mmds_state(),
            process_topology.mmds_controller(),
            Some(vsock_identity),
            plan.resource_keys(),
        ));
        assert!(!plan.preflight_process_resource_identity(
            identities(),
            process_topology.mmds_state(),
            process_topology.mmds_controller(),
            None,
            plan.resource_keys(),
        ));
        assert!(!plan.preflight_process_resource_identity(
            identities(),
            process_topology.mmds_state(),
            process_topology.mmds_controller(),
            Some(vsock_identity),
            &[],
        ));
        let wrong_order = process_topology.interfaces().iter().map(|interface| {
            HvfSnapshotV2NetworkProcessResourceIdentity::new(
                interface.source_index().saturating_add(1),
                interface.resource_key(),
                interface.controller(),
                interface.portable().profile(),
                interface.portable().backend(),
                interface.mmds_stack(),
            )
        });
        assert!(!plan.preflight_process_resource_identity(
            wrong_order,
            process_topology.mmds_state(),
            process_topology.mmds_controller(),
            Some(vsock_identity),
            plan.resource_keys(),
        ));

        let plan_debug = format!("{plan:?}");
        assert!(plan_debug.contains(REDACTED));
        assert!(!plan_debug.contains(&socket_path));
        let error = PrepareHvfSnapshotV2VsockPlatformPlanError::Network(Box::new(
            PrepareHvfSnapshotV2NetworkPlatformPlanError::RangeConflict,
        ));
        let error_debug = format!("{error:?}");
        let error_display = error.to_string();
        let source_display = std::error::Error::source(&error)
            .expect("nested network source should exist")
            .to_string();
        assert!(error_debug.contains(REDACTED));
        assert!(!error_debug.contains(&socket_path));
        assert!(!error_display.contains(&socket_path));
        assert!(!source_display.contains(&socket_path));
    }

    #[test]
    fn placement_and_cross_interface_route_mismatches_fail_before_ownership() {
        let (platform, misplaced) = pci_product(1, true, MmdsSelection::All, false, 1);
        assert!(matches!(
            prepare_hvf_snapshot_v2_network_pci_platform_plan(&platform, misplaced),
            Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::Placement),
        ));

        let (platform, duplicate_routes) = pci_product(2, true, MmdsSelection::Subset, true, 0);
        assert!(matches!(
            prepare_hvf_snapshot_v2_network_pci_platform_plan(&platform, duplicate_routes),
            Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::RouteConflict),
        ));
    }

    #[test]
    fn untracked_full_snapshot_profile_is_accepted() {
        let (untracked, _) = mmio_product(1, false, MmdsSelection::None);
        let (memory, machine, global, topology, vcpus, time) = untracked.into_parts();
        let machine = HvfSnapshotV2MachineState::try_new(
            machine.machine().with_track_dirty_pages(false),
            machine.boot().clone(),
            machine.fdt(),
            machine.cpu_template().cloned(),
        )
        .expect("untracked machine should validate");
        let untracked =
            HvfSnapshotV2PlatformState::try_new(memory, machine, global, topology, vcpus, time)
                .expect("untracked platform should validate");
        let (_, product) = mmio_product(1, false, MmdsSelection::None);
        assert!(
            prepare_hvf_snapshot_v2_network_mmio_platform_plan(
                &untracked,
                product,
                mmio_process_config(),
            )
            .is_ok()
        );
    }

    #[test]
    fn every_stable_cancellation_stage_is_observable_and_redacted() {
        for stage in [
            HvfSnapshotV2NetworkPlatformPlanStage::Start,
            HvfSnapshotV2NetworkPlatformPlanStage::Product,
            HvfSnapshotV2NetworkPlatformPlanStage::Interface,
            HvfSnapshotV2NetworkPlatformPlanStage::Components,
            HvfSnapshotV2NetworkPlatformPlanStage::Inventory,
            HvfSnapshotV2NetworkPlatformPlanStage::Completion,
        ] {
            let (platform, product) = mmio_product(1, false, MmdsSelection::None);
            let error = prepare_hvf_snapshot_v2_network_mmio_platform_plan_with_cancel(
                &platform,
                product,
                mmio_process_config(),
                |candidate| candidate == stage,
            )
            .expect_err("injected cancellation should fail");
            assert!(matches!(
                error,
                PrepareHvfSnapshotV2NetworkPlatformPlanError::Cancelled {
                    stage: actual,
                } if actual == stage
            ));
            let diagnostics = format!("{error:?} {error}");
            assert!(diagnostics.contains(REDACTED));
            assert!(!diagnostics.contains("eth0"));
            assert!(!diagnostics.contains("vmnet:shared"));
        }
    }

    struct FailingReserve {
        calls: usize,
        fail_at: usize,
    }

    impl FailingReserve {
        fn should_fail(&mut self) -> bool {
            let call = self.calls;
            self.calls = self.calls.saturating_add(1);
            call == self.fail_at
        }
    }

    impl NetworkPlatformPlanReserve for FailingReserve {
        fn reserve<T>(
            &mut self,
            values: &mut Vec<T>,
            additional: usize,
        ) -> Result<(), PrepareHvfSnapshotV2NetworkPlatformPlanError> {
            if self.should_fail() {
                Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::Allocation)
            } else {
                values
                    .try_reserve_exact(additional)
                    .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::Allocation)
            }
        }

        fn clone_key(
            &mut self,
            key: &SnapshotRestoreResourceKey,
        ) -> Result<SnapshotRestoreResourceKey, PrepareHvfSnapshotV2NetworkPlatformPlanError>
        {
            if self.should_fail() {
                Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::Allocation)
            } else {
                key.try_clone()
                    .map_err(|_| PrepareHvfSnapshotV2NetworkPlatformPlanError::Allocation)
            }
        }
    }

    #[test]
    fn every_network_identity_and_inventory_allocation_failure_is_explicit() {
        for fail_at in 0..8 {
            let (platform, product) = mmio_product(1, true, MmdsSelection::All);
            assert!(matches!(
                prepare_network_mmio_platform_plan(
                    &platform,
                    product,
                    mmio_process_config(),
                    None,
                    &mut FailingReserve { calls: 0, fail_at },
                    &mut |_| false,
                ),
                Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::Allocation),
            ));
        }
        for fail_at in 0..7 {
            let (platform, product) = pci_product(1, true, MmdsSelection::All, false, 0);
            assert!(matches!(
                prepare_network_pci_platform_plan(
                    &platform,
                    product,
                    None,
                    &mut FailingReserve { calls: 0, fail_at },
                    &mut |_| false,
                ),
                Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::Allocation),
            ));
        }
    }

    #[test]
    fn every_exact_vsock_key_copy_and_inventory_allocation_failure_is_explicit() {
        let mut mmio_failure_count = 0;
        for fail_at in 0..32 {
            let (platform, parts, process, _state, _image) = mmio_network_vsock_parts(1);
            let product = HvfSnapshotV2VsockPreparedProduct::try_from_parts(parts)
                .expect("MMIO allocation product should validate");
            match prepare_vsock_mmio_platform_plan(
                &platform,
                product,
                process,
                &mut FailingReserve { calls: 0, fail_at },
                &mut |_| false,
            ) {
                Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Allocation) => {
                    mmio_failure_count += 1;
                }
                Ok(_) => {
                    assert_eq!(fail_at, mmio_failure_count);
                    break;
                }
                Err(error) => panic!("unexpected MMIO allocation failure: {error:?}"),
            }
        }
        assert!(mmio_failure_count > 1, "key copy and inventories must fail");

        let mut pci_failure_count = 0;
        for fail_at in 0..32 {
            let (platform, parts, _msi, _state, _image) = pci_network_vsock_parts(1, false);
            let product = HvfSnapshotV2VsockPreparedProduct::try_from_parts(parts)
                .expect("PCI allocation product should validate");
            match prepare_vsock_pci_platform_plan(
                &platform,
                product,
                &mut FailingReserve { calls: 0, fail_at },
                &mut |_| false,
            ) {
                Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Allocation) => {
                    pci_failure_count += 1;
                }
                Ok(_) => {
                    assert_eq!(fail_at, pci_failure_count);
                    break;
                }
                Err(error) => panic!("unexpected PCI allocation failure: {error:?}"),
            }
        }
        assert!(pci_failure_count > 1, "key copy and inventories must fail");
    }

    #[test]
    fn product_and_error_debug_are_bounded_and_redacted() {
        let (_, product) = mmio_product(1, false, MmdsSelection::None);
        let product_debug = format!("{product:?}");
        assert!(product_debug.contains(REDACTED));
        assert!(!product_debug.contains("eth0"));
        assert!(!product_debug.contains("vmnet:shared"));

        for error in [
            PrepareHvfSnapshotV2NetworkPlatformPlanError::PciCapacity {
                count: 32,
                maximum: 31,
            },
            PrepareHvfSnapshotV2NetworkPlatformPlanError::RangeConflict,
            PrepareHvfSnapshotV2NetworkPlatformPlanError::RouteConflict,
        ] {
            let diagnostics = format!("{error:?} {error}");
            assert!(diagnostics.contains(REDACTED));
            assert!(!diagnostics.contains("32"));
            assert!(!diagnostics.contains("31"));
        }
    }
}

#[cfg(test)]
mod tests {
    use bangbang_runtime::balloon::BalloonMmioLayout;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::fdt::ARM64_GICV2M_MSI_SET_SPI_NSR_OFFSET;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::pci::PCI_FIRST_ENDPOINT_DEVICE;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::snapshot::SnapshotNetworkOverride;
    use bangbang_runtime::snapshot_device::SnapshotV1PlatformDeviceMetadata;
    use bangbang_runtime::snapshot_device_v2::{
        SnapshotV2DeviceTransport, SnapshotV2DeviceTransportKind,
    };
    use bangbang_runtime::snapshot_network_restore_v2_11::PreparedSnapshotV2NetworkRestoreTopology;
    use bangbang_runtime::snapshot_network_v2_11::SnapshotV2NetworkState;

    use super::*;
    use crate::gic::{HvfGicInterruptRange, HvfGicRegion};
    use crate::snapshot_bundle::HvfSnapshotV1CompatibilityState;
    use crate::snapshot_v2::{
        HvfSnapshotV2GlobalState, HvfSnapshotV2MachineState, HvfSnapshotV2NetworkPlatformState,
        HvfSnapshotV2TimeState,
        tests::{
            MMIO_GRAPH_FIXTURE_HEX, complete_network_state_fixture, complete_state_fixture,
            product_network_fixture, product_storage_fixture,
        },
    };
    use crate::snapshot_v2_memory_hotplug_platform::tests::prepared_storage_bundle;

    const BALLOON_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_8000);
    const BALLOON_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(4_000);
    const BLOCK_MMIO_BASE: GuestAddress = GuestAddress::new(0x5000_0000);
    const BLOCK_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(1);
    const PMEM_MMIO_BASE: GuestAddress = GuestAddress::new(0x5800_0000);
    const PMEM_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(500);
    const ENTROPY_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_7000);
    const ENTROPY_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(3_000);
    const MEMORY_HOTPLUG_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_9000);
    const MEMORY_HOTPLUG_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(5_000);

    fn network_topology(state: SnapshotV2NetworkState) -> PreparedSnapshotV2NetworkRestoreTopology {
        let overrides = state
            .interfaces()
            .iter()
            .map(|interface| SnapshotNetworkOverride::new(interface.iface_id(), "vmnet:shared"))
            .collect::<Vec<_>>();
        PreparedSnapshotV2NetworkRestoreTopology::prepare(state, &overrides)
            .expect("network topology should prepare")
    }

    fn network_fixture(
        transport: SnapshotV2DeviceTransportKind,
    ) -> (HvfSnapshotV2PlatformState, SnapshotV2NetworkState) {
        let platform = complete_network_state_fixture(transport, false, false, false, false, false)
            .into_parts()
            .0;
        let state = product_network_fixture(transport, PCI_FIRST_ENDPOINT_DEVICE);
        let platform = platform_with_network_interrupts(platform, &state);
        assert!(
            platform.machine().fdt().is_product_process_profile(),
            "network fixture must retain the process FDT profile",
        );
        assert_eq!(
            platform.time().rtc_layout(),
            RtcMmioLayout::new(PROCESS_RTC_MMIO_BASE, PROCESS_RTC_MMIO_REGION_ID),
        );
        (platform, state)
    }

    fn platform_with_network_interrupts(
        platform: HvfSnapshotV2PlatformState,
        network: &SnapshotV2NetworkState,
    ) -> HvfSnapshotV2PlatformState {
        let (memory, machine, global, topology, vcpus, time) = platform.into_parts();
        let machine = HvfSnapshotV2MachineState::try_new(
            machine.machine().with_track_dirty_pages(true),
            machine.boot().clone(),
            machine.fdt(),
            machine.cpu_template().cloned(),
        )
        .expect("tracked network machine should validate");
        let (compatibility, gic_device) = global.into_parts();
        let mut gic = compatibility.gic_metadata();
        match network
            .interfaces()
            .first()
            .expect("network fixture should contain an interface")
            .transport()
        {
            SnapshotV2DeviceTransport::Mmio(first) => {
                gic.spi_interrupt_range.base = first.interrupt_line().raw_value();
                gic.spi_interrupt_range.count = u32::try_from(network.interfaces().len() + 3)
                    .expect("MMIO interrupt count should fit");
                gic.msi = None;
            }
            SnapshotV2DeviceTransport::Pci(first) => {
                let entry = first
                    .msix()
                    .entries()
                    .first()
                    .expect("PCI network fixture should retain an MSI-X entry");
                let message_address = (u64::from(entry.message_address_high()) << 32)
                    | u64::from(entry.message_address_low());
                let msi_base = message_address
                    .checked_sub(ARM64_GICV2M_MSI_SET_SPI_NSR_OFFSET)
                    .expect("MSI region base should fit");
                let interrupt_base = network
                    .interfaces()
                    .iter()
                    .flat_map(|interface| match interface.transport() {
                        SnapshotV2DeviceTransport::Pci(transport) => transport.msix().entries(),
                        SnapshotV2DeviceTransport::Mmio(_) => &[],
                    })
                    .map(|entry| entry.message_data())
                    .min()
                    .expect("PCI network fixture should retain route data");
                gic.spi_interrupt_range = HvfGicInterruptRange {
                    base: interrupt_base
                        .checked_sub(3)
                        .expect("legacy interrupt base should fit"),
                    count: 3,
                };
                gic.msi = Some(HvfGicMsiMetadata {
                    region: HvfGicRegion {
                        base: msi_base,
                        size: 0x1_0000,
                    },
                    interrupt_range: HvfGicInterruptRange {
                        base: interrupt_base,
                        count: pci_root_restore_gic_msi_configuration()
                            .expect("root MSI configuration should validate")
                            .interrupt_count()
                            .get(),
                    },
                });
            }
        }
        let compatibility = HvfSnapshotV1CompatibilityState::new(
            compatibility.identification(),
            compatibility.optional_sve_sme_identification(),
            compatibility.cache_manifest(),
            compatibility.primary_mpidr(),
            gic,
            RtcMmioLayout::new(PROCESS_RTC_MMIO_BASE, PROCESS_RTC_MMIO_REGION_ID),
        );
        let global = HvfSnapshotV2GlobalState::try_new(compatibility, gic_device)
            .expect("network global state should validate");

        let mut allocator = HvfGicInterruptLineAllocator::from_metadata(&gic)
            .expect("network interrupt allocator should validate");
        if gic.msi.is_none() {
            for _ in network.interfaces() {
                allocator
                    .allocate()
                    .expect("network MMIO interrupt should allocate");
            }
        }
        let _serial = allocator
            .allocate()
            .expect("serial interrupt should allocate");
        let vmgenid_interrupt = allocator
            .allocate()
            .expect("VMGenID interrupt should allocate");
        let vmclock_interrupt = allocator
            .allocate()
            .expect("VMClock interrupt should allocate");
        let (_rtc, vmgenid, vmclock, vmclock_abi, pvtime) = time.into_parts();
        let vmgenid = SnapshotV1PlatformDeviceMetadata::new(
            vmgenid.range(),
            vmgenid.fdt_region(),
            vmgenid_interrupt,
        );
        let vmclock = SnapshotV1PlatformDeviceMetadata::new(
            vmclock.range(),
            vmclock.fdt_region(),
            vmclock_interrupt,
        );
        let time = HvfSnapshotV2TimeState::try_new(
            RtcMmioLayout::new(PROCESS_RTC_MMIO_BASE, PROCESS_RTC_MMIO_REGION_ID),
            vmgenid,
            vmclock,
            vmclock_abi,
            pvtime,
        )
        .expect("network time state should validate");
        HvfSnapshotV2NetworkPlatformState::try_new(
            memory, machine, global, topology, vcpus, time, None,
        )
        .expect("network platform should validate")
        .platform()
        .clone()
    }

    fn mmio_process(network: &SnapshotV2NetworkState) -> HvfSnapshotV2NetworkMmioProcessConfig {
        let SnapshotV2DeviceTransport::Mmio(first) = network.interfaces()[0].transport() else {
            panic!("MMIO process requires an MMIO network fixture");
        };
        HvfSnapshotV2NetworkMmioProcessConfig::new(
            BalloonMmioLayout::new(BALLOON_MMIO_BASE, BALLOON_MMIO_REGION_ID),
            HvfSnapshotV2StorageMmioProcessConfig::new(
                BlockMmioLayout::new(BLOCK_MMIO_BASE, BLOCK_MMIO_REGION_ID),
                PmemMmioLayout::new(PMEM_MMIO_BASE, PMEM_MMIO_REGION_ID),
            ),
            NetworkMmioLayout::new(first.region().range().start(), first.region().id()),
            EntropyMmioLayout::new(ENTROPY_MMIO_BASE, ENTROPY_MMIO_REGION_ID),
            VirtioMemMmioLayout::new(MEMORY_HOTPLUG_MMIO_BASE, MEMORY_HOTPLUG_MMIO_REGION_ID),
        )
    }

    #[test]
    fn network_only_mmio_plan_retains_exact_identity_and_fixed_interrupts() {
        let (platform, state) = network_fixture(SnapshotV2DeviceTransportKind::Mmio);
        let process = mmio_process(&state);
        let expected = state.clone();
        let product = HvfSnapshotV2NetworkPreparedProduct::serial_network(
            platform.memory().clone(),
            network_topology(state),
        );

        let plan = prepare_hvf_snapshot_v2_network_mmio_platform_plan(&platform, product, process)
            .expect("network-only MMIO product should plan");

        assert_eq!(plan.kind(), HvfSnapshotV2NetworkProductKind::SerialNetwork);
        assert_eq!(
            plan.product().interface_count(),
            expected.interfaces().len()
        );
        assert_eq!(plan.product().has_mmds(), expected.mmds().is_some());
        assert!(!plan.product().has_storage());
        assert!(!plan.product().has_entropy());
        assert!(!plan.product().has_balloon());
        assert!(!plan.product().has_memory_hotplug());
        assert_eq!(plan.network().len(), expected.interfaces().len());
        for (index, (endpoint, interface)) in
            plan.network().iter().zip(expected.interfaces()).enumerate()
        {
            let SnapshotV2DeviceTransport::Mmio(transport) = interface.transport() else {
                panic!("expected an MMIO network interface");
            };
            assert_eq!(endpoint.source_index(), u16::try_from(index).unwrap());
            assert_eq!(
                endpoint.resource_key().public_id().as_str(),
                interface.iface_id(),
            );
            assert_eq!(endpoint.region(), transport.region());
            assert_eq!(endpoint.dispatcher_region_id(), transport.region().id());
            assert_eq!(endpoint.interrupt_line(), transport.interrupt_line());
            assert_eq!(
                endpoint.fdt_device().region.base,
                transport.region().range().start().raw_value()
            );
            assert_eq!(
                endpoint.fdt_device().interrupt_line,
                transport.interrupt_line(),
            );
        }
        assert_eq!(
            plan.vmgenid_interrupt(),
            platform.time().vmgenid().interrupt_line(),
        );
        assert_eq!(
            plan.vmclock_interrupt(),
            platform.time().vmclock().interrupt_line(),
        );
    }

    #[test]
    fn mmio_process_resource_preflight_closes_saved_order_identity() {
        let (platform, state) = network_fixture(SnapshotV2DeviceTransportKind::Mmio);
        let process = mmio_process(&state);
        let process_topology = network_topology(state.clone());
        let product = HvfSnapshotV2NetworkPreparedProduct::serial_network(
            platform.memory().clone(),
            network_topology(state),
        );
        let plan = prepare_hvf_snapshot_v2_network_mmio_platform_plan(&platform, product, process)
            .expect("network-only MMIO product should plan");

        let identities = process_topology.interfaces().iter().map(|interface| {
            HvfSnapshotV2NetworkProcessResourceIdentity::new(
                interface.source_index(),
                interface.resource_key(),
                interface.controller(),
                interface.portable().profile(),
                interface.portable().backend(),
                interface.mmds_stack(),
            )
        });
        assert!(plan.preflight_process_resource_identity(
            identities,
            process_topology.mmds_state(),
            process_topology.mmds_controller(),
        ));

        let wrong_order = process_topology.interfaces().iter().map(|interface| {
            HvfSnapshotV2NetworkProcessResourceIdentity::new(
                interface.source_index().saturating_add(1),
                interface.resource_key(),
                interface.controller(),
                interface.portable().profile(),
                interface.portable().backend(),
                interface.mmds_stack(),
            )
        });
        assert!(!plan.preflight_process_resource_identity(
            wrong_order,
            process_topology.mmds_state(),
            process_topology.mmds_controller(),
        ));
    }

    #[test]
    fn binding_and_component_transport_mismatches_fail_before_planning() {
        let (platform, state) = network_fixture(SnapshotV2DeviceTransportKind::Mmio);
        let process = mmio_process(&state);
        let foreign_binding = complete_state_fixture(MMIO_GRAPH_FIXTURE_HEX)
            .platform()
            .memory()
            .clone();
        assert_ne!(&foreign_binding, platform.memory());
        let product = HvfSnapshotV2NetworkPreparedProduct::serial_network(
            foreign_binding,
            network_topology(state),
        );
        assert!(matches!(
            prepare_hvf_snapshot_v2_network_mmio_platform_plan(&platform, product, process),
            Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::Binding),
        ));

        let (platform, state) = network_fixture(SnapshotV2DeviceTransportKind::Mmio);
        let process = mmio_process(&state);
        let (storage, _backings) =
            prepared_storage_bundle(product_storage_fixture(SnapshotV2DeviceTransportKind::Pci));
        let product = HvfSnapshotV2NetworkPreparedProduct::serial_storage_network(
            platform.memory().clone(),
            network_topology(state),
            storage,
        );
        assert!(matches!(
            prepare_hvf_snapshot_v2_network_mmio_platform_plan(&platform, product, process),
            Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::TransportPolicy),
        ));
    }

    #[test]
    fn network_only_pci_plan_retains_vectors_routes_and_dispatcher_identity() {
        let (platform, state) = network_fixture(SnapshotV2DeviceTransportKind::Pci);
        let expected = state.clone();
        let product = HvfSnapshotV2NetworkPreparedProduct::serial_network(
            platform.memory().clone(),
            network_topology(state),
        );

        let plan = prepare_hvf_snapshot_v2_network_pci_platform_plan(&platform, product)
            .expect("network-only PCI product should plan");

        assert_eq!(plan.kind(), HvfSnapshotV2NetworkProductKind::SerialNetwork);
        assert_eq!(plan.endpoint_count(), expected.interfaces().len());
        assert_eq!(
            plan.route_demand(),
            expected.interfaces().len() * (VIRTIO_NET_QUEUE_COUNT + 1),
        );
        for (index, (endpoint, interface)) in
            plan.network().iter().zip(expected.interfaces()).enumerate()
        {
            let SnapshotV2DeviceTransport::Pci(transport) = interface.transport() else {
                panic!("expected a PCI network interface");
            };
            assert_eq!(endpoint.source_index(), u16::try_from(index).unwrap());
            assert_eq!(endpoint.sbdf(), transport.sbdf());
            assert_eq!(endpoint.bar_range(), transport.bar_range());
            assert_eq!(endpoint.dispatcher_region_id(), endpoint.bar_region_id());
            assert_eq!(endpoint.queue_vectors(), transport.msix().queue_vectors());
            assert_eq!(endpoint.config_vector(), transport.msix().config_vector());
            assert_eq!(endpoint.route_count(), VIRTIO_NET_QUEUE_COUNT + 1);
            assert_eq!(
                endpoint.msi_interrupt_count(),
                plan.msi().interrupt_range.count,
            );
        }
    }

    #[test]
    fn pci_process_resource_preflight_closes_saved_order_identity() {
        let (platform, state) = network_fixture(SnapshotV2DeviceTransportKind::Pci);
        let process_topology = network_topology(state.clone());
        let product = HvfSnapshotV2NetworkPreparedProduct::serial_network(
            platform.memory().clone(),
            network_topology(state),
        );
        let plan = prepare_hvf_snapshot_v2_network_pci_platform_plan(&platform, product)
            .expect("network-only PCI product should plan");

        let identities = process_topology.interfaces().iter().map(|interface| {
            HvfSnapshotV2NetworkProcessResourceIdentity::new(
                interface.source_index(),
                interface.resource_key(),
                interface.controller(),
                interface.portable().profile(),
                interface.portable().backend(),
                interface.mmds_stack(),
            )
        });
        assert!(plan.preflight_process_resource_identity(
            identities,
            process_topology.mmds_state(),
            process_topology.mmds_controller(),
        ));

        let wrong_order = process_topology.interfaces().iter().map(|interface| {
            HvfSnapshotV2NetworkProcessResourceIdentity::new(
                interface.source_index().saturating_add(1),
                interface.resource_key(),
                interface.controller(),
                interface.portable().profile(),
                interface.portable().backend(),
                interface.mmds_stack(),
            )
        });
        assert!(!plan.preflight_process_resource_identity(
            wrong_order,
            process_topology.mmds_state(),
            process_topology.mmds_controller(),
        ));
    }

    #[test]
    fn cancellation_is_observed_at_every_stable_stage() {
        let stages = [
            HvfSnapshotV2NetworkPlatformPlanStage::Start,
            HvfSnapshotV2NetworkPlatformPlanStage::Product,
            HvfSnapshotV2NetworkPlatformPlanStage::Interface,
            HvfSnapshotV2NetworkPlatformPlanStage::Components,
            HvfSnapshotV2NetworkPlatformPlanStage::Inventory,
            HvfSnapshotV2NetworkPlatformPlanStage::Completion,
        ];
        for cancelled in stages {
            let (platform, state) = network_fixture(SnapshotV2DeviceTransportKind::Mmio);
            let process = mmio_process(&state);
            let product = HvfSnapshotV2NetworkPreparedProduct::serial_network(
                platform.memory().clone(),
                network_topology(state),
            );
            assert!(matches!(
                prepare_hvf_snapshot_v2_network_mmio_platform_plan_with_cancel(
                    &platform,
                    product,
                    process,
                    |stage| stage == cancelled,
                ),
                Err(PrepareHvfSnapshotV2NetworkPlatformPlanError::Cancelled { stage })
                    if stage == cancelled
            ));
        }
    }

    #[test]
    fn errors_and_plans_redact_network_relationship_values() {
        let (platform, state) = network_fixture(SnapshotV2DeviceTransportKind::Mmio);
        let process = mmio_process(&state);
        let product = HvfSnapshotV2NetworkPreparedProduct::serial_network(
            platform.memory().clone(),
            network_topology(state),
        );
        let plan = prepare_hvf_snapshot_v2_network_mmio_platform_plan(&platform, product, process)
            .expect("redaction fixture should plan");
        let debug = format!("{plan:?}");
        assert!(!debug.contains("eth0"));
        assert!(!debug.contains("vmnet:shared"));
        assert!(debug.contains(REDACTED));

        let error = PrepareHvfSnapshotV2NetworkPlatformPlanError::PciCapacity {
            count: 32,
            maximum: 31,
        };
        let debug = format!("{error:?}");
        let display = error.to_string();
        assert!(!debug.contains("32"));
        assert!(!debug.contains("31"));
        assert!(!display.contains("32"));
        assert!(!display.contains("31"));
    }
}
