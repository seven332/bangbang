use bangbang_runtime::memory::{GuestAddress, GuestMemoryRange, aarch64};
use bangbang_runtime::snapshot_artifact::{
    NativeSnapshotArtifactFamily, NativeV2SnapshotArtifactProfile,
};
use bangbang_runtime::snapshot_commit::{SnapshotCommitKind, SnapshotCommitRecord};
use bangbang_runtime::snapshot_device_v2::SnapshotV2DeviceTransportKind;
use bangbang_runtime::snapshot_diff_v2_13::SnapshotV2DiffBase;
use bangbang_runtime::snapshot_format::NativeSnapshotFormatError;
use bangbang_runtime::snapshot_format_v2::{
    NATIVE_V2_SNAPSHOT_INTEGRITY_BYTES, SnapshotV2DecodeError,
};

use super::*;
use crate::snapshot_bundle::tests::{fixture as native_v1_fixture, memory_binding};
use crate::snapshot_v2::tests::{
    ENTROPY_INACTIVE_MMIO_FIXTURE_HEX, MMIO_GRAPH_FIXTURE_HEX, MULTI_BLOCK_MMIO_GRAPH_FIXTURE_HEX,
    SERIAL_CONFIGURED_FIXTURE_HEX, STORAGE_MMIO_GRAPH_FIXTURE_HEX, complete_balloon_state_fixture,
    complete_diff_state_fixture, complete_entropy_state_fixture,
    complete_memory_hotplug_state_fixture, complete_multi_block_state_fixture,
    complete_network_state_fixture, complete_serial_state_fixture, complete_state_fixture,
    complete_storage_state_fixture, complete_vsock_state_fixture,
    exact_minor_twelve_memory_binding, platform_fixture, product_memory_hotplug_fixture,
};

fn assert_document(
    bytes: &[u8],
    expected_profile: HvfNativeSnapshotDocumentProfile,
    expected_version: SnapshotFormatVersion,
    expected_vcpus: usize,
) -> HvfNativeSnapshotDocument {
    let document = HvfNativeSnapshotDocument::decode(bytes)
        .expect("complete exact-profile fixture should decode as a document");
    let expected_family = match expected_profile {
        HvfNativeSnapshotDocumentProfile::V1 => NativeSnapshotArtifactFamily::V1,
        HvfNativeSnapshotDocumentProfile::V2(_) => NativeSnapshotArtifactFamily::V2,
    };
    assert_eq!(document.family(), expected_family);
    assert_eq!(document.version(), expected_version);
    assert_eq!(document.profile(), expected_profile);
    assert_eq!(document.vcpu_count(), expected_vcpus);

    let mut vcpus = document.vcpus();
    assert_eq!(vcpus.len(), expected_vcpus);
    let indexes = vcpus
        .by_ref()
        .map(HvfNativeSnapshotVcpuRef::index)
        .collect::<Vec<_>>();
    assert_eq!(vcpus.len(), 0);
    assert_eq!(
        indexes,
        (0..u32::try_from(expected_vcpus).expect("fixture vCPU count should fit u32"))
            .collect::<Vec<_>>()
    );

    assert_eq!(
        document
            .encode()
            .expect("checked document should encode canonically"),
        bytes
    );
    let replacements = document
        .vcpus()
        .map(HvfNativeSnapshotVcpuState::from)
        .collect::<Vec<_>>();
    let replaced = document
        .clone()
        .try_replace_vcpus(replacements)
        .expect("same-value replacement should revalidate");
    assert_eq!(replaced, document);
    assert_eq!(
        replaced
            .encode()
            .expect("rebuilt document should encode canonically"),
        bytes
    );
    document
}

fn native_v1_document_fixture() -> (HvfSnapshotV1Bundle, Vec<u8>) {
    let bundle = HvfSnapshotV1Bundle::try_new(memory_binding(1), native_v1_fixture())
        .expect("native-v1 fixture bundle should validate");
    let bytes = encode_snapshot_commit_envelope(bundle.commit_record())
        .expect("native-v1 fixture envelope should encode");
    (bundle, bytes)
}

