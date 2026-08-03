use std::cell::Cell;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::memory::{GuestAddress, GuestMemoryLayout};
use crate::message_interrupt::{
    GuestMessage, GuestMessageInterrupt, GuestMessageInterruptRegistry,
    GuestMessageInterruptSignalError,
};
use crate::mmio::MmioRegionId;
use crate::snapshot::SnapshotVsockSelectorError;
use crate::snapshot_device_v2::SnapshotV2DeviceTransport;
use crate::snapshot_device_v2::snapshot_v2_device_key_for_test;
use crate::snapshot_restore::{SnapshotRestorePublicId, SnapshotRestoreResourceClass};
use crate::snapshot_vsock_v2_12::NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION;
use crate::vsock::{
    SuppliedVsockListener, VirtioVsockReconstructionResource, VsockBackendSelector,
    VsockGuestConnector,
};

use super::*;

const INACTIVE_MMIO_HEX: &str = include_str!("../snapshot_vsock_v2_12/fixtures/inactive-mmio.hex");
const ACTIVE_PCI_HEX: &str = include_str!("../snapshot_vsock_v2_12/fixtures/active-pci.hex");
static NEXT_LISTENER_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
struct RejectingGuestConnector;

impl VsockGuestConnector for RejectingGuestConnector {
    fn connect(&mut self, _host_port: u32) -> std::io::Result<UnixStream> {
        Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
    }
}

#[derive(Debug)]
struct TestMessageRoute(GuestMessage);

impl GuestMessageInterrupt for TestMessageRoute {
    fn matches(&self, message: GuestMessage) -> bool {
        self.0 == message
    }

    fn signal(&self, message: GuestMessage) -> Result<(), GuestMessageInterruptSignalError> {
        if self.matches(message) {
            Ok(())
        } else {
            Err(GuestMessageInterruptSignalError::new(
                "test route rejected an unknown message",
                false,
            ))
        }
    }
}

fn vsock_message_registry(state: &SnapshotV2VsockState) -> GuestMessageInterruptRegistry {
    let SnapshotV2DeviceTransport::Pci(pci) = state.transport() else {
        panic!("test vsock should use PCI");
    };
    let routes: Vec<Arc<dyn GuestMessageInterrupt>> = pci
        .msix()
        .entries()
        .iter()
        .map(|entry| {
            let address = (u64::from(entry.message_address_high()) << 32)
                | u64::from(entry.message_address_low());
            Arc::new(TestMessageRoute(GuestMessage::new(
                address,
                entry.message_data(),
            ))) as Arc<dyn GuestMessageInterrupt>
        })
        .collect();
    GuestMessageInterruptRegistry::new(routes)
        .expect("vsock message registry should contain every retained vector")
}

fn supplied_vsock_resource(
    captured: VsockBackendSelector,
    destination: VsockBackendSelector,
) -> VirtioVsockReconstructionResource {
    let listener_path = std::path::Path::new("/tmp").join(format!(
        "bb-vsock-{}-{}.sock",
        std::process::id(),
        NEXT_LISTENER_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let listener = UnixListener::bind(&listener_path)
        .expect("test reconstruction listener should bind before handoff");
    std::fs::remove_file(&listener_path)
        .expect("test reconstruction listener should unlink after bind");
    VirtioVsockReconstructionResource::with_destination_selector(
        captured,
        destination,
        SuppliedVsockListener::new(listener).with_guest_connector(RejectingGuestConnector),
    )
}

fn decode_hex(value: &str) -> Vec<u8> {
    let value = value.split_whitespace().collect::<String>();
    assert!(value.len().is_multiple_of(2));
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            u8::from_str_radix(
                std::str::from_utf8(pair).expect("fixture pair should be UTF-8"),
                16,
            )
            .expect("fixture pair should be hexadecimal")
        })
        .collect()
}

fn state(fixture: &str) -> SnapshotV2VsockState {
    SnapshotV2VsockState::decode(
        NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION,
        &decode_hex(fixture),
    )
    .expect("exact-2.12 fixture should decode")
}

fn memory_for(state: &SnapshotV2VsockState) -> GuestMemory {
    let range = GuestMemoryRange::new(GuestAddress::new(0), 0x0040_0000)
        .expect("test memory range should validate");
    let layout = GuestMemoryLayout::new(vec![range]).expect("test memory layout should validate");
    let mut memory = GuestMemory::allocate(&layout).expect("test memory should allocate");
    let Some(active) = state.active_queues() else {
        return memory;
    };
    for (queue, cursor) in
        state
            .virtio()
            .queues()
            .iter()
            .zip([active.rx(), active.tx(), active.event()])
    {
        let available_index = queue
            .driver_ring()
            .checked_add(2)
            .expect("available index should not overflow");
        memory
            .write_slice(&cursor.next_available().to_le_bytes(), available_index)
            .expect("available index should write");
        let used_index = queue
            .device_ring()
            .checked_add(2)
            .expect("used index should not overflow");
        memory
            .write_slice(&cursor.next_used().to_le_bytes(), used_index)
            .expect("used index should write");
    }
    memory
}

