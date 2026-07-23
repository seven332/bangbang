use crate::snapshot_format::{
    NativeSnapshotFormatError, NativeSnapshotState, decode_native_snapshot_state,
    encode_snapshot_envelope,
};

use super::*;

const TEST_REQUIRED_FEATURE_CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        id: 10,
        introduced_minor: 0,
    },
    CatalogEntry {
        id: 20,
        introduced_minor: 0,
    },
];
const TEST_SEMANTIC_COMPONENT_CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        id: 1,
        introduced_minor: 0,
    },
    CatalogEntry {
        id: 2,
        introduced_minor: 0,
    },
];
const EMPTY_V2_FIXTURE: [u8; 72] = [
    0x42, 0x41, 0x4e, 0x47, 0x56, 0x32, 0x41, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x48, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x5e, 0xf9, 0x9d, 0x5f, 0xee, 0xdf, 0xc8, 0x6c,
];
const FIRECRACKER_AARCH64_PREFIX: [u8; 9] = [0x00, 0x00, 0x00, 0xaa, 0xaa, 0x84, 0x19, 0x10, 0x07];
const FIRECRACKER_X86_64_PREFIX: [u8; 9] = [0x00, 0x00, 0x00, 0x64, 0x86, 0x84, 0x19, 0x10, 0x07];

#[test]
fn empty_foundation_encoding_matches_immutable_fixture() {
    let encoded = encode_snapshot_v2_state(&[], &[]).expect("empty foundation should encode");

    assert_eq!(encoded, EMPTY_V2_FIXTURE);
    let decoded = decode_snapshot_v2_state(&EMPTY_V2_FIXTURE)
        .expect("empty foundation fixture should decode");
    assert_eq!(decoded.metadata().version(), NATIVE_V2_SNAPSHOT_VERSION);
    assert_eq!(
        decoded.metadata().architecture(),
        SnapshotArchitecture::Arm64
    );
    assert_eq!(decoded.metadata().required_feature_count(), 0);
    assert_eq!(decoded.metadata().component_count(), 0);
    assert_eq!(decoded.metadata().total_length(), 72);
    assert_eq!(
        decoded.metadata().integrity(),
        SnapshotIntegrity::Crc64Jones
    );
    assert_eq!(
        decoded.component_payload_offset(),
        NATIVE_V2_SNAPSHOT_HEADER_BYTES
    );
    assert_eq!(decoded.required_features().len(), 0);
    assert_eq!(decoded.components().len(), 0);
}

#[test]
fn canonical_test_directory_round_trips_borrowed_components() {
    let components = test_components();
    let encoded = encode_test_state(&[10, 20], &components);
    let decoded = decode_test_state(&encoded).expect("test state should decode");

    assert_eq!(
        decoded.required_features().collect::<Vec<_>>(),
        vec![10, 20]
    );
    let decoded_components = decoded.components().collect::<Vec<_>>();
    assert_eq!(decoded_components, components);
    for component in decoded_components {
        let payload_range = encoded.as_ptr_range();
        let payload_pointer = component.payload().as_ptr();
        assert!(payload_range.start <= payload_pointer && payload_pointer < payload_range.end);
        assert_eq!(decoded.component(component.key()), Some(component));
    }
}

#[test]
fn production_catalog_accepts_nonsemantic_extensions_only() {
    let nonsemantic = [SnapshotV2Component::new(
        SnapshotV2ComponentKey::new(77, 3),
        SnapshotV2ComponentDisposition::NonSemantic,
        b"diagnostic",
    )];
    let encoded =
        encode_snapshot_v2_state(&[], &nonsemantic).expect("nonsemantic extension should encode");
    let decoded = decode_snapshot_v2_state(&encoded).expect("nonsemantic extension should decode");
    assert_eq!(decoded.components().collect::<Vec<_>>(), nonsemantic);

    let semantic = [SnapshotV2Component::new(
        SnapshotV2ComponentKey::new(1, 3),
        SnapshotV2ComponentDisposition::Semantic,
        b"semantic",
    )];
    assert!(matches!(
        encode_snapshot_v2_state(&[], &semantic),
        Err(SnapshotV2EncodeError::UnknownSemanticComponent)
    ));

    let encoded_semantic = encode_test_state(&[], &semantic);
    assert_eq!(
        decode_snapshot_v2_state(&encoded_semantic),
        Err(SnapshotV2DecodeError::UnknownSemanticComponent)
    );
}