#[test]
fn native_v1_document_preserves_commit_memory_state_and_replacement() {
    let (bundle, bytes) = native_v1_document_fixture();
    let document = assert_document(
        &bytes,
        HvfNativeSnapshotDocumentProfile::V1,
        NATIVE_V1_SNAPSHOT_VERSION,
        1,
    );

    let HvfNativeSnapshotDocumentState::V1(decoded) = &document.state else {
        panic!("native-v1 fixture should retain the native-v1 variant");
    };
    assert_eq!(decoded, &bundle);
    assert_eq!(
        decoded.commit_record().kind(),
        SnapshotCommitKind::Composite
    );
    assert_eq!(
        decoded.commit_record().memory_binding(),
        bundle.commit_record().memory_binding()
    );
    assert_eq!(decoded.state(), bundle.state());

    let HvfNativeSnapshotPlatformRef::V1 {
        memory_binding,
        state,
    } = document.platform()
    else {
        panic!("native-v1 fixture should expose a native-v1 platform");
    };
    assert_eq!(memory_binding, bundle.commit_record().memory_binding());
    assert_eq!(state, bundle.state());
}

#[test]
fn exact_native_v2_profiles_round_trip_without_outer_state_loss() {
    let legacy = platform_fixture(false);
    let bytes =
        encode_hvf_snapshot_v2_platform_state(&legacy).expect("exact-2.3 fixture should encode");
    let document = assert_document(
        &bytes,
        HvfNativeSnapshotDocumentProfile::V2(NativeV2SnapshotArtifactProfile::LegacyPlatformV2_3),
        NATIVE_V2_LEGACY_PLATFORM_VERSION,
        2,
    );
    assert!(matches!(
        &document.state,
        HvfNativeSnapshotDocumentState::V2LegacyPlatform(state) if state == &legacy
    ));

    let device_graph = complete_state_fixture(MMIO_GRAPH_FIXTURE_HEX);
    let bytes =
        encode_hvf_snapshot_v2_state(&device_graph).expect("exact-2.4 fixture should encode");
    let document = assert_document(
        &bytes,
        HvfNativeSnapshotDocumentProfile::V2(NativeV2SnapshotArtifactProfile::DeviceGraphV2_4),
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        2,
    );
    assert!(matches!(
        &document.state,
        HvfNativeSnapshotDocumentState::V2DeviceGraph(state) if state == &device_graph
    ));

    let multi_block = complete_multi_block_state_fixture(MULTI_BLOCK_MMIO_GRAPH_FIXTURE_HEX);
    let bytes = encode_hvf_snapshot_v2_multi_block_state(&multi_block)
        .expect("exact-2.5 fixture should encode");
    let document = assert_document(
        &bytes,
        HvfNativeSnapshotDocumentProfile::V2(
            NativeV2SnapshotArtifactProfile::MultiBlockDeviceGraphV2_5,
        ),
        NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        2,
    );
    assert!(matches!(
        &document.state,
        HvfNativeSnapshotDocumentState::V2MultiBlock(state) if state == &multi_block
    ));

    let storage = complete_storage_state_fixture(STORAGE_MMIO_GRAPH_FIXTURE_HEX);
    let bytes =
        encode_hvf_snapshot_v2_storage_state(&storage).expect("exact-2.6 fixture should encode");
    let document = assert_document(
        &bytes,
        HvfNativeSnapshotDocumentProfile::V2(
            NativeV2SnapshotArtifactProfile::StorageDeviceGraphV2_6,
        ),
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        2,
    );
    assert!(matches!(
        &document.state,
        HvfNativeSnapshotDocumentState::V2Storage(state) if state == &storage
    ));

    let serial = complete_serial_state_fixture(
        Some(STORAGE_MMIO_GRAPH_FIXTURE_HEX),
        SERIAL_CONFIGURED_FIXTURE_HEX,
    );
    let bytes =
        encode_hvf_snapshot_v2_serial_state(&serial).expect("exact-2.7 fixture should encode");
    let document = assert_document(
        &bytes,
        HvfNativeSnapshotDocumentProfile::V2(NativeV2SnapshotArtifactProfile::SerialStateV2_7),
        NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
        2,
    );
    assert!(matches!(
        &document.state,
        HvfNativeSnapshotDocumentState::V2Serial(state) if state == &serial
    ));

    let entropy = complete_entropy_state_fixture(
        None,
        SERIAL_CONFIGURED_FIXTURE_HEX,
        Some(ENTROPY_INACTIVE_MMIO_FIXTURE_HEX),
        false,
    );
    let bytes =
        encode_hvf_snapshot_v2_entropy_state(&entropy).expect("exact-2.8 fixture should encode");
    let document = assert_document(
        &bytes,
        HvfNativeSnapshotDocumentProfile::V2(NativeV2SnapshotArtifactProfile::EntropyStateV2_8),
        NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
        2,
    );
    assert!(matches!(
        &document.state,
        HvfNativeSnapshotDocumentState::V2Entropy(state) if state == &entropy
    ));

    let balloon =
        complete_balloon_state_fixture(SnapshotV2DeviceTransportKind::Mmio, true, true, true);
    let bytes =
        encode_hvf_snapshot_v2_balloon_state(&balloon).expect("exact-2.9 fixture should encode");
    let document = assert_document(
        &bytes,
        HvfNativeSnapshotDocumentProfile::V2(NativeV2SnapshotArtifactProfile::BalloonStateV2_9),
        NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
        2,
    );
    assert!(matches!(
        &document.state,
        HvfNativeSnapshotDocumentState::V2Balloon(state) if state == &balloon
    ));

    let memory_hotplug = complete_memory_hotplug_state_fixture(
        SnapshotV2DeviceTransportKind::Mmio,
        true,
        true,
        true,
        true,
    );
    let bytes = encode_hvf_snapshot_v2_memory_hotplug_state(&memory_hotplug)
        .expect("exact-2.10 fixture should encode");
    let document = assert_document(
        &bytes,
        HvfNativeSnapshotDocumentProfile::V2(
            NativeV2SnapshotArtifactProfile::MemoryHotplugStateV2_10,
        ),
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
        2,
    );
    assert!(matches!(
        &document.state,
        HvfNativeSnapshotDocumentState::V2MemoryHotplug(state) if state == &memory_hotplug
    ));

    let network = complete_network_state_fixture(
        SnapshotV2DeviceTransportKind::Mmio,
        true,
        true,
        true,
        true,
        true,
    );
    let bytes =
        encode_hvf_snapshot_v2_network_state(&network).expect("exact-2.11 fixture should encode");
    let document = assert_document(
        &bytes,
        HvfNativeSnapshotDocumentProfile::V2(NativeV2SnapshotArtifactProfile::NetworkStateV2_11),
        NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION,
        2,
    );
    assert!(matches!(
        &document.state,
        HvfNativeSnapshotDocumentState::V2Network(state) if state == &network
    ));

    let vsock = complete_vsock_state_fixture(
        SnapshotV2DeviceTransportKind::Mmio,
        true,
        true,
        true,
        true,
        true,
        true,
    );
    let bytes =
        encode_hvf_snapshot_v2_vsock_state(&vsock).expect("exact-2.12 fixture should encode");
    let document = assert_document(
        &bytes,
        HvfNativeSnapshotDocumentProfile::V2(NativeV2SnapshotArtifactProfile::VsockStateV2_12),
        NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION,
        2,
    );
    assert!(matches!(
        &document.state,
        HvfNativeSnapshotDocumentState::V2Vsock(state) if state == &vsock
    ));

    let predecessor_memory_hotplug =
        product_memory_hotplug_fixture(SnapshotV2DeviceTransportKind::Mmio);
    let predecessor = exact_minor_twelve_memory_binding(Some(&predecessor_memory_hotplug));
    let extent = GuestMemoryRange::new(GuestAddress::new(aarch64::DRAM_MEM_START), 4096)
        .expect("Diff fixture extent should validate");
    let diff = complete_diff_state_fixture(
        SnapshotV2DeviceTransportKind::Mmio,
        true,
        true,
        true,
        true,
        true,
        true,
        SnapshotV2DiffBase::Image(predecessor.clone()),
        &[extent],
    );
    assert_eq!(diff.layer().base().binding(), Some(&predecessor));
    assert_eq!(diff.layer().data_extents().len(), 1);
    let layer = diff.layer().clone();
    let bytes = encode_hvf_snapshot_v2_diff_state(&diff).expect("exact-2.13 fixture should encode");
    let document = assert_document(
        &bytes,
        HvfNativeSnapshotDocumentProfile::V2(NativeV2SnapshotArtifactProfile::DiffStateV2_13),
        NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION,
        2,
    );
    let HvfNativeSnapshotDocumentState::V2Diff(decoded) = &document.state else {
        panic!("exact-2.13 fixture should retain the Diff variant");
    };
    assert_eq!(decoded, &diff);
    assert_eq!(decoded.layer(), &layer);
    assert_eq!(decoded.layer().base().binding(), Some(&predecessor));
    assert_eq!(decoded.layer().data_extents(), layer.data_extents());
    assert_eq!(decoded.layer().result(), diff.platform().memory());
}