fn blank_memory() -> GuestMemory {
    let state = state(INACTIVE_MMIO_HEX);
    memory_for(&state)
}

fn resource_key(
    kind: u32,
    instance: u32,
    public_id: &str,
    resource_class: SnapshotRestoreResourceClass,
) -> SnapshotRestoreResourceKey {
    SnapshotRestoreResourceKey::new(
        snapshot_v2_device_key_for_test(kind, instance),
        SnapshotRestorePublicId::try_from(public_id).expect("test public ID should validate"),
        resource_class,
    )
}

#[test]
fn valid_mmio_and_pci_topologies_are_owner_free_and_normalize_exactly() {
    for (portable, transport) in [
        (
            state(INACTIVE_MMIO_HEX),
            SnapshotV2DeviceTransportKind::Mmio,
        ),
        (state(ACTIVE_PCI_HEX), SnapshotV2DeviceTransportKind::Pci),
    ] {
        let memory = memory_for(&portable);
        let captured_selector = portable.backend_selector().path().to_path_buf();
        let expected = portable.clone();
        let topology =
            PreparedSnapshotV2VsockRestoreTopology::prepare(portable, None, transport, &memory)
                .expect("valid portable topology should prepare");

        assert_eq!(topology.transport_kind(), transport);
        assert_eq!(topology.request().resource_key().device_key().kind(), 5);
        assert_eq!(topology.request().resource_key().device_key().instance(), 0);
        assert_eq!(
            topology.request().resource_key().resource_class(),
            SnapshotRestoreResourceClass::VsockEndpoint
        );
        assert_eq!(
            topology.request().resource_key().public_id().as_str(),
            NATIVE_V2_VSOCK_RESTORE_PUBLIC_ID
        );
        assert_eq!(
            topology.request().selectors().captured().path(),
            captured_selector
        );
        assert_eq!(
            topology.request().selectors().destination().path(),
            captured_selector
        );
        assert_eq!(
            topology.request().config().guest_cid(),
            expected.guest_cid() as u32
        );
        assert_eq!(
            topology.request().config().uds_path(),
            captured_selector.as_path()
        );
        assert!(!topology.request().is_overridden());

        let debug = format!("{topology:?} {:?}", topology.request());
        assert!(!debug.contains(captured_selector.to_string_lossy().as_ref()));
        assert_eq!(
            topology
                .into_normalized_state()
                .expect("checked topology should normalize"),
            expected
        );
    }
}

#[test]
fn explicit_override_changes_only_destination_intent() {
    let portable = state(INACTIVE_MMIO_HEX);
    let captured = portable.backend_selector().path().to_path_buf();
    let destination = "/tmp/bangbang-vsock-restored-private.sock";
    let memory = memory_for(&portable);
    let requested = SnapshotVsockOverride::new(destination);
    let topology = PreparedSnapshotV2VsockRestoreTopology::prepare(
        portable,
        Some(&requested),
        SnapshotV2DeviceTransportKind::Mmio,
        &memory,
    )
    .expect("valid override should prepare");

    assert_eq!(topology.request().selectors().captured().path(), captured);
    assert_eq!(
        topology.request().selectors().destination().path(),
        std::path::Path::new(destination)
    );
    assert_eq!(
        topology.request().config().uds_path(),
        std::path::Path::new(destination)
    );
    assert!(topology.request().is_overridden());
    let destination_state = topology
        .state()
        .clone()
        .into_destination_normalized_state(topology.request().config())
        .expect("override should normalize to the selected destination");
    assert_eq!(
        destination_state.backend_selector().path(),
        std::path::Path::new(destination)
    );
    assert_eq!(
        destination_state.guest_cid(),
        u64::from(topology.request().config().guest_cid())
    );
    let debug = format!("{topology:?} {:?}", topology.request());
    assert!(!debug.contains(destination));
    assert!(!debug.contains(captured.to_string_lossy().as_ref()));
}