#[test]
fn family_dispatch_preserves_v1_recognizes_v2_and_rejects_firecracker() {
    let v1 =
        encode_snapshot_envelope(b"private-v1-state").expect("native-v1 fixture should encode");
    let decoded_v1 = decode_native_snapshot_state(&v1).expect("native-v1 fixture should dispatch");
    assert!(matches!(decoded_v1, NativeSnapshotState::V1(_)));
    assert_eq!(decoded_v1.version().to_string(), "1.0.0");
    assert_eq!(decoded_v1.architecture(), SnapshotArchitecture::Arm64);

    let decoded_v2 =
        decode_native_snapshot_state(&EMPTY_V2_FIXTURE).expect("native-v2 fixture should dispatch");
    assert!(matches!(decoded_v2, NativeSnapshotState::V2(_)));
    assert_eq!(decoded_v2.version(), NATIVE_V2_SNAPSHOT_VERSION);

    for prefix in [
        FIRECRACKER_AARCH64_PREFIX.as_slice(),
        FIRECRACKER_X86_64_PREFIX.as_slice(),
    ] {
        assert_eq!(
            decode_native_snapshot_state(prefix),
            Err(NativeSnapshotFormatError::IncompatibleFirecrackerFormat)
        );
    }
    assert_eq!(
        decode_native_snapshot_state(b"unrecognized-state"),
        Err(NativeSnapshotFormatError::IncompatibleFormat)
    );
}

#[test]
fn rejects_every_minimum_length_truncation() {
    for actual in 0..EMPTY_V2_FIXTURE.len() {
        let truncated = EMPTY_V2_FIXTURE
            .get(..actual)
            .expect("fixture prefix should exist");
        assert_eq!(
            decode_snapshot_v2_state(truncated),
            Err(SnapshotV2DecodeError::Truncated {
                minimum: NATIVE_V2_SNAPSHOT_HEADER_BYTES + NATIVE_V2_SNAPSHOT_INTEGRITY_BYTES,
                actual,
            })
        );
    }
}

#[test]
fn version_policy_rejects_major_and_newer_minor_but_accepts_patch() {
    let major = with_u16_field(&EMPTY_V2_FIXTURE, VERSION_MAJOR_OFFSET, 3);
    assert_eq!(
        decode_snapshot_v2_state(&major),
        Err(SnapshotV2DecodeError::UnsupportedVersion {
            found: SnapshotFormatVersion::new(3, 0, 0),
            supported: NATIVE_V2_SNAPSHOT_VERSION,
        })
    );

    let minor = with_u16_field(&EMPTY_V2_FIXTURE, VERSION_MINOR_OFFSET, 1);
    assert_eq!(
        decode_snapshot_v2_state(&minor),
        Err(SnapshotV2DecodeError::UnsupportedVersion {
            found: SnapshotFormatVersion::new(2, 1, 0),
            supported: NATIVE_V2_SNAPSHOT_VERSION,
        })
    );

    let patch = with_u16_field_and_checksum(&EMPTY_V2_FIXTURE, VERSION_PATCH_OFFSET, u16::MAX);
    let decoded = decode_snapshot_v2_state(&patch).expect("patch should be nonsemantic");
    assert_eq!(
        decoded.metadata().version(),
        SnapshotFormatVersion::new(2, 0, u16::MAX)
    );
}

