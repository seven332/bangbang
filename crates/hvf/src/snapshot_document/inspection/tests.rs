use std::io::{self, Seek, SeekFrom, Write};

use bangbang_runtime::memory::{
    GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange, aarch64,
};
use bangbang_runtime::snapshot_diff_v2_13::{
    NATIVE_V2_DIFF_MAX_EXTENTS, NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION, SnapshotV2DiffBase,
    SnapshotV2DiffLayerBinding,
};
use bangbang_runtime::snapshot_memory_v2::{
    NATIVE_V2_MEMORY_GUEST_GRANULE, NATIVE_V2_MEMORY_MAX_EXTENTS, SnapshotV2MemoryBinding,
    write_snapshot_v2_memory_image_with_compatibility_version,
};

use super::*;
use crate::snapshot_document::tests::{
    inspection_document_fixtures, inspection_native_v1_document,
    inspection_native_v1_document_with_pc,
};
use crate::snapshot_v2::tests::{
    maximum_balloon_state_fixture, maximum_memory_hotplug_state_fixture,
    maximum_network_state_fixture, maximum_storage_graph_fixture, platform_fixture,
    platform_fixture_with_count, platform_fixture_with_cpu_template,
};
use serde_json::Value;

#[test]
fn maximum_vcpu_collection_serializes_within_the_public_ceiling() {
    let document = HvfNativeSnapshotDocument {
        state: HvfNativeSnapshotDocumentState::V2LegacyPlatform(platform_fixture_with_count(
            MAX_SUPPORTED_VCPUS,
            false,
        )),
    };
    let json = document
        .inspect_vcpu_states()
        .to_pretty_json()
        .expect("maximum vCPU inspection should serialize");
    assert!(json.len() <= HVF_NATIVE_SNAPSHOT_INSPECTION_MAX_JSON_BYTES);
    let value: Value = serde_json::from_str(&json).expect("maximum vCPU inspection should parse");
    assert_eq!(
        value["vcpus"]
            .as_array()
            .expect("maximum vCPU collection should be an array")
            .len(),
        usize::from(MAX_SUPPORTED_VCPUS)
    );
}

#[test]
fn maximum_memory_extent_collection_serializes_within_the_public_ceiling() {
    // Four guest granules are also aligned to the largest supported host page
    // size, so the same maximum-extent fixture allocates on macOS and Linux.
    let extent_bytes = NATIVE_V2_MEMORY_GUEST_GRANULE * 4;
    let ranges = (0..NATIVE_V2_MEMORY_MAX_EXTENTS)
        .map(|index| {
            GuestMemoryRange::new(
                GuestAddress::new(
                    aarch64::DRAM_MEM_START
                        + u64::try_from(index).expect("memory extent index should fit")
                            * extent_bytes,
                ),
                extent_bytes,
            )
            .expect("maximum memory extent should validate")
        })
        .collect::<Vec<_>>();
    let binding = memory_binding_for_ranges(ranges);
    let value = assert_serializes_within_public_ceiling(&common::V2Memory(&binding));
    assert_eq!(
        value["extent_count"].as_u64(),
        Some(u64::try_from(NATIVE_V2_MEMORY_MAX_EXTENTS).expect("extent count should fit u64"))
    );
    assert_eq!(
        value["extents"]
            .as_array()
            .expect("memory extents should be an array")
            .len(),
        NATIVE_V2_MEMORY_MAX_EXTENTS
    );
}

#[test]
fn maximum_storage_and_network_collections_serialize_within_the_public_ceiling() {
    let storage = maximum_storage_graph_fixture();
    let storage_value =
        assert_serializes_within_public_ceiling(&devices::StorageGraphForTest(&storage));
    assert_eq!(
        storage_value["records"]
            .as_array()
            .expect("storage records should be an array")
            .len(),
        usize::from(NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_RECORDS)
    );

    let network = maximum_network_state_fixture();
    let network_value = assert_serializes_within_public_ceiling(&devices::NetworkForTest(&network));
    assert_eq!(
        network_value["interfaces"]
            .as_array()
            .expect("network interfaces should be an array")
            .len(),
        NATIVE_V2_NETWORK_MAX_INTERFACES
    );
}

#[test]
fn maximum_balloon_accounting_serializes_within_the_public_ceiling() {
    let balloon = maximum_balloon_state_fixture();
    let value = assert_serializes_within_public_ceiling(&devices::BalloonForTest(&balloon));
    assert_eq!(
        value["accounting"]["ranges"]
            .as_array()
            .expect("balloon ranges should be an array")
            .len(),
        NATIVE_V2_BALLOON_STATE_MAX_ACCOUNTING_RANGES
    );
}