#[test]
fn replacement_rejects_count_family_and_invalid_vcpu_order() {
    let (_, v1_bytes) = native_v1_document_fixture();
    let v1 = HvfNativeSnapshotDocument::decode(&v1_bytes)
        .expect("native-v1 fixture should decode as a document");
    assert!(matches!(
        v1.clone().try_replace_vcpus(Vec::new()),
        Err(HvfNativeSnapshotDocumentReplaceError::VcpuCount {
            expected: 1,
            actual: 0
        })
    ));

    let v2_bytes = encode_hvf_snapshot_v2_platform_state(&platform_fixture(false))
        .expect("native-v2 fixture should encode");
    let v2 = HvfNativeSnapshotDocument::decode(&v2_bytes)
        .expect("native-v2 fixture should decode as a document");
    let v2_replacement = v2
        .vcpus()
        .next()
        .map(HvfNativeSnapshotVcpuState::from)
        .expect("native-v2 fixture should contain vCPUs");
    assert!(matches!(
        v1.try_replace_vcpus(vec![v2_replacement]),
        Err(HvfNativeSnapshotDocumentReplaceError::VcpuFamily)
    ));

    let v1_state = HvfNativeSnapshotVcpuState::V1(Box::new(native_v1_fixture().vcpu().clone()));
    assert!(matches!(
        v2.clone()
            .try_replace_vcpus(vec![v1_state.clone(), v1_state]),
        Err(HvfNativeSnapshotDocumentReplaceError::VcpuFamily)
    ));

    let mut reversed = v2
        .vcpus()
        .map(HvfNativeSnapshotVcpuState::from)
        .collect::<Vec<_>>();
    reversed.reverse();
    assert!(matches!(
        v2.try_replace_vcpus(reversed),
        Err(HvfNativeSnapshotDocumentReplaceError::NativeV2(_))
    ));
}