#[test]
fn rejects_noncanonical_fixed_header_fields() {
    let mut invalid_magic = EMPTY_V2_FIXTURE;
    *invalid_magic
        .get_mut(MAGIC_OFFSET)
        .expect("fixture magic should exist") ^= 0x80;
    assert_eq!(
        decode_snapshot_v2_state(&invalid_magic),
        Err(SnapshotV2DecodeError::InvalidMagic)
    );

    for (offset, value) in [(HEADER_BYTES_OFFSET, 63_u16), (HEADER_BYTES_OFFSET, 65_u16)] {
        let encoded = with_u16_field_and_checksum(&EMPTY_V2_FIXTURE, offset, value);
        assert_eq!(
            decode_snapshot_v2_state(&encoded),
            Err(SnapshotV2DecodeError::InvalidHeader)
        );
    }
    for offset in [FLAGS_OFFSET, RESERVED_OFFSET] {
        let encoded = with_u32_field_and_checksum(&EMPTY_V2_FIXTURE, offset, 1);
        assert_eq!(
            decode_snapshot_v2_state(&encoded),
            Err(SnapshotV2DecodeError::InvalidHeader)
        );
    }
    for offset in [
        REQUIRED_FEATURE_OFFSET_OFFSET,
        COMPONENT_DIRECTORY_OFFSET_OFFSET,
        COMPONENT_PAYLOAD_OFFSET_OFFSET,
    ] {
        let encoded = with_u64_field_and_checksum(&EMPTY_V2_FIXTURE, offset, 63);
        assert_eq!(
            decode_snapshot_v2_state(&encoded),
            Err(SnapshotV2DecodeError::InvalidHeader)
        );
    }
}

#[test]
fn rejects_count_caps_before_table_walk() {
    let required = with_u32_field(
        &EMPTY_V2_FIXTURE,
        REQUIRED_FEATURE_COUNT_OFFSET,
        u32::try_from(NATIVE_V2_SNAPSHOT_MAX_REQUIRED_FEATURES)
            .expect("feature cap should fit u32")
            + 1,
    );
    assert_eq!(
        decode_snapshot_v2_state(&required),
        Err(SnapshotV2DecodeError::CountOutOfBounds {
            count: u64::try_from(NATIVE_V2_SNAPSHOT_MAX_REQUIRED_FEATURES)
                .expect("feature cap should fit u64")
                + 1,
            maximum: NATIVE_V2_SNAPSHOT_MAX_REQUIRED_FEATURES,
        })
    );

    let components = with_u32_field(
        &EMPTY_V2_FIXTURE,
        COMPONENT_COUNT_OFFSET,
        u32::try_from(NATIVE_V2_SNAPSHOT_MAX_COMPONENTS).expect("component cap should fit u32") + 1,
    );
    assert_eq!(
        decode_snapshot_v2_state(&components),
        Err(SnapshotV2DecodeError::CountOutOfBounds {
            count: u64::try_from(NATIVE_V2_SNAPSHOT_MAX_COMPONENTS)
                .expect("component cap should fit u64")
                + 1,
            maximum: NATIVE_V2_SNAPSHOT_MAX_COMPONENTS,
        })
    );
}

#[test]
fn rejects_length_mismatch_limit_and_integrity() {
    let mismatch = with_u64_field(&EMPTY_V2_FIXTURE, TOTAL_LENGTH_OFFSET, 73);
    assert_eq!(
        decode_snapshot_v2_state(&mismatch),
        Err(SnapshotV2DecodeError::LengthMismatch {
            declared: 73,
            actual: EMPTY_V2_FIXTURE.len(),
        })
    );

    let over_limit = with_u64_field(
        &EMPTY_V2_FIXTURE,
        TOTAL_LENGTH_OFFSET,
        u64::try_from(NATIVE_V2_SNAPSHOT_MAX_FILE_BYTES).expect("file cap should fit u64") + 1,
    );
    assert_eq!(
        decode_snapshot_v2_state(&over_limit),
        Err(SnapshotV2DecodeError::FileTooLarge {
            length: u64::try_from(NATIVE_V2_SNAPSHOT_MAX_FILE_BYTES)
                .expect("file cap should fit u64")
                + 1,
            maximum: NATIVE_V2_SNAPSHOT_MAX_FILE_BYTES,
        })
    );

    let mut trailing = EMPTY_V2_FIXTURE.to_vec();
    trailing.push(0);
    assert_eq!(
        decode_snapshot_v2_state(&trailing),
        Err(SnapshotV2DecodeError::LengthMismatch {
            declared: u64::try_from(EMPTY_V2_FIXTURE.len()).expect("fixture length should fit u64"),
            actual: EMPTY_V2_FIXTURE.len() + 1,
        })
    );

    let mut corrupt = EMPTY_V2_FIXTURE;
    let checksum_byte = corrupt.last_mut().expect("fixture checksum should exist");
    *checksum_byte ^= 0x80;
    assert_eq!(
        decode_snapshot_v2_state(&corrupt),
        Err(SnapshotV2DecodeError::IntegrityMismatch)
    );
}