#[test]
fn maximum_valid_virtio_mem_bitmap_and_blocks_serialize_within_the_public_ceiling() {
    let memory_hotplug = maximum_memory_hotplug_state_fixture();
    let configured_blocks =
        memory_hotplug.config_space().region_size() / memory_hotplug.config_space().block_size();
    let plugged_ranges = memory_hotplug.plugged_ranges().len();
    let bitmap_bytes = memory_hotplug.plugged_bitmap().len();
    let value =
        assert_serializes_within_public_ceiling(&devices::MemoryHotplugForTest(&memory_hotplug));
    assert_eq!(
        value["configured_block_count"].as_u64(),
        Some(configured_blocks)
    );
    assert_eq!(
        value["plugged_bitmap_byte_count"].as_u64(),
        Some(u64::try_from(bitmap_bytes).expect("bitmap byte count should fit u64"))
    );
    assert_eq!(
        value["plugged_ranges"]
            .as_array()
            .expect("virtio-mem ranges should be an array")
            .len(),
        plugged_ranges
    );
}

#[test]
fn maximum_diff_extent_collection_serializes_within_the_public_ceiling() {
    let granule = NATIVE_V2_MEMORY_GUEST_GRANULE;
    let stride = granule * 2;
    let result_bytes = u64::try_from(NATIVE_V2_DIFF_MAX_EXTENTS)
        .expect("Diff extent count should fit u64")
        * stride;
    let result = memory_binding_for_ranges(vec![
        GuestMemoryRange::new(GuestAddress::new(aarch64::DRAM_MEM_START), result_bytes)
            .expect("maximum Diff result range should validate"),
    ]);
    let ranges = (0..NATIVE_V2_DIFF_MAX_EXTENTS)
        .map(|index| {
            GuestMemoryRange::new(
                GuestAddress::new(
                    aarch64::DRAM_MEM_START
                        + u64::try_from(index).expect("Diff extent index should fit") * stride,
                ),
                granule,
            )
            .expect("maximum Diff extent should validate")
        })
        .collect::<Vec<_>>();
    let layer =
        SnapshotV2DiffLayerBinding::try_from_ranges(SnapshotV2DiffBase::Zero, result, &ranges)
            .expect("maximum Diff layer should validate");
    let value = assert_serializes_within_public_ceiling(&devices::DiffLayerForTest(&layer));
    assert_eq!(
        value["data_extents"]
            .as_array()
            .expect("Diff extents should be an array")
            .len(),
        NATIVE_V2_DIFF_MAX_EXTENTS
    );
}

#[test]
fn injected_limit_rejects_before_returning_a_value() {
    assert!(matches!(
        format_pretty_json_with_limit(&"bounded", 1),
        Err(HvfNativeSnapshotInspectionError::OutputTooLarge { maximum: 1 })
    ));
}

#[test]
fn every_exact_profile_has_deterministic_parseable_views() {
    let expected_profiles = [
        "v1",
        "legacy-platform-v2.3",
        "device-graph-v2.4",
        "multi-block-device-graph-v2.5",
        "storage-device-graph-v2.6",
        "serial-state-v2.7",
        "entropy-state-v2.8",
        "balloon-state-v2.9",
        "memory-hotplug-state-v2.10",
        "network-state-v2.11",
        "vsock-state-v2.12",
        "diff-state-v2.13",
    ];

    let documents = inspection_document_fixtures();
    assert_eq!(documents.len(), expected_profiles.len());
    for (document, expected_profile) in documents.iter().zip(expected_profiles) {
        let vcpus = document
            .inspect_vcpu_states()
            .to_pretty_json()
            .expect("vCPU inspection should serialize");
        let vm = document
            .inspect_vm_state()
            .to_pretty_json()
            .expect("VM inspection should serialize");
        assert_eq!(
            document
                .inspect_vcpu_states()
                .to_pretty_json()
                .expect("repeated vCPU inspection should serialize"),
            vcpus
        );
        assert_eq!(
            document
                .inspect_vm_state()
                .to_pretty_json()
                .expect("repeated VM inspection should serialize"),
            vm
        );
        assert!(vcpus.len() <= HVF_NATIVE_SNAPSHOT_INSPECTION_MAX_JSON_BYTES);
        assert!(vm.len() <= HVF_NATIVE_SNAPSHOT_INSPECTION_MAX_JSON_BYTES);

        let vcpu_value: Value =
            serde_json::from_str(&vcpus).expect("vCPU inspection should be valid JSON");
        let vm_value: Value =
            serde_json::from_str(&vm).expect("VM inspection should be valid JSON");
        assert_eq!(vcpu_value["schema"], SCHEMA);
        assert_eq!(vm_value["schema"], SCHEMA);
        assert_eq!(vcpu_value["view"], VCPU_VIEW);
        assert_eq!(vm_value["view"], VM_VIEW);
        assert_eq!(vcpu_value["profile"], expected_profile);
        assert_eq!(vm_value["profile"], expected_profile);
        assert_eq!(vcpu_value["version"], vm_value["version"]);
        assert_eq!(vcpu_value["vcpus"], vm_value["vcpus"]);

        let indexes = vm_value["vcpus"]
            .as_array()
            .expect("vCPU subtree should be an array")
            .iter()
            .map(|vcpu| {
                vcpu["index"]
                    .as_u64()
                    .expect("vCPU index should be numeric")
            })
            .collect::<Vec<_>>();
        assert_eq!(indexes, (0..indexes.len() as u64).collect::<Vec<_>>());
        assert_fixed_lowercase_hex(&vcpu_value);
        assert_fixed_lowercase_hex(&vm_value);

        assert_root_field_order(
            &vcpus,
            &["schema", "view", "family", "profile", "version", "vcpus"],
        );
        assert_root_field_order(
            &vm,
            &[
                "schema", "view", "family", "profile", "version", "memory", "machine", "global",
                "topology", "time", "vcpus", "devices", "diff",
            ],
        );
    }
}