#[test]
fn active_pci_state_materializes_one_exact_destination_endpoint() {
    let portable = state(ACTIVE_PCI_HEX);
    let memory = memory_for(&portable);
    let destination = std::path::Path::new("/tmp").join(format!(
        "bangbang-vsock-restored-pci-{}.sock",
        std::process::id()
    ));
    let requested = SnapshotVsockOverride::new(&destination);
    let topology = PreparedSnapshotV2VsockRestoreTopology::prepare(
        portable.clone(),
        Some(&requested),
        SnapshotV2DeviceTransportKind::Pci,
        &memory,
    )
    .expect("active PCI topology should prepare");
    let expected = topology
        .state()
        .clone()
        .into_destination_normalized_state(topology.request().config())
        .expect("checked PCI state should normalize to the destination");
    let config = topology.request().config().clone();
    let captured_selector = portable.backend_selector().clone();
    let destination_selector = VsockBackendSelector::try_from_path(&destination)
        .expect("destination selector should validate");
    let messages = vsock_message_registry(&portable);
    let (_, prepared) = topology.into_parts();
    let PreparedSnapshotV2VsockRestoreState::Pci(prepared) = prepared else {
        panic!("prepared state should retain PCI transport");
    };
    let origin = prepared.origin();
    let sbdf = prepared.sbdf();
    let bar_range = prepared.bar_range();
    let mut resource = supplied_vsock_resource(captured_selector, destination_selector);
    let endpoint = prepared
        .into_pci_endpoint(
            &config,
            &memory,
            &mut resource,
            MmioRegionId::new(700),
            messages,
        )
        .expect("checked PCI state should materialize");

    assert!(resource.is_consumed());
    assert_eq!(endpoint.guest_cid(), config.guest_cid());
    assert_eq!(endpoint.uds_path(), destination);
    assert_eq!(endpoint.expected_state(), &expected);
    assert_eq!(endpoint.origin(), origin);
    assert_eq!(endpoint.endpoint().sbdf(), sbdf);
    assert_eq!(endpoint.endpoint().bar_range(), bar_range);
    assert_eq!(endpoint.endpoint().region_id(), MmioRegionId::new(700));
    assert!(
        endpoint
            .endpoint()
            .endpoint()
            .transport_state()
            .expect("reconstructed transport should recapture")
            .is_device_activated()
    );
    let debug = format!("{endpoint:?}");
    assert!(debug.contains(REDACTED));
    assert!(!debug.contains(destination.to_string_lossy().as_ref()));
}

#[test]
fn pci_endpoint_rejects_mismatched_destination_without_consuming_resource() {
    let portable = state(ACTIVE_PCI_HEX);
    let memory = memory_for(&portable);
    let destination = "/tmp/bangbang-vsock-expected-pci.sock";
    let topology = PreparedSnapshotV2VsockRestoreTopology::prepare(
        portable.clone(),
        Some(&SnapshotVsockOverride::new(destination)),
        SnapshotV2DeviceTransportKind::Pci,
        &memory,
    )
    .expect("active PCI topology should prepare");
    let config = topology.request().config().clone();
    let messages = vsock_message_registry(&portable);
    let (_, prepared) = topology.into_parts();
    let PreparedSnapshotV2VsockRestoreState::Pci(prepared) = prepared else {
        panic!("prepared state should retain PCI transport");
    };
    let wrong_destination =
        VsockBackendSelector::try_from_path("/tmp/bangbang-vsock-wrong-pci.sock")
            .expect("wrong test selector should still validate");
    let mut resource =
        supplied_vsock_resource(portable.backend_selector().clone(), wrong_destination);
    let error = prepared
        .into_pci_endpoint(
            &config,
            &memory,
            &mut resource,
            MmioRegionId::new(700),
            messages,
        )
        .expect_err("mismatched destination selector should reject");

    assert!(matches!(
        error,
        SnapshotV2VsockPciEndpointError::ResourceIdentity
    ));
    assert!(!resource.is_consumed());
    let diagnostic = format!("{error:?} {error}");
    assert!(!diagnostic.contains(destination));
    assert!(!diagnostic.contains("bangbang-vsock-wrong-pci"));
}

#[test]
fn absent_or_invalid_selector_policy_finishes_before_callbacks() {
    let memory = blank_memory();
    let calls = Cell::new(0);
    let absent = PreparedSnapshotV2VsockRestoreTopology::prepare_optional_with_cancel(
        None,
        None,
        SnapshotV2DeviceTransportKind::Mmio,
        &memory,
        |_| {
            calls.set(calls.get() + 1);
            false
        },
    )
    .expect("absent state without override should remain absent");
    assert!(absent.is_none());
    assert_eq!(calls.get(), 0);

    let secret = "/tmp/private-no-device.sock";
    let error = PreparedSnapshotV2VsockRestoreTopology::prepare_optional_with_cancel(
        None,
        Some(&SnapshotVsockOverride::new(secret)),
        SnapshotV2DeviceTransportKind::Mmio,
        &memory,
        |_| {
            calls.set(calls.get() + 1);
            false
        },
    )
    .expect_err("override without a captured device should fail");
    assert!(matches!(
        error,
        SnapshotV2VsockRestorePreparationError::Selector(
            SnapshotVsockSelectorError::OverrideWithoutDevice
        )
    ));
    assert_eq!(calls.get(), 0);
    assert!(!format!("{error:?} {error}").contains(secret));

    let portable = state(INACTIVE_MMIO_HEX);
    let invalid = "invalid\nprivate";
    let error = PreparedSnapshotV2VsockRestoreTopology::prepare_with_cancel(
        portable,
        Some(&SnapshotVsockOverride::new(invalid)),
        SnapshotV2DeviceTransportKind::Mmio,
        &memory,
        |_| {
            calls.set(calls.get() + 1);
            false
        },
    )
    .expect_err("invalid destination selector should fail");
    assert!(matches!(
        error,
        SnapshotV2VsockRestorePreparationError::Selector(
            SnapshotVsockSelectorError::InvalidOverride(_)
        )
    ));
    assert_eq!(calls.get(), 0);
    assert!(!format!("{error:?} {error}").contains(invalid));
}