#[test]
fn required_feature_inventory_rejects_zero_order_duplicates_and_unknowns() {
    let components = test_components();
    let fixture = encode_test_state(&[10, 20], &components);
    let feature_offset = NATIVE_V2_SNAPSHOT_HEADER_BYTES;

    let zero = with_u32_field_and_checksum(&fixture, feature_offset, 0);
    assert_eq!(
        decode_test_state(&zero),
        Err(SnapshotV2DecodeError::InvalidRequiredFeatureInventory)
    );

    let duplicate = with_u32_field_and_checksum(&fixture, feature_offset + size_of::<u32>(), 10);
    assert_eq!(
        decode_test_state(&duplicate),
        Err(SnapshotV2DecodeError::InvalidRequiredFeatureInventory)
    );

    let first_twenty = with_u32_field(&fixture, feature_offset, 20);
    let reordered =
        with_u32_field_and_checksum(&first_twenty, feature_offset + size_of::<u32>(), 10);
    assert_eq!(
        decode_test_state(&reordered),
        Err(SnapshotV2DecodeError::InvalidRequiredFeatureInventory)
    );

    let unknown = with_u32_field_and_checksum(&fixture, feature_offset, 30);
    assert_eq!(
        decode_test_state(&unknown),
        Err(SnapshotV2DecodeError::UnknownRequiredFeature)
    );

    assert_eq!(
        decode_snapshot_v2_state(&fixture),
        Err(SnapshotV2DecodeError::UnknownRequiredFeature)
    );
}

#[test]
fn component_directory_rejects_identity_flags_reserved_and_ranges() {
    let components = test_components();
    let fixture = encode_test_state(&[10, 20], &components);
    let directory = NATIVE_V2_SNAPSHOT_HEADER_BYTES + 2 * size_of::<u32>();
    let second = directory + NATIVE_V2_COMPONENT_DIRECTORY_ENTRY_BYTES;
    let third = second + NATIVE_V2_COMPONENT_DIRECTORY_ENTRY_BYTES;
    let payload = directory + 3 * NATIVE_V2_COMPONENT_DIRECTORY_ENTRY_BYTES;

    for (case, encoded) in [
        (
            "zero kind",
            with_u32_field_and_checksum(&fixture, directory + COMPONENT_KIND_OFFSET, 0),
        ),
        (
            "unknown flags",
            with_u32_field_and_checksum(&fixture, directory + COMPONENT_FLAGS_OFFSET, 2),
        ),
        (
            "nonzero reserved",
            with_u32_field_and_checksum(&fixture, directory + COMPONENT_RESERVED_OFFSET, 1),
        ),
        (
            "payload gap",
            with_u64_field_and_checksum(
                &fixture,
                directory + COMPONENT_PAYLOAD_OFFSET,
                u64::try_from(payload + 1).expect("fixture offset should fit u64"),
            ),
        ),
        (
            "payload overlap",
            with_u64_field_and_checksum(
                &fixture,
                second + COMPONENT_PAYLOAD_OFFSET,
                u64::try_from(payload).expect("fixture offset should fit u64"),
            ),
        ),
        (
            "zero payload length",
            with_u64_field_and_checksum(&fixture, directory + COMPONENT_PAYLOAD_LENGTH_OFFSET, 0),
        ),
        (
            "payload length wrap",
            with_u64_field_and_checksum(
                &fixture,
                directory + COMPONENT_PAYLOAD_LENGTH_OFFSET,
                u64::MAX,
            ),
        ),
        (
            "unclaimed trailing payload byte",
            with_u64_field_and_checksum(&fixture, third + COMPONENT_PAYLOAD_LENGTH_OFFSET, 9),
        ),
    ] {
        assert!(
            matches!(
                decode_test_state(&encoded),
                Err(SnapshotV2DecodeError::InvalidComponentDirectory
                    | SnapshotV2DecodeError::LengthOverflow)
            ),
            "{case}"
        );
    }

    let duplicate_kind = with_u32_field(&fixture, second + COMPONENT_KIND_OFFSET, 1);
    let duplicate_key =
        with_u32_field_and_checksum(&duplicate_kind, second + COMPONENT_INSTANCE_OFFSET, 0);
    assert_eq!(
        decode_test_state(&duplicate_key),
        Err(SnapshotV2DecodeError::InvalidComponentDirectory)
    );

    let first_kind = with_u32_field(&fixture, directory + COMPONENT_KIND_OFFSET, 2);
    let descending_key =
        with_u32_field_and_checksum(&first_kind, directory + COMPONENT_INSTANCE_OFFSET, 2);
    assert_eq!(
        decode_test_state(&descending_key),
        Err(SnapshotV2DecodeError::InvalidComponentDirectory)
    );
}