#[test]
fn ordinary_register_values_remain_explicit() {
    let first: Value = serde_json::from_str(
        &inspection_native_v1_document_with_pc(0x2000)
            .inspect_vcpu_states()
            .to_pretty_json()
            .expect("first native-v1 inspection should serialize"),
    )
    .expect("first native-v1 inspection should parse");
    let second: Value = serde_json::from_str(
        &inspection_native_v1_document_with_pc(0x3000)
            .inspect_vcpu_states()
            .to_pretty_json()
            .expect("second native-v1 inspection should serialize"),
    )
    .expect("second native-v1 inspection should parse");

    assert_eq!(first["vcpus"][0]["general"]["pc"], "0x0000000000002000");
    assert_eq!(second["vcpus"][0]["general"]["pc"], "0x0000000000003000");
    assert_ne!(
        first["vcpus"][0]["general"]["pc"],
        second["vcpus"][0]["general"]["pc"]
    );
}

#[test]
fn ordinary_cpu_template_values_remain_explicit_at_their_exact_widths() {
    let document = HvfNativeSnapshotDocument {
        state: HvfNativeSnapshotDocumentState::V2LegacyPlatform(
            platform_fixture_with_cpu_template(),
        ),
    };
    let value: Value = serde_json::from_str(
        &document
            .inspect_vm_state()
            .to_pretty_json()
            .expect("CPU-template inspection should serialize"),
    )
    .expect("CPU-template inspection should parse");
    let entries = value["machine"]["cpu_template_application"]["entries"]
        .as_array()
        .expect("CPU-template entries should be an array");
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0]["filter"], "0x000000ff");
    assert_eq!(entries[1]["filter"], "0x000000000000ff00");
    assert_eq!(
        entries[2]["common_baseline"],
        "0x0000001000000000000000000000abcd"
    );
    assert!(entries.iter().all(|entry| entry.get("values").is_none()));
    assert!(
        value["global"]["compatibility"]["cache_manifest"]["ctr_el0"]
            .as_str()
            .is_some()
    );
}

#[test]
fn sve_sme_identification_is_explicit_while_mutable_sme_state_is_fingerprinted() {
    let document = HvfNativeSnapshotDocument {
        state: HvfNativeSnapshotDocumentState::V2LegacyPlatform(platform_fixture(true)),
    };
    let value: Value = serde_json::from_str(
        &document
            .inspect_vm_state()
            .to_pretty_json()
            .expect("SME inspection should serialize"),
    )
    .expect("SME inspection should parse");
    let identification = &value["global"]["compatibility"]["sve_sme_identification"];
    assert_eq!(identification["id_aa64zfr0_el1"], "0x0000000000001234");
    assert_eq!(identification["id_aa64smfr0_el1"], "0x0000000000005678");
    assert_eq!(
        value["vcpus"][0]["reviewed_sme"]["state"]["algorithm"],
        "sha256"
    );
}

#[test]
fn fdt_checksum_is_literal_redaction_not_an_equality_oracle() {
    for document in inspection_document_fixtures().into_iter().skip(1) {
        let value: Value = serde_json::from_str(
            &document
                .inspect_vm_state()
                .to_pretty_json()
                .expect("native-v2 inspection should serialize"),
        )
        .expect("native-v2 inspection should parse");
        assert_eq!(value["machine"]["fdt"]["checksum"], "<redacted>");
    }
}