#[test]
fn product_transport_and_destination_memory_fail_before_publication() {
    let mmio = state(INACTIVE_MMIO_HEX);
    let memory = memory_for(&mmio);
    assert!(matches!(
        PreparedSnapshotV2VsockRestoreTopology::prepare(
            mmio,
            None,
            SnapshotV2DeviceTransportKind::Pci,
            &memory,
        ),
        Err(SnapshotV2VsockRestorePreparationError::DestinationTransport)
    ));

    let pci = state(ACTIVE_PCI_HEX);
    let empty = blank_memory();
    assert!(matches!(
        PreparedSnapshotV2VsockRestoreTopology::prepare(
            pci,
            None,
            SnapshotV2DeviceTransportKind::Pci,
            &empty,
        ),
        Err(SnapshotV2VsockRestorePreparationError::Device(_))
    ));
}

#[test]
fn wrong_resource_key_class_and_private_identity_are_rejected() {
    let portable = state(INACTIVE_MMIO_HEX);
    let memory = memory_for(&portable);
    let cases = [
        resource_key(
            4,
            0,
            NATIVE_V2_VSOCK_RESTORE_PUBLIC_ID,
            SnapshotRestoreResourceClass::VsockEndpoint,
        ),
        resource_key(
            5,
            0,
            NATIVE_V2_VSOCK_RESTORE_PUBLIC_ID,
            SnapshotRestoreResourceClass::NetworkPacketIo,
        ),
        resource_key(
            5,
            0,
            "private-vsock",
            SnapshotRestoreResourceClass::VsockEndpoint,
        ),
    ];
    for injected in cases {
        let error = prepare_optional_vsock_restore_topology(
            Some(portable.clone()),
            None,
            SnapshotV2DeviceTransportKind::Mmio,
            &memory,
            |_| false,
            AllocationPolicy::System,
            Some(injected),
        )
        .expect_err("wrong resource identity should fail");
        assert!(matches!(
            error,
            SnapshotV2VsockRestorePreparationError::ResourceIdentity
        ));
    }
}

#[test]
fn cancellation_and_allocation_checkpoints_are_deterministic() {
    let portable = state(INACTIVE_MMIO_HEX);
    let memory = memory_for(&portable);
    for target in [
        SnapshotV2VsockRestorePreparationStage::Resource,
        SnapshotV2VsockRestorePreparationStage::Device,
        SnapshotV2VsockRestorePreparationStage::Normalize,
        SnapshotV2VsockRestorePreparationStage::Completion,
    ] {
        let observed = Cell::new(Vec::new());
        let error = PreparedSnapshotV2VsockRestoreTopology::prepare_with_cancel(
            portable.clone(),
            None,
            SnapshotV2DeviceTransportKind::Mmio,
            &memory,
            |stage| {
                let mut stages = observed.take();
                stages.push(stage);
                observed.set(stages);
                stage == target
            },
        )
        .expect_err("selected cancellation stage should stop preparation");
        assert!(error.is_cancelled());
        assert!(matches!(
            error,
            SnapshotV2VsockRestorePreparationError::Cancelled { stage } if stage == target
        ));
        assert_eq!(observed.take().last().copied(), Some(target));
    }

    for point in [
        AllocationFailure::ResourceId,
        AllocationFailure::DestinationConfig,
        AllocationFailure::DeviceState,
        AllocationFailure::TransportState,
        AllocationFailure::Normalization,
    ] {
        let error = prepare_optional_vsock_restore_topology(
            Some(portable.clone()),
            None,
            SnapshotV2DeviceTransportKind::Mmio,
            &memory,
            |_| false,
            AllocationPolicy::Fail(point),
            None,
        )
        .expect_err("selected allocation point should fail");
        assert!(matches!(
            error,
            SnapshotV2VsockRestorePreparationError::Allocation
        ));
    }
}