#[test]
fn unknown_semantic_component_rejects_without_affecting_nonsemantic_extension() {
    let components = test_components();
    let fixture = encode_test_state(&[10, 20], &components);
    let directory = NATIVE_V2_SNAPSHOT_HEADER_BYTES + 2 * size_of::<u32>();
    let third = directory + 2 * NATIVE_V2_COMPONENT_DIRECTORY_ENTRY_BYTES;

    let unknown_semantic = with_u32_field_and_checksum(&fixture, third + COMPONENT_FLAGS_OFFSET, 0);
    assert_eq!(
        decode_test_state(&unknown_semantic),
        Err(SnapshotV2DecodeError::UnknownSemanticComponent)
    );

    let decoded = decode_test_state(&fixture).expect("nonsemantic extension should decode");
    assert_eq!(
        decoded
            .component(SnapshotV2ComponentKey::new(99, 0))
            .expect("extension should exist")
            .disposition(),
        SnapshotV2ComponentDisposition::NonSemantic
    );
}

#[test]
fn encoder_rejects_noncanonical_and_undeclared_inputs() {
    assert!(matches!(
        encode_snapshot_v2_state(&[0], &[]),
        Err(SnapshotV2EncodeError::InvalidRequiredFeatureInventory)
    ));
    assert!(matches!(
        encode_snapshot_v2_state(&[10], &[]),
        Err(SnapshotV2EncodeError::UnknownRequiredFeature)
    ));

    let first = SnapshotV2Component::new(
        SnapshotV2ComponentKey::new(2, 0),
        SnapshotV2ComponentDisposition::NonSemantic,
        b"first",
    );
    let second = SnapshotV2Component::new(
        SnapshotV2ComponentKey::new(1, 0),
        SnapshotV2ComponentDisposition::NonSemantic,
        b"second",
    );
    assert!(matches!(
        encode_snapshot_v2_state(&[], &[first, second]),
        Err(SnapshotV2EncodeError::InvalidComponentDirectory)
    ));
    let empty = SnapshotV2Component::new(
        SnapshotV2ComponentKey::new(1, 0),
        SnapshotV2ComponentDisposition::NonSemantic,
        b"",
    );
    assert!(matches!(
        encode_snapshot_v2_state(&[], &[empty]),
        Err(SnapshotV2EncodeError::InvalidComponentDirectory)
    ));
}