#[test]
fn decode_rejects_malformed_incompatible_nonexact_and_mismatched_profiles() {
    let (_, mut malformed_v1) = native_v1_document_fixture();
    *malformed_v1
        .last_mut()
        .expect("native-v1 envelope should contain an integrity trailer") ^= 1;
    assert!(matches!(
        HvfNativeSnapshotDocument::decode(&malformed_v1),
        Err(HvfNativeSnapshotDocumentDecodeError::Format(
            NativeSnapshotFormatError::NativeV1(_)
        ))
    ));

    let mut malformed_v2 = encode_hvf_snapshot_v2_platform_state(&platform_fixture(false))
        .expect("native-v2 fixture should encode");
    *malformed_v2
        .last_mut()
        .expect("native-v2 state should contain an integrity trailer") ^= 1;
    assert!(matches!(
        HvfNativeSnapshotDocument::decode(&malformed_v2),
        Err(HvfNativeSnapshotDocumentDecodeError::Format(
            NativeSnapshotFormatError::NativeV2(_)
        ))
    ));

    let firecracker_aarch64 = [0x00, 0x00, 0x00, 0xaa, 0xaa, 0x84, 0x19, 0x10, 0x07];
    assert!(matches!(
        HvfNativeSnapshotDocument::decode(&firecracker_aarch64),
        Err(HvfNativeSnapshotDocumentDecodeError::Format(
            NativeSnapshotFormatError::IncompatibleFirecrackerFormat
        ))
    ));

    let memory_only = SnapshotCommitRecord::new(memory_binding(1));
    let memory_only = encode_snapshot_commit_envelope(&memory_only)
        .expect("memory-only native-v1 fixture should encode");
    assert!(matches!(
        HvfNativeSnapshotDocument::decode(&memory_only),
        Err(HvfNativeSnapshotDocumentDecodeError::NativeV1(
            HvfSnapshotV1BundleError::MemoryOnlyCommit
        ))
    ));

    let mut nonexact = encode_hvf_snapshot_v2_platform_state(&platform_fixture(false))
        .expect("native-v2 fixture should encode");
    rewrite_v2_version(&mut nonexact, 3, 1);
    let error = HvfNativeSnapshotDocument::decode(&nonexact)
        .expect_err("structurally admitted 2.3.1 must not select exact 2.3.0");
    let HvfNativeSnapshotDocumentDecodeError::UnsupportedExactProfile(version) = error else {
        panic!("2.3.1 should fail as an unsupported exact profile");
    };
    assert_eq!(version.major(), 2);
    assert_eq!(version.minor(), 3);
    assert_eq!(version.patch(), 1);

    let mut future = encode_hvf_snapshot_v2_platform_state(&platform_fixture(false))
        .expect("native-v2 fixture should encode");
    rewrite_v2_version(&mut future, 14, 0);
    assert!(matches!(
        HvfNativeSnapshotDocument::decode(&future),
        Err(HvfNativeSnapshotDocumentDecodeError::Format(
            NativeSnapshotFormatError::NativeV2(SnapshotV2DecodeError::UnsupportedVersion { .. })
        ))
    ));

    let mut mismatched =
        encode_hvf_snapshot_v2_state(&complete_state_fixture(MMIO_GRAPH_FIXTURE_HEX))
            .expect("exact-2.4 fixture should encode");
    rewrite_v2_version(&mut mismatched, 5, 0);
    assert!(matches!(
        HvfNativeSnapshotDocument::decode(&mismatched),
        Err(HvfNativeSnapshotDocumentDecodeError::NativeV2(_))
    ));
}