#[test]
fn confidential_memory_identity_changes_only_its_fingerprint() {
    let left_document = inspection_native_v1_document(1);
    let right_document = inspection_native_v1_document(1);
    let left_vcpus = left_document
        .inspect_vcpu_states()
        .to_pretty_json()
        .expect("left vCPU inspection should serialize");
    let right_vcpus = right_document
        .inspect_vcpu_states()
        .to_pretty_json()
        .expect("right vCPU inspection should serialize");
    assert_eq!(left_vcpus, right_vcpus);

    let mut left: Value = serde_json::from_str(
        &left_document
            .inspect_vm_state()
            .to_pretty_json()
            .expect("left VM inspection should serialize"),
    )
    .expect("left VM inspection should parse");
    let mut right: Value = serde_json::from_str(
        &right_document
            .inspect_vm_state()
            .to_pretty_json()
            .expect("right VM inspection should serialize"),
    )
    .expect("right VM inspection should parse");
    assert_ne!(
        left["memory"]["binding_identity"]["digest"],
        right["memory"]["binding_identity"]["digest"]
    );
    assert_eq!(
        left["memory"]["binding_identity"]["byte_length"],
        right["memory"]["binding_identity"]["byte_length"]
    );
    left["memory"]["binding_identity"]["digest"] = Value::String("<digest>".to_owned());
    right["memory"]["binding_identity"]["digest"] = Value::String("<digest>".to_owned());
    assert_eq!(left, right);
}

#[test]
fn authority_fields_are_literal_redactions_without_fingerprints() {
    for document in inspection_document_fixtures() {
        let json = document
            .inspect_vm_state()
            .to_pretty_json()
            .expect("VM inspection should serialize");
        let value: Value = serde_json::from_str(&json).expect("VM inspection should parse");
        assert_authority_redaction(&value);
    }
}

#[test]
fn seeded_sensitive_values_are_absent_from_both_views() {
    for document in inspection_document_fixtures() {
        let vcpus = document
            .inspect_vcpu_states()
            .to_pretty_json()
            .expect("vCPU inspection should serialize");
        let vm = document
            .inspect_vm_state()
            .to_pretty_json()
            .expect("VM inspection should serialize");

        for marker in [
            "sensitive-gic-state",
            "/fixture/kernel",
            "/fixture/initrd",
            "secret=fixture",
            "serial-log",
            "vmnet:shared",
            "/tmp/bangbang-vsock-inactive.sock",
            "\"abc\"",
        ] {
            assert!(!vcpus.contains(marker), "vCPU view leaked {marker}");
            assert!(!vm.contains(marker), "VM view leaked {marker}");
        }

        let (image_id, checksum) = raw_memory_identity_markers(&document);
        assert!(!vcpus.contains(&image_id));
        assert!(!vm.contains(&image_id));
        assert!(!vcpus.contains(&checksum));
        assert!(!vm.contains(&checksum));
    }
}

#[test]
fn fingerprints_are_algorithm_labelled_fixed_lowercase_sha256() {
    let documents = inspection_document_fixtures();
    let mut count = 0;
    for document in &documents {
        for json in [
            document
                .inspect_vcpu_states()
                .to_pretty_json()
                .expect("vCPU inspection should serialize"),
            document
                .inspect_vm_state()
                .to_pretty_json()
                .expect("VM inspection should serialize"),
        ] {
            let value: Value = serde_json::from_str(&json).expect("inspection should parse");
            count += assert_fingerprint_shape(&value);
        }
    }
    assert!(count > documents.len());
}

#[test]
fn errors_and_view_debug_do_not_expose_values() {
    let document = inspection_native_v1_document(1);
    let error = format_pretty_json_with_limit(&"sensitive-output-marker", 1)
        .expect_err("injected low limit should fail");
    let display = error.to_string();
    let debug = format!("{error:?}");
    assert!(!display.contains("sensitive-output-marker"));
    assert!(!debug.contains("sensitive-output-marker"));

    let vcpu_debug = format!("{:?}", document.inspect_vcpu_states());
    let vm_debug = format!("{:?}", document.inspect_vm_state());
    for marker in ["sensitive-gic-state", "rootfs.img", "serial-log"] {
        assert!(!vcpu_debug.contains(marker));
        assert!(!vm_debug.contains(marker));
    }
}