#[test]
fn encoder_rejects_count_and_file_limits() {
    let required_features = vec![0; NATIVE_V2_SNAPSHOT_MAX_REQUIRED_FEATURES + 1];
    assert!(matches!(
        encode_snapshot_v2_state(&required_features, &[]),
        Err(SnapshotV2EncodeError::CountOutOfBounds {
            maximum: NATIVE_V2_SNAPSHOT_MAX_REQUIRED_FEATURES,
            ..
        })
    ));

    let component = SnapshotV2Component::new(
        SnapshotV2ComponentKey::new(1, 0),
        SnapshotV2ComponentDisposition::NonSemantic,
        b"x",
    );
    let components = vec![component; NATIVE_V2_SNAPSHOT_MAX_COMPONENTS + 1];
    assert!(matches!(
        encode_snapshot_v2_state(&[], &components),
        Err(SnapshotV2EncodeError::CountOutOfBounds {
            maximum: NATIVE_V2_SNAPSHOT_MAX_COMPONENTS,
            ..
        })
    ));

    let oversized_payload = vec![0; NATIVE_V2_SNAPSHOT_MAX_FILE_BYTES];
    let oversized = [SnapshotV2Component::new(
        SnapshotV2ComponentKey::new(1, 0),
        SnapshotV2ComponentDisposition::NonSemantic,
        &oversized_payload,
    )];
    assert!(matches!(
        encode_snapshot_v2_state(&[], &oversized),
        Err(SnapshotV2EncodeError::FileTooLarge {
            maximum: NATIVE_V2_SNAPSHOT_MAX_FILE_BYTES,
            ..
        })
    ));
}

#[test]
fn encoder_allocation_failure_returns_no_partial_value() {
    let allocation_error = Vec::<u8>::new()
        .try_reserve_exact(usize::MAX)
        .expect_err("impossible allocation should fail");
    let result = encode_snapshot_v2_state_with_catalog_and_reserve(
        &[],
        &[],
        NATIVE_V2_SNAPSHOT_VERSION,
        PRODUCTION_REQUIRED_FEATURES,
        PRODUCTION_SEMANTIC_COMPONENTS,
        move |_, _| Err(allocation_error),
    );

    assert!(matches!(
        result,
        Err(SnapshotV2EncodeError::AllocationFailed { .. })
    ));
}

#[test]
fn diagnostics_redact_identifiers_payloads_and_format_magic() {
    let sensitive_payload = b"private-component-payload";
    let sensitive_kind = 424_242;
    let catalog = [CatalogEntry {
        id: sensitive_kind,
        introduced_minor: 0,
    }];
    let components = [SnapshotV2Component::new(
        SnapshotV2ComponentKey::new(sensitive_kind, 313_131),
        SnapshotV2ComponentDisposition::Semantic,
        sensitive_payload,
    )];
    let encoded = encode_snapshot_v2_state_with_catalog_and_reserve(
        &[],
        &components,
        NATIVE_V2_SNAPSHOT_VERSION,
        &[],
        &catalog,
        Vec::try_reserve_exact,
    )
    .expect("sensitive fixture should encode");
    let state =
        decode_snapshot_v2_state_with_catalog(&encoded, NATIVE_V2_SNAPSHOT_VERSION, &[], &catalog)
            .expect("sensitive fixture should decode");

    for diagnostic in [
        format!("{state:?}"),
        format!("{:?}", state.components()),
        format!(
            "{:?}",
            state.components().next().expect("component should exist")
        ),
        SnapshotV2DecodeError::UnknownSemanticComponent.to_string(),
        format!("{:?}", SnapshotV2DecodeError::UnknownSemanticComponent),
        NativeSnapshotFormatError::IncompatibleFirecrackerFormat.to_string(),
    ] {
        assert!(!diagnostic.contains("424242"));
        assert!(!diagnostic.contains("313131"));
        assert!(!diagnostic.contains("private-component-payload"));
        assert!(!diagnostic.contains("BANGV2A"));
        assert!(!diagnostic.contains("07101984"));
    }
}

#[test]
fn catalog_introduction_minor_is_enforced() {
    let catalog = [CatalogEntry {
        id: 10,
        introduced_minor: 1,
    }];
    assert!(matches!(
        encode_snapshot_v2_state_with_catalog_and_reserve(
            &[10],
            &[],
            NATIVE_V2_SNAPSHOT_VERSION,
            &catalog,
            &[],
            Vec::try_reserve_exact,
        ),
        Err(SnapshotV2EncodeError::UnknownRequiredFeature)
    ));

    let version_one = SnapshotFormatVersion::new(2, 1, 0);
    let encoded = encode_snapshot_v2_state_with_catalog_and_reserve(
        &[10],
        &[],
        version_one,
        &catalog,
        &[],
        Vec::try_reserve_exact,
    )
    .expect("minor-one feature should encode");
    let decoded = decode_snapshot_v2_state_with_catalog(&encoded, version_one, &catalog, &[])
        .expect("minor-one feature should decode");
    assert_eq!(decoded.required_features().collect::<Vec<_>>(), vec![10]);
    assert!(matches!(
        decode_snapshot_v2_state(&encoded),
        Err(SnapshotV2DecodeError::UnsupportedVersion { .. })
    ));
}