#[test]
fn document_views_owned_state_and_errors_redact_values_and_paths() {
    let network = complete_network_state_fixture(
        SnapshotV2DeviceTransportKind::Mmio,
        true,
        true,
        true,
        true,
        true,
    );
    let bytes =
        encode_hvf_snapshot_v2_network_state(&network).expect("exact-2.11 fixture should encode");
    let document = HvfNativeSnapshotDocument::decode(&bytes)
        .expect("exact-2.11 fixture should decode as a document");
    let owned = document
        .vcpus()
        .next()
        .map(HvfNativeSnapshotVcpuState::from)
        .expect("exact-2.11 fixture should contain a vCPU");
    let replace_error = document
        .clone()
        .try_replace_vcpus(Vec::new())
        .expect_err("empty replacement should fail");
    let decode_error = HvfNativeSnapshotDocument::decode(b"/tmp/sensitive-snapshot.state")
        .expect_err("path bytes are not a native state");
    let encode_error =
        HvfNativeSnapshotDocumentEncodeError::NativeV2(HvfSnapshotV2EncodeError::LengthOverflow);
    let rendered = format!(
        "{document:?}\n{:?}\n{:?}\n{owned:?}\n{replace_error}\n{replace_error:?}\n{decode_error}\n{decode_error:?}\n{encode_error}\n{encode_error:?}",
        document.platform(),
        document.vcpus(),
    );
    assert!(rendered.contains("<redacted>"));
    for sensitive in [
        "rootfs.img",
        "serial-log",
        "vmnet:",
        "sensitive-snapshot.state",
        "/tmp/",
    ] {
        assert!(!rendered.contains(sensitive));
    }
}

fn rewrite_v2_version(bytes: &mut [u8], minor: u16, patch: u16) {
    bytes
        .get_mut(10..12)
        .expect("native-v2 header should contain a minor version")
        .copy_from_slice(&minor.to_le_bytes());
    bytes
        .get_mut(12..14)
        .expect("native-v2 header should contain a patch version")
        .copy_from_slice(&patch.to_le_bytes());
    let checksum_offset = bytes
        .len()
        .checked_sub(NATIVE_V2_SNAPSHOT_INTEGRITY_BYTES)
        .expect("native-v2 fixture should contain an integrity trailer");
    let (contents, checksum) = bytes.split_at_mut(checksum_offset);
    checksum.copy_from_slice(&crc64::crc64(0, contents).to_le_bytes());
}