fn assert_fixed_lowercase_hex(value: &Value) {
    match value {
        Value::Array(values) => values.iter().for_each(assert_fixed_lowercase_hex),
        Value::Object(values) => values.values().for_each(assert_fixed_lowercase_hex),
        Value::String(value) if value.starts_with("0x") => {
            assert!(matches!(value.len(), 4 | 6 | 10 | 18 | 34));
            assert!(
                value[2..]
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            );
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn assert_authority_redaction(value: &Value) {
    match value {
        Value::Array(values) => values.iter().for_each(assert_authority_redaction),
        Value::Object(values) => {
            if values.contains_key("capacity_sectors") && values.contains_key("device_id") {
                assert_eq!(
                    values["device_id"], "<redacted>",
                    "inode-derived block device ID was not literally redacted"
                );
            }
            for (key, value) in values {
                if key.contains("selector")
                    || key.ends_with("_path")
                    || key.ends_with("_authority")
                    || matches!(
                        key.as_str(),
                        "path" | "backing_identity" | "host_local_port_cursor"
                    )
                    || (key == "arguments" && !value.is_array())
                {
                    assert!(
                        value.is_null() || value.as_str() == Some("<redacted>"),
                        "authority field {key} was not literally redacted: {value}"
                    );
                }
                assert_authority_redaction(value);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn assert_fingerprint_shape(value: &Value) -> usize {
    match value {
        Value::Array(values) => values.iter().map(assert_fingerprint_shape).sum(),
        Value::Object(values)
            if values.get("algorithm") == Some(&Value::String("sha256".to_owned())) =>
        {
            assert_eq!(values.len(), 3);
            assert!(values["byte_length"].as_u64().is_some());
            let digest = values["digest"]
                .as_str()
                .expect("fingerprint digest should be a string");
            assert_eq!(digest.len(), 64);
            assert!(
                digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            );
            1
        }
        Value::Object(values) => values.values().map(assert_fingerprint_shape).sum(),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => 0,
    }
}

fn raw_memory_identity_markers(document: &HvfNativeSnapshotDocument) -> (String, String) {
    let (image_id, checksum) = match &document.state {
        HvfNativeSnapshotDocumentState::V1(bundle) => {
            let binding = bundle.commit_record().memory_binding();
            (*binding.image_id().as_bytes(), binding.checksum())
        }
        state => {
            let binding = platform_v2(state)
                .expect("native-v2 fixture should retain a platform")
                .memory();
            (*binding.image_id().as_bytes(), binding.metadata_checksum())
        }
    };
    let image_id = image_id
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    (image_id, format!("{checksum:016x}"))
}

fn assert_root_field_order(json: &str, fields: &[&str]) {
    let mut previous = None;
    for field in fields {
        let position = json
            .find(&format!("\"{field}\":"))
            .expect("root field should be present");
        if let Some(previous) = previous {
            assert!(position > previous, "root field {field} is out of order");
        }
        previous = Some(position);
    }
}

fn assert_serializes_within_public_ceiling<T: Serialize>(value: &T) -> Value {
    let json = format_pretty_json(value, HVF_NATIVE_SNAPSHOT_INSPECTION_MAX_JSON_BYTES)
        .expect("maximum inspection collection should serialize");
    assert!(json.len() <= HVF_NATIVE_SNAPSHOT_INSPECTION_MAX_JSON_BYTES);
    serde_json::from_str(&json).expect("maximum inspection collection should parse")
}

fn memory_binding_for_ranges(ranges: Vec<GuestMemoryRange>) -> SnapshotV2MemoryBinding {
    let layout = GuestMemoryLayout::new(ranges).expect("maximum memory layout should validate");
    let memory = GuestMemory::allocate(&layout).expect("maximum memory layout should allocate");
    write_snapshot_v2_memory_image_with_compatibility_version(
        &memory,
        &mut DiscardSeek::default(),
        NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION,
    )
    .expect("maximum memory binding should encode")
}

#[derive(Default)]
struct DiscardSeek {
    position: u64,
    length: u64,
}

impl Write for DiscardSeek {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.position = self
            .position
            .checked_add(u64::try_from(bytes.len()).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "write length does not fit u64")
            })?)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "write position overflow")
            })?;
        self.length = self.length.max(self.position);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for DiscardSeek {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.position = match position {
            SeekFrom::Start(position) => position,
            SeekFrom::End(offset) => checked_seek(self.length, offset)?,
            SeekFrom::Current(offset) => checked_seek(self.position, offset)?,
        };
        Ok(self.position)
    }
}

fn checked_seek(base: u64, offset: i64) -> io::Result<u64> {
    if offset >= 0 {
        base.checked_add(offset.unsigned_abs())
    } else {
        base.checked_sub(offset.unsigned_abs())
    }
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek position overflow"))
}