fn test_components() -> [SnapshotV2Component<'static>; 3] {
    [
        SnapshotV2Component::new(
            SnapshotV2ComponentKey::new(1, 0),
            SnapshotV2ComponentDisposition::Semantic,
            b"machine",
        ),
        SnapshotV2Component::new(
            SnapshotV2ComponentKey::new(2, 1),
            SnapshotV2ComponentDisposition::Semantic,
            b"platform",
        ),
        SnapshotV2Component::new(
            SnapshotV2ComponentKey::new(99, 0),
            SnapshotV2ComponentDisposition::NonSemantic,
            b"diagnostic",
        ),
    ]
}

fn encode_test_state(required_features: &[u32], components: &[SnapshotV2Component<'_>]) -> Vec<u8> {
    encode_snapshot_v2_state_with_catalog_and_reserve(
        required_features,
        components,
        NATIVE_V2_SNAPSHOT_VERSION,
        TEST_REQUIRED_FEATURE_CATALOG,
        TEST_SEMANTIC_COMPONENT_CATALOG,
        Vec::try_reserve_exact,
    )
    .expect("test state should encode")
}

fn decode_test_state(bytes: &[u8]) -> Result<SnapshotV2State<'_>, SnapshotV2DecodeError> {
    decode_snapshot_v2_state_with_catalog(
        bytes,
        NATIVE_V2_SNAPSHOT_VERSION,
        TEST_REQUIRED_FEATURE_CATALOG,
        TEST_SEMANTIC_COMPONENT_CATALOG,
    )
}

fn with_u16_field(bytes: &[u8], offset: usize, value: u16) -> Vec<u8> {
    let mut encoded = bytes.to_vec();
    replace_field(&mut encoded, offset, &value.to_le_bytes());
    encoded
}

fn with_u16_field_and_checksum(bytes: &[u8], offset: usize, value: u16) -> Vec<u8> {
    let mut encoded = with_u16_field(bytes, offset, value);
    replace_checksum(&mut encoded);
    encoded
}

fn with_u32_field(bytes: &[u8], offset: usize, value: u32) -> Vec<u8> {
    let mut encoded = bytes.to_vec();
    replace_field(&mut encoded, offset, &value.to_le_bytes());
    encoded
}

fn with_u32_field_and_checksum(bytes: &[u8], offset: usize, value: u32) -> Vec<u8> {
    let mut encoded = with_u32_field(bytes, offset, value);
    replace_checksum(&mut encoded);
    encoded
}

fn with_u64_field(bytes: &[u8], offset: usize, value: u64) -> Vec<u8> {
    let mut encoded = bytes.to_vec();
    replace_field(&mut encoded, offset, &value.to_le_bytes());
    encoded
}

fn with_u64_field_and_checksum(bytes: &[u8], offset: usize, value: u64) -> Vec<u8> {
    let mut encoded = with_u64_field(bytes, offset, value);
    replace_checksum(&mut encoded);
    encoded
}

fn replace_field(bytes: &mut [u8], offset: usize, value: &[u8]) {
    let end = offset + value.len();
    bytes
        .get_mut(offset..end)
        .expect("fixture field should exist")
        .copy_from_slice(value);
}

fn replace_checksum(bytes: &mut [u8]) {
    let checksum_offset = bytes.len() - NATIVE_V2_SNAPSHOT_INTEGRITY_BYTES;
    let checksum = crc64(
        0,
        bytes
            .get(..checksum_offset)
            .expect("checksummed fixture bytes should exist"),
    );
    bytes
        .get_mut(checksum_offset..)
        .expect("fixture checksum should exist")
        .copy_from_slice(&checksum.to_le_bytes());
}
