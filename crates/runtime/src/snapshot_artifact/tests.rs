use super::*;
#[cfg(target_os = "macos")]
use crate::memory::{
    GuestAddress, GuestMemoryBacking, GuestMemoryLayout, GuestMemoryRange,
    GuestMemoryRegionBacking, aarch64,
};
#[cfg(target_os = "macos")]
use crate::serial::{CaptureReadySerialState, SerialConfig, SerialMmioDevice};
#[cfg(target_os = "macos")]
use crate::snapshot_balloon_v2_9::NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION;
#[cfg(target_os = "macos")]
use crate::snapshot_device_v2::{
    NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION, SnapshotV2DeviceTransportKind,
};
#[cfg(target_os = "macos")]
use crate::snapshot_device_v2_6::NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION;
#[cfg(target_os = "macos")]
use crate::snapshot_entropy_v2_8::NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION;
#[cfg(target_os = "macos")]
use crate::snapshot_format_v2::{
    NATIVE_V2_BALLOON_COMPONENT_KEY, NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY,
    NATIVE_V2_ENTROPY_COMPONENT_KEY, NATIVE_V2_MEMORY_COMPONENT_KEY,
    NATIVE_V2_MEMORY_HOTPLUG_COMPONENT_KEY, NATIVE_V2_SERIAL_COMPONENT_KEY, SnapshotV2Component,
    SnapshotV2ComponentDisposition, SnapshotV2ComponentKey,
    encode_snapshot_v2_state_with_compatibility_version,
};
#[cfg(target_os = "macos")]
use crate::snapshot_memory_hotplug_v2_10::{
    NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION, SnapshotV2MemoryHotplugState,
};
#[cfg(target_os = "macos")]
use crate::snapshot_memory_v2::{
    write_snapshot_v2_memory_image, write_snapshot_v2_memory_image_with_compatibility_version,
};
#[cfg(target_os = "macos")]
use crate::snapshot_serial_v2_7::{
    NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION, SnapshotV2SerialState,
};

#[cfg(target_os = "macos")]
use std::fs;
#[cfg(target_os = "macos")]
use std::io::{BufRead, BufReader, Cursor, Seek, SeekFrom, Write};
#[cfg(target_os = "macos")]
use std::os::fd::AsFd;
#[cfg(target_os = "macos")]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
#[cfg(target_os = "macos")]
use std::os::unix::fs::{FileExt, MetadataExt, PermissionsExt, symlink};
#[cfg(target_os = "macos")]
use std::os::unix::net::UnixListener;
#[cfg(target_os = "macos")]
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(target_os = "macos")]
use std::sync::{Arc, Mutex};

#[cfg(target_os = "macos")]
const TEST_MEMORY_BYTES: usize = 16 * 1024;

#[test]
fn paths_and_load_results_redact_host_paths_and_memory() {
    let paths = SnapshotArtifactPaths::new(
        "/sentinel/private/state.snap",
        "/sentinel/private/memory.snap",
    );
    let debug = format!("{paths:?}");
    assert!(debug.contains(REDACTED));
    assert!(!debug.contains("sentinel"));
    assert!(!debug.contains("state.snap"));
    assert!(!debug.contains("memory.snap"));
}

#[test]
fn pre_staging_producer_error_retains_source_without_cleanup() {
    let error = SnapshotPublicationTransactionError::from_producer("admission closed");
    let producer = error
        .producer()
        .expect("pre-staging failure should retain its producer source");

    assert_eq!(producer.source(), &"admission closed");
    assert_eq!(producer.memory_cleanup(), None);
    assert_eq!(producer.state_cleanup(), None);
    assert!(error.publication().is_none());
}

#[cfg(not(target_os = "macos"))]
#[test]
fn generalized_publication_rejects_platform_without_invoking_producer() {
    let paths = SnapshotArtifactPaths::new("state.snap", "memory.snap");
    let called = std::cell::Cell::new(false);
    let error = publish_snapshot_artifacts_with::<std::io::Error, _>(&paths, |_writer| {
        called.set(true);
        Err(std::io::Error::other("producer must not run"))
    })
    .expect_err("non-macOS publication should reject at platform preflight");

    assert!(!called.get());
    assert_eq!(
        error
            .publication()
            .expect("platform rejection should be a publication failure")
            .stage(),
        SnapshotPublicationStage::PlatformCheck
    );
    assert!(error.producer().is_none());

    let native_called = std::cell::Cell::new(false);
    let native_error =
        publish_native_snapshot_artifacts_with::<std::io::Error, _>(&paths, |_writer| {
            native_called.set(true);
            Err(std::io::Error::other("native producer must not run"))
        })
        .expect_err("non-macOS native publication should reject at platform preflight");
    assert!(!native_called.get());
    assert_eq!(
        native_error
            .publication()
            .expect("native platform rejection should be a publication failure")
            .stage(),
        SnapshotPublicationStage::PlatformCheck
    );
}

#[cfg(target_os = "macos")]
#[test]
fn closed_native_state_derives_v2_binding_and_redacts_owned_bytes() {
    let memory = test_v2_memory();
    let mut image = Cursor::new(Vec::new());
    let binding =
        write_snapshot_v2_memory_image(&memory, &mut image).expect("v2 memory should encode");
    let bytes = current_v2_state(&binding).expect("v2 state should encode canonically");
    let state = NativeSnapshotArtifactState::from_current_v2(bytes.clone())
        .expect("current v2 state should validate");

    assert_eq!(state.family(), NativeSnapshotArtifactFamily::V2);
    assert_eq!(state.version(), NATIVE_V2_SNAPSHOT_VERSION);
    assert_eq!(state.v2_bytes(), Some(bytes.as_slice()));
    assert_eq!(state.v2_memory_binding(), Some(&binding));
    assert_eq!(
        state
            .v2_profile()
            .expect("current state should classify as exact virtio-mem profile"),
        NativeV2SnapshotArtifactProfile::MemoryHotplugStateV2_10
    );
    assert!(state.v1_record().is_none());
    let debug = format!("{state:?}");
    assert!(debug.contains(REDACTED));
    assert!(!debug.contains("BANGV2A"));
    assert!(!debug.contains("BANGM2A"));

    let (owned_bytes, owned_binding) = state
        .into_v2_parts()
        .expect("v2 state should consume into its closed parts");
    assert_eq!(owned_bytes, bytes);
    assert_eq!(owned_binding, binding);

    let binding_offset = bytes
        .windows(crate::snapshot_memory_v2::NATIVE_V2_MEMORY_MAGIC.len())
        .position(|window| window == crate::snapshot_memory_v2::NATIVE_V2_MEMORY_MAGIC)
        .expect("memory binding should occur in state");
    let binding_length = crate::snapshot_memory_v2::NATIVE_V2_MEMORY_HEADER_BYTES
        + crate::snapshot_memory_v2::NATIVE_V2_MEMORY_EXTENT_BYTES;

    let mut mismatched = bytes.clone();
    mismatched[binding_offset + 10..binding_offset + 12].copy_from_slice(&1_u16.to_le_bytes());
    mismatched[binding_offset + 48..binding_offset + 56].fill(0);
    let binding_checksum = crc64::crc64(
        0,
        &mismatched[binding_offset..binding_offset + binding_length],
    );
    mismatched[binding_offset + 48..binding_offset + 56]
        .copy_from_slice(&binding_checksum.to_le_bytes());
    let state_checksum_offset =
        mismatched.len() - crate::snapshot_format_v2::NATIVE_V2_SNAPSHOT_INTEGRITY_BYTES;
    let state_checksum = crc64::crc64(0, &mismatched[..state_checksum_offset]);
    mismatched[state_checksum_offset..].copy_from_slice(&state_checksum.to_le_bytes());
    assert!(matches!(
        NativeSnapshotArtifactState::from_current_v2(mismatched),
        Err(NativeSnapshotArtifactStateError::CurrentV2Profile(
            NativeV2SnapshotCandidateStateError::VersionMismatch { .. }
        ))
    ));

    let binding_payload = binding
        .encode()
        .expect("legacy binding source should encode");
    let memory_component = SnapshotV2Component::new(
        NATIVE_V2_MEMORY_COMPONENT_KEY,
        SnapshotV2ComponentDisposition::Semantic,
        &binding_payload,
    );
    let mut legacy = encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_SNAPSHOT_VERSION,
        &[],
        &[memory_component],
    )
    .expect("legacy structural source should encode");
    let legacy_binding_offset = legacy
        .windows(crate::snapshot_memory_v2::NATIVE_V2_MEMORY_MAGIC.len())
        .position(|window| window == crate::snapshot_memory_v2::NATIVE_V2_MEMORY_MAGIC)
        .expect("legacy memory binding should occur in state");
    legacy[10..12].copy_from_slice(&1_u16.to_le_bytes());
    legacy[legacy_binding_offset + 10..legacy_binding_offset + 12]
        .copy_from_slice(&1_u16.to_le_bytes());
    legacy[legacy_binding_offset + 48..legacy_binding_offset + 56].fill(0);
    let binding_checksum = crc64::crc64(
        0,
        &legacy[legacy_binding_offset..legacy_binding_offset + binding_length],
    );
    legacy[legacy_binding_offset + 48..legacy_binding_offset + 56]
        .copy_from_slice(&binding_checksum.to_le_bytes());
    let state_checksum_offset =
        legacy.len() - crate::snapshot_format_v2::NATIVE_V2_SNAPSHOT_INTEGRITY_BYTES;
    let state_checksum = crc64::crc64(0, &legacy[..state_checksum_offset]);
    legacy[state_checksum_offset..].copy_from_slice(&state_checksum.to_le_bytes());

    assert!(matches!(
        NativeSnapshotArtifactState::from_current_v2(legacy.clone()),
        Err(NativeSnapshotArtifactStateError::CurrentV2Profile(_))
    ));
    let compatible = NativeSnapshotArtifactState::from_compatible_bytes(legacy)
        .expect("compatible older-minor v2 state should prepare");
    assert_eq!(compatible.version(), SnapshotFormatVersion::new(2, 1, 0));

    let mut legacy_image = image.into_inner();
    let legacy_header = compatible
        .v2_memory_binding()
        .expect("compatible v2 state should retain a binding")
        .encode()
        .expect("compatible binding should re-encode");
    legacy_image[..crate::snapshot_memory_v2::NATIVE_V2_MEMORY_HEADER_BYTES].copy_from_slice(
        &legacy_header[..crate::snapshot_memory_v2::NATIVE_V2_MEMORY_HEADER_BYTES],
    );
    let directory = TestDirectory::new("legacy-v2-publish");
    let paths = directory.paths("state.snap", "memory.snap");
    let error = publish_native_snapshot_artifacts_with(&paths, |mut writer| {
        writer
            .write_all(&legacy_image)
            .expect("compatible image fixture should write");
        Ok::<_, io::Error>(compatible)
    })
    .expect_err("a compatible reader value must not bypass current-version publication");
    assert!(matches!(
        error
            .publication()
            .expect("version rejection should be a publication failure")
            .failure(),
        SnapshotPublicationFailure::NativeState(
            NativeSnapshotArtifactStateError::NonCurrentV2Publication { .. }
        )
    ));
    assert!(!paths.state().exists());
    assert!(!paths.memory().exists());

    let v1 = NativeSnapshotArtifactState::from_v1(test_memory_only_record());
    assert_eq!(v1.family(), NativeSnapshotArtifactFamily::V1);
    assert_eq!(v1.version(), NATIVE_V1_SNAPSHOT_VERSION);
    assert!(v1.v2_bytes().is_none());
    assert!(
        v1.into_v1_record().is_ok(),
        "v1 state should consume only through the v1 accessor"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn current_v2_artifact_boundary_rejects_missing_serial_state() {
    let memory = test_v2_memory();
    let mut image = Cursor::new(Vec::new());
    let binding = write_snapshot_v2_memory_image_with_compatibility_version(
        &memory,
        &mut image,
        NATIVE_V2_SNAPSHOT_VERSION,
    )
    .expect("graphless current memory should encode internally");
    let binding_payload = binding
        .encode()
        .expect("graphless current binding should encode");
    let memory_component = SnapshotV2Component::new(
        NATIVE_V2_MEMORY_COMPONENT_KEY,
        SnapshotV2ComponentDisposition::Semantic,
        &binding_payload,
    );
    let bytes = encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_SNAPSHOT_VERSION,
        &[],
        &[memory_component],
    )
    .expect("graphless current state should encode internally");

    assert!(matches!(
        NativeSnapshotArtifactState::from_current_v2(bytes),
        Err(NativeSnapshotArtifactStateError::CurrentV2Profile(
            NativeV2SnapshotCandidateStateError::MissingSerialState
        ))
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn current_v2_artifact_boundary_rejects_duplicate_and_nonsemantic_serial_state() {
    let memory = test_v2_memory();
    let mut image = Cursor::new(Vec::new());
    let binding = write_snapshot_v2_memory_image_with_compatibility_version(
        &memory,
        &mut image,
        NATIVE_V2_SNAPSHOT_VERSION,
    )
    .expect("current memory should encode internally");
    let binding_payload = binding.encode().expect("current binding should encode");
    let serial_device = SerialMmioDevice::discarding()
        .capture_state()
        .expect("serial fixture should capture");
    let serial_payload = SnapshotV2SerialState::try_from_capture_ready(
        CaptureReadySerialState::new(SerialConfig::default(), serial_device),
    )
    .expect("serial fixture should normalize")
    .encode(NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION)
    .expect("serial fixture should encode");
    let memory_component = SnapshotV2Component::new(
        NATIVE_V2_MEMORY_COMPONENT_KEY,
        SnapshotV2ComponentDisposition::Semantic,
        &binding_payload,
    );
    let serial = SnapshotV2Component::new(
        NATIVE_V2_SERIAL_COMPONENT_KEY,
        SnapshotV2ComponentDisposition::Semantic,
        &serial_payload,
    );
    let duplicate_serial = SnapshotV2Component::new(
        SnapshotV2ComponentKey::new(NATIVE_V2_SERIAL_COMPONENT_KEY.kind(), 1),
        SnapshotV2ComponentDisposition::Semantic,
        &serial_payload,
    );
    let duplicate = encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_SNAPSHOT_VERSION,
        &[],
        &[memory_component, serial, duplicate_serial],
    )
    .expect("structural encoder should retain duplicate serial-kind fixture");
    assert!(matches!(
        NativeSnapshotArtifactState::from_current_v2(duplicate),
        Err(NativeSnapshotArtifactStateError::CurrentV2Profile(
            NativeV2SnapshotCandidateStateError::InvalidSerialComponent
        ))
    ));

    let nonsemantic_serial = SnapshotV2Component::new(
        NATIVE_V2_SERIAL_COMPONENT_KEY,
        SnapshotV2ComponentDisposition::NonSemantic,
        &serial_payload,
    );
    let nonsemantic = encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_SNAPSHOT_VERSION,
        &[],
        &[memory_component, nonsemantic_serial],
    )
    .expect("structural encoder should retain nonsemantic serial fixture");
    assert!(matches!(
        NativeSnapshotArtifactState::from_current_v2(nonsemantic),
        Err(NativeSnapshotArtifactStateError::CurrentV2Profile(
            NativeV2SnapshotCandidateStateError::InvalidSerialComponent
        ))
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn exact_minor_eight_candidate_classifies_all_optional_storage_entropy_combinations() {
    let memory = test_v2_memory();
    let mut image = Cursor::new(Vec::new());
    let binding = write_snapshot_v2_memory_image_with_compatibility_version(
        &memory,
        &mut image,
        NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
    )
    .expect("exact 2.8 memory should encode internally");
    let storage_payload = fixture_bytes(include_str!(
        "../snapshot_device_v2_6/fixtures/block-root-mmio.hex"
    ));
    let entropy_payload = fixture_bytes(include_str!(
        "../snapshot_entropy_v2_8/fixtures/inactive-mmio.hex"
    ));
    let binding_payload = binding.encode().expect("exact 2.8 binding should encode");
    let missing_serial_components = [
        SnapshotV2Component::new(
            NATIVE_V2_MEMORY_COMPONENT_KEY,
            SnapshotV2ComponentDisposition::Semantic,
            &binding_payload,
        ),
        SnapshotV2Component::new(
            NATIVE_V2_ENTROPY_COMPONENT_KEY,
            SnapshotV2ComponentDisposition::Semantic,
            &entropy_payload,
        ),
    ];
    let missing_serial = encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
        &[],
        &missing_serial_components,
    )
    .expect("missing serial should remain structurally encodable");
    assert!(matches!(
        NativeV2EntropySnapshotCandidateState::from_entropy_state_v2_8(missing_serial),
        Err(NativeV2SnapshotCandidateStateError::MissingSerialState)
    ));

    let invalid_serial_components = [
        SnapshotV2Component::new(
            NATIVE_V2_MEMORY_COMPONENT_KEY,
            SnapshotV2ComponentDisposition::Semantic,
            &binding_payload,
        ),
        SnapshotV2Component::new(
            NATIVE_V2_SERIAL_COMPONENT_KEY,
            SnapshotV2ComponentDisposition::Semantic,
            b"invalid-serial",
        ),
    ];
    let invalid_serial = encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
        &[],
        &invalid_serial_components,
    )
    .expect("invalid nested serial should remain structurally encodable");
    assert!(matches!(
        NativeV2EntropySnapshotCandidateState::from_entropy_state_v2_8(invalid_serial),
        Err(NativeV2SnapshotCandidateStateError::SerialState(_))
    ));

    for (with_storage, with_entropy) in [(false, false), (true, false), (false, true), (true, true)]
    {
        let entropy_components = if with_entropy {
            vec![(
                NATIVE_V2_ENTROPY_COMPONENT_KEY,
                SnapshotV2ComponentDisposition::Semantic,
                entropy_payload.as_slice(),
            )]
        } else {
            Vec::new()
        };
        let bytes = entropy_v2_8_state(
            &binding,
            with_storage.then_some(storage_payload.as_slice()),
            &entropy_components,
        )
        .expect("exact 2.8 fixture should encode");
        assert!(matches!(
            NativeSnapshotArtifactState::from_current_v2(bytes.clone()),
            Err(NativeSnapshotArtifactStateError::CurrentV2Profile(_))
        ));
        let compatible = NativeSnapshotArtifactState::from_compatible_bytes(bytes.clone())
            .expect("public compatible file loading should admit exact 2.8");
        assert_eq!(
            compatible
                .v2_profile()
                .expect("compatible exact 2.8 state should classify"),
            NativeV2SnapshotArtifactProfile::EntropyStateV2_8
        );

        let candidate =
            NativeV2EntropySnapshotCandidateState::from_entropy_state_v2_8(bytes.clone())
                .expect("exact 2.8 candidate should validate");
        assert_eq!(
            candidate.version(),
            NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION
        );
        assert_eq!(candidate.memory_binding(), &binding);
        assert_eq!(candidate.device_graph().is_some(), with_storage);
        assert_eq!(candidate.entropy().is_some(), with_entropy);
        assert_eq!(candidate.bytes(), bytes);

        let compatible = candidate.into_compatible_artifact_state();
        assert_eq!(
            compatible
                .v2_profile()
                .expect("compatible exact 2.8 state should classify"),
            NativeV2SnapshotArtifactProfile::EntropyStateV2_8
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn exact_minor_eight_candidate_rejects_component_and_nested_version_mismatches() {
    let memory = test_v2_memory();
    let mut image = Cursor::new(Vec::new());
    let binding = write_snapshot_v2_memory_image_with_compatibility_version(
        &memory,
        &mut image,
        NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
    )
    .expect("exact 2.8 memory should encode internally");
    let entropy_payload = fixture_bytes(include_str!(
        "../snapshot_entropy_v2_8/fixtures/inactive-mmio.hex"
    ));

    for components in [
        vec![(
            SnapshotV2ComponentKey::new(NATIVE_V2_ENTROPY_COMPONENT_KEY.kind(), 1),
            SnapshotV2ComponentDisposition::Semantic,
            entropy_payload.as_slice(),
        )],
        vec![(
            NATIVE_V2_ENTROPY_COMPONENT_KEY,
            SnapshotV2ComponentDisposition::NonSemantic,
            entropy_payload.as_slice(),
        )],
        vec![
            (
                NATIVE_V2_ENTROPY_COMPONENT_KEY,
                SnapshotV2ComponentDisposition::Semantic,
                entropy_payload.as_slice(),
            ),
            (
                SnapshotV2ComponentKey::new(NATIVE_V2_ENTROPY_COMPONENT_KEY.kind(), 1),
                SnapshotV2ComponentDisposition::Semantic,
                entropy_payload.as_slice(),
            ),
        ],
    ] {
        let bytes = entropy_v2_8_state(&binding, None, &components)
            .expect("structural exact 2.8 fixture should encode");
        assert!(matches!(
            NativeV2EntropySnapshotCandidateState::from_entropy_state_v2_8(bytes),
            Err(NativeV2SnapshotCandidateStateError::InvalidEntropyComponent)
        ));
    }

    let invalid_payload = [0_u8; 160];
    let invalid = entropy_v2_8_state(
        &binding,
        None,
        &[(
            NATIVE_V2_ENTROPY_COMPONENT_KEY,
            SnapshotV2ComponentDisposition::Semantic,
            invalid_payload.as_slice(),
        )],
    )
    .expect("invalid nested entropy should remain structurally encodable");
    assert!(matches!(
        NativeV2EntropySnapshotCandidateState::from_entropy_state_v2_8(invalid),
        Err(NativeV2SnapshotCandidateStateError::EntropyState(_))
    ));

    let wrong_storage = fixture_bytes(include_str!(
        "../snapshot_device_v2_5/fixtures/root-mmio.hex"
    ));
    let wrong_storage = entropy_v2_8_state(&binding, Some(&wrong_storage), &[])
        .expect("cross-profile storage should remain structurally encodable");
    assert!(matches!(
        NativeV2EntropySnapshotCandidateState::from_entropy_state_v2_8(wrong_storage),
        Err(NativeV2SnapshotCandidateStateError::StorageDeviceGraph(_))
    ));

    let mut mismatched_image = Cursor::new(Vec::new());
    let mismatched_binding = write_snapshot_v2_memory_image_with_compatibility_version(
        &memory,
        &mut mismatched_image,
        NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
    )
    .expect("exact 2.7 memory fixture should encode");
    let mismatched = entropy_v2_8_state(&mismatched_binding, None, &[])
        .expect("mismatch should encode structurally");
    assert!(matches!(
        NativeV2EntropySnapshotCandidateState::from_entropy_state_v2_8(mismatched),
        Err(NativeV2SnapshotCandidateStateError::VersionMismatch { .. })
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn exact_minor_nine_candidate_classifies_all_optional_component_combinations_and_stays_internal() {
    let memory = test_v2_memory();
    let mut image = Cursor::new(Vec::new());
    let binding = write_snapshot_v2_memory_image_with_compatibility_version(
        &memory,
        &mut image,
        NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
    )
    .expect("exact 2.9 memory should encode internally");
    let storage_payload = fixture_bytes(include_str!(
        "../snapshot_device_v2_6/fixtures/block-root-mmio.hex"
    ));
    let entropy_payload = fixture_bytes(include_str!(
        "../snapshot_entropy_v2_8/fixtures/inactive-mmio.hex"
    ));
    let balloon_payload = fixture_bytes(include_str!(
        "../snapshot_balloon_v2_9/fixtures/active-pci.hex"
    ));

    for with_storage in [false, true] {
        for with_entropy in [false, true] {
            for with_balloon in [false, true] {
                let entropy_components = if with_entropy {
                    vec![(
                        NATIVE_V2_ENTROPY_COMPONENT_KEY,
                        SnapshotV2ComponentDisposition::Semantic,
                        entropy_payload.as_slice(),
                    )]
                } else {
                    Vec::new()
                };
                let balloon_components = if with_balloon {
                    vec![(
                        NATIVE_V2_BALLOON_COMPONENT_KEY,
                        SnapshotV2ComponentDisposition::Semantic,
                        balloon_payload.as_slice(),
                    )]
                } else {
                    Vec::new()
                };
                let bytes = balloon_v2_9_state(
                    &binding,
                    with_storage.then_some(storage_payload.as_slice()),
                    &entropy_components,
                    &balloon_components,
                )
                .expect("exact 2.9 fixture should encode");

                assert!(matches!(
                    NativeSnapshotArtifactState::from_current_v2(bytes.clone()),
                    Err(NativeSnapshotArtifactStateError::CurrentV2Profile(_))
                ));
                let compatible = NativeSnapshotArtifactState::from_compatible_bytes(bytes.clone())
                    .expect("public compatible file loading should admit exact 2.9");
                assert_eq!(
                    compatible
                        .v2_profile()
                        .expect("compatible exact 2.9 state should classify"),
                    NativeV2SnapshotArtifactProfile::BalloonStateV2_9
                );

                let candidate =
                    NativeV2BalloonSnapshotCandidateState::from_balloon_state_v2_9(bytes.clone())
                        .expect("exact 2.9 candidate should validate");
                assert_eq!(
                    candidate.version(),
                    NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION
                );
                assert_eq!(candidate.memory_binding(), &binding);
                assert_eq!(candidate.device_graph().is_some(), with_storage);
                assert_eq!(candidate.entropy().is_some(), with_entropy);
                assert_eq!(candidate.balloon().is_some(), with_balloon);
                assert_eq!(candidate.bytes(), bytes);
                let debug = format!("{candidate:?}");
                assert!(debug.contains(REDACTED));
                assert!(!debug.contains("BANGBL2"));

                let compatible = candidate.into_compatible_artifact_state();
                assert_eq!(
                    compatible
                        .v2_profile()
                        .expect("compatible exact 2.9 state should classify"),
                    NativeV2SnapshotArtifactProfile::BalloonStateV2_9
                );
                assert!(matches!(
                    compatible.validate_for_publication(),
                    Err(NativeSnapshotArtifactStateError::NonCurrentV2Publication {
                        state: NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
                        memory: NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
                    })
                ));
            }
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn exact_minor_nine_candidate_rejects_balloon_cardinality_payload_and_version_mismatches() {
    let memory = test_v2_memory();
    let mut image = Cursor::new(Vec::new());
    let binding = write_snapshot_v2_memory_image_with_compatibility_version(
        &memory,
        &mut image,
        NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
    )
    .expect("exact 2.9 memory should encode internally");
    let binding_payload = binding.encode().expect("exact 2.9 binding should encode");
    let balloon_payload = fixture_bytes(include_str!(
        "../snapshot_balloon_v2_9/fixtures/inactive-mmio.hex"
    ));

    let missing_serial = encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
        &[],
        &[
            SnapshotV2Component::new(
                NATIVE_V2_MEMORY_COMPONENT_KEY,
                SnapshotV2ComponentDisposition::Semantic,
                &binding_payload,
            ),
            SnapshotV2Component::new(
                NATIVE_V2_BALLOON_COMPONENT_KEY,
                SnapshotV2ComponentDisposition::Semantic,
                &balloon_payload,
            ),
        ],
    )
    .expect("missing serial should remain structurally encodable");
    assert!(matches!(
        NativeV2BalloonSnapshotCandidateState::from_balloon_state_v2_9(missing_serial),
        Err(NativeV2SnapshotCandidateStateError::MissingSerialState)
    ));

    for components in [
        vec![(
            SnapshotV2ComponentKey::new(NATIVE_V2_BALLOON_COMPONENT_KEY.kind(), 1),
            SnapshotV2ComponentDisposition::Semantic,
            balloon_payload.as_slice(),
        )],
        vec![(
            NATIVE_V2_BALLOON_COMPONENT_KEY,
            SnapshotV2ComponentDisposition::NonSemantic,
            balloon_payload.as_slice(),
        )],
        vec![
            (
                NATIVE_V2_BALLOON_COMPONENT_KEY,
                SnapshotV2ComponentDisposition::Semantic,
                balloon_payload.as_slice(),
            ),
            (
                SnapshotV2ComponentKey::new(NATIVE_V2_BALLOON_COMPONENT_KEY.kind(), 1),
                SnapshotV2ComponentDisposition::Semantic,
                balloon_payload.as_slice(),
            ),
        ],
    ] {
        let bytes = balloon_v2_9_state(&binding, None, &[], &components)
            .expect("structural exact 2.9 fixture should encode");
        assert!(matches!(
            NativeV2BalloonSnapshotCandidateState::from_balloon_state_v2_9(bytes),
            Err(NativeV2SnapshotCandidateStateError::InvalidBalloonComponent)
        ));
    }

    let invalid_payload = [0_u8; 192];
    let invalid = balloon_v2_9_state(
        &binding,
        None,
        &[],
        &[(
            NATIVE_V2_BALLOON_COMPONENT_KEY,
            SnapshotV2ComponentDisposition::Semantic,
            invalid_payload.as_slice(),
        )],
    )
    .expect("invalid nested balloon should remain structurally encodable");
    assert!(matches!(
        NativeV2BalloonSnapshotCandidateState::from_balloon_state_v2_9(invalid),
        Err(NativeV2SnapshotCandidateStateError::BalloonState(_))
    ));

    let mut mismatched_image = Cursor::new(Vec::new());
    let mismatched_binding = write_snapshot_v2_memory_image_with_compatibility_version(
        &memory,
        &mut mismatched_image,
        NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
    )
    .expect("exact 2.8 memory fixture should encode");
    let mismatched = balloon_v2_9_state(&mismatched_binding, None, &[], &[])
        .expect("mismatch should encode structurally");
    assert!(matches!(
        NativeV2BalloonSnapshotCandidateState::from_balloon_state_v2_9(mismatched),
        Err(NativeV2SnapshotCandidateStateError::VersionMismatch { .. })
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn exact_minor_ten_products_prepare_or_preserve_every_optional_component_product() {
    let directory = TestDirectory::new("mixed-products");
    let storage_payload = fixture_bytes(include_str!(
        "../snapshot_device_v2_6/fixtures/block-root-mmio.hex"
    ));
    let entropy_payload = fixture_bytes(include_str!(
        "../snapshot_entropy_v2_8/fixtures/inactive-mmio.hex"
    ));
    let balloon_payload = fixture_bytes(include_str!(
        "../snapshot_balloon_v2_9/fixtures/active-pci.hex"
    ));
    let base_memory = test_v2_memory();
    let mut base_image = Cursor::new(Vec::new());
    let base_binding = write_snapshot_v2_memory_image_with_compatibility_version(
        &base_memory,
        &mut base_image,
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
    )
    .expect("base exact 2.10 memory should encode internally");

    let inactive_mmio_payload = fixture_bytes(include_str!(
        "../snapshot_memory_hotplug_v2_10/fixtures/inactive-mmio.hex"
    ));
    let inactive_mmio_state = SnapshotV2MemoryHotplugState::decode(
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
        &inactive_mmio_payload,
    )
    .expect("inactive MMIO exact 2.10 fixture should decode");
    let active_pci_payload = fixture_bytes(include_str!(
        "../snapshot_memory_hotplug_v2_10/fixtures/active-pci.hex"
    ));
    let active_pci_state = SnapshotV2MemoryHotplugState::decode(
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
        &active_pci_payload,
    )
    .expect("active PCI exact 2.10 fixture should decode");
    let (active_config, active_config_space, active_queue, active_bitmap, active_virtio, _) =
        active_pci_state.clone().into_parts();
    let (_, _, _, _, _, inactive_mmio_transport) = inactive_mmio_state.clone().into_parts();
    let active_mmio_state = SnapshotV2MemoryHotplugState::try_new(
        active_config,
        active_config_space,
        active_queue,
        active_bitmap,
        active_virtio,
        inactive_mmio_transport,
    )
    .expect("active MMIO exact 2.10 state should validate");
    let active_mmio_payload = active_mmio_state
        .encode(NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION)
        .expect("active MMIO exact 2.10 state should encode");

    let mut profiles = Vec::new();
    for (payload, state) in [
        (inactive_mmio_payload, inactive_mmio_state),
        (active_mmio_payload, active_mmio_state),
        (active_pci_payload, active_pci_state),
    ] {
        let memory = test_v2_memory_with_hotplug(&state);
        let mut image = Cursor::new(Vec::new());
        let binding = write_snapshot_v2_memory_image_with_compatibility_version(
            &memory,
            &mut image,
            NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
        )
        .expect("dynamic exact 2.10 memory should encode internally");
        profiles.push((payload, state, binding, image.into_inner()));
    }

    let mut prepared_count = 0;
    let mut compatible_count = 0;
    let mut materialized_count = 0;
    for (
        profile_index,
        (memory_hotplug_payload, memory_hotplug_state, hotplug_binding, memory_image),
    ) in profiles.iter().enumerate()
    {
        let image_path = directory
            .path
            .join(format!("memory-profile-{profile_index}.snap"));
        fs::write(&image_path, memory_image).expect("profile memory image should write");
        for with_storage in [false, true] {
            for with_entropy in [false, true] {
                for with_balloon in [false, true] {
                    for with_memory_hotplug in [false, true] {
                        let entropy_components = if with_entropy {
                            vec![(
                                NATIVE_V2_ENTROPY_COMPONENT_KEY,
                                SnapshotV2ComponentDisposition::Semantic,
                                entropy_payload.as_slice(),
                            )]
                        } else {
                            Vec::new()
                        };
                        let balloon_components = if with_balloon {
                            vec![(
                                NATIVE_V2_BALLOON_COMPONENT_KEY,
                                SnapshotV2ComponentDisposition::Semantic,
                                balloon_payload.as_slice(),
                            )]
                        } else {
                            Vec::new()
                        };
                        let memory_hotplug_components = if with_memory_hotplug {
                            vec![(
                                NATIVE_V2_MEMORY_HOTPLUG_COMPONENT_KEY,
                                SnapshotV2ComponentDisposition::Semantic,
                                memory_hotplug_payload.as_slice(),
                            )]
                        } else {
                            Vec::new()
                        };
                        let binding = if with_memory_hotplug {
                            hotplug_binding
                        } else {
                            &base_binding
                        };
                        let bytes = memory_hotplug_v2_10_state(
                            binding,
                            with_storage.then_some(storage_payload.as_slice()),
                            &entropy_components,
                            &balloon_components,
                            &memory_hotplug_components,
                        )
                        .expect("exact 2.10 product should encode");

                        let current = NativeSnapshotArtifactState::from_current_v2(bytes.clone())
                            .expect("exact 2.10 should have current publication authority");
                        assert_eq!(
                            current
                                .v2_profile()
                                .expect("current exact 2.10 state should classify"),
                            NativeV2SnapshotArtifactProfile::MemoryHotplugStateV2_10
                        );
                        let compatible =
                            NativeSnapshotArtifactState::from_compatible_bytes(bytes.clone())
                                .expect("public compatible loading should admit exact 2.10");
                        assert_eq!(
                            compatible
                                .v2_profile()
                                .expect("compatible exact 2.10 state should classify"),
                            NativeV2SnapshotArtifactProfile::MemoryHotplugStateV2_10
                        );

                        let candidate =
                            NativeV2MemoryHotplugSnapshotCandidateState::from_memory_hotplug_state_v2_10(
                                bytes.clone(),
                            )
                            .expect("exact 2.10 candidate should validate");
                        assert_eq!(
                            candidate.version(),
                            NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION
                        );
                        assert_eq!(candidate.memory_binding(), binding);
                        assert_eq!(candidate.device_graph().is_some(), with_storage);
                        assert_eq!(candidate.entropy().is_some(), with_entropy);
                        assert_eq!(candidate.balloon().is_some(), with_balloon);
                        assert_eq!(candidate.memory_hotplug().is_some(), with_memory_hotplug);
                        assert_eq!(candidate.bytes(), bytes);
                        let debug = format!("{candidate:?}");
                        assert!(debug.contains(REDACTED));
                        assert!(!debug.contains("BANGME2"));
                        let expected_device_graph = candidate.device_graph().cloned();
                        let expected_serial = candidate.serial().clone();
                        let expected_entropy = candidate.entropy().cloned();
                        let expected_balloon = candidate.balloon().cloned();

                        let preparation = candidate
                            .prepare()
                            .expect("exact 2.10 candidate should prepare");
                        let preparation_debug = format!("{preparation:?}");
                        assert!(preparation_debug.contains(REDACTED));
                        assert!(!preparation_debug.contains("BANGME2"));
                        if with_memory_hotplug {
                            let prepared = preparation
                                .prepared()
                                .expect("kind-11 product should be prepared");
                            assert!(preparation.compatible().is_none());
                            assert_eq!(prepared.bytes(), bytes);
                            assert_eq!(prepared.memory_binding(), binding);
                            assert_eq!(prepared.device_graph(), expected_device_graph.as_ref());
                            assert_eq!(prepared.serial(), &expected_serial);
                            assert_eq!(prepared.entropy(), expected_entropy.as_ref());
                            assert_eq!(prepared.balloon(), expected_balloon.as_ref());
                            assert_eq!(prepared.topology().state(), memory_hotplug_state);
                            assert_eq!(
                                prepared.topology().queue_ranges().is_some(),
                                memory_hotplug_state.virtio().is_activated()
                            );
                            let prepared_debug = format!("{prepared:?}");
                            assert!(prepared_debug.contains(REDACTED));
                            assert!(!prepared_debug.contains("BANGME2"));

                            let materialized = prepared_memory_hotplug_candidate(bytes.clone())
                                .materialize_memory_file(
                                    File::open(&image_path)
                                        .expect("profile image should open read-only"),
                                )
                                .expect("every exact optional product should materialize");
                            assert_eq!(materialized.bytes(), bytes);
                            assert_eq!(materialized.memory_binding(), binding);
                            assert_eq!(materialized.device_graph(), expected_device_graph.as_ref());
                            assert_eq!(materialized.serial(), &expected_serial);
                            assert_eq!(materialized.entropy(), expected_entropy.as_ref());
                            assert_eq!(materialized.balloon(), expected_balloon.as_ref());
                            assert_eq!(materialized.topology().state(), memory_hotplug_state);
                            assert!(
                                materialized
                                    .memory()
                                    .dirty_tracker()
                                    .expect("materialized product should track dirty pages")
                                    .dirty_pages()
                                    .expect("materialized product dirty pages should query")
                                    .is_empty()
                            );
                            let materialized_debug = format!("{materialized:?}");
                            assert!(materialized_debug.contains(REDACTED));
                            assert!(!materialized_debug.contains("BANGME2"));
                            materialized_count += 1;
                            prepared_count += 1;
                        } else {
                            let compatible_candidate = preparation
                                .compatible()
                                .expect("no-kind-11 product should remain compatible");
                            assert!(preparation.prepared().is_none());
                            assert_eq!(compatible_candidate.bytes(), bytes);
                            assert_eq!(compatible_candidate.memory_binding(), binding);
                            assert!(compatible_candidate.memory_hotplug().is_none());
                            compatible_count += 1;
                        }

                        let compatible =
                            NativeV2MemoryHotplugSnapshotCandidateState::from_memory_hotplug_state_v2_10(
                                bytes,
                            )
                            .expect("exact 2.10 current candidate should validate")
                            .into_current_artifact_state();
                        assert_eq!(
                            compatible
                                .v2_profile()
                                .expect("current exact 2.10 state should classify"),
                            NativeV2SnapshotArtifactProfile::MemoryHotplugStateV2_10
                        );
                        compatible
                            .validate_for_publication()
                            .expect("exact 2.10 candidate should retain publication authority");
                    }
                }
            }
        }
    }
    assert_eq!(prepared_count, 24);
    assert_eq!(compatible_count, 24);
    assert_eq!(materialized_count, 24);
}

#[cfg(target_os = "macos")]
#[test]
fn exact_minor_ten_materialization_preserves_bytes_ownership_and_clean_isolation() {
    let directory = TestDirectory::new("mixed-memory");
    let memory_hotplug_payload = fixture_bytes(include_str!(
        "../snapshot_memory_hotplug_v2_10/fixtures/active-pci.hex"
    ));
    let state = SnapshotV2MemoryHotplugState::decode(
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
        &memory_hotplug_payload,
    )
    .expect("active exact 2.10 virtio-mem fixture should decode");
    let mut source_memory = test_v2_memory_with_hotplug(&state);
    let source_ranges = source_memory
        .regions()
        .iter()
        .map(|region| region.range())
        .collect::<Vec<_>>();
    for (region_index, range) in source_ranges.iter().copied().enumerate() {
        let length = usize::try_from(range.size()).expect("fixture range should fit usize");
        let bytes = (0..length)
            .map(|byte_index| {
                u8::try_from((region_index * 73 + byte_index) % 251)
                    .expect("fixture byte should fit")
            })
            .collect::<Vec<_>>();
        source_memory
            .write_slice(&bytes, range.start())
            .expect("fixture bytes should write");
    }
    let source_tracker = source_memory
        .enable_dirty_tracking()
        .expect("source fixture dirty tracking should enable");
    assert_eq!(source_tracker.clear_quiesced(), 1);
    assert_eq!(source_tracker.clear_quiesced(), 2);
    source_memory
        .write_slice(&[0x3c], source_ranges[0].start())
        .expect("source fixture should record a later dirty epoch");
    assert_eq!(source_tracker.epoch(), 2);
    assert!(
        !source_tracker
            .dirty_pages()
            .expect("source fixture dirty pages should query")
            .is_empty()
    );

    let mut image = Cursor::new(Vec::new());
    let binding = write_snapshot_v2_memory_image_with_compatibility_version(
        &source_memory,
        &mut image,
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
    )
    .expect("mixed exact 2.10 memory should encode");
    let image = image.into_inner();
    let state_bytes = memory_hotplug_v2_10_state(
        &binding,
        None,
        &[],
        &[],
        &[(
            NATIVE_V2_MEMORY_HOTPLUG_COMPONENT_KEY,
            SnapshotV2ComponentDisposition::Semantic,
            memory_hotplug_payload.as_slice(),
        )],
    )
    .expect("mixed exact 2.10 state should encode");

    let source_path = directory.path.join("memory.snap");
    let retained_path = directory.path.join("retained.snap");
    fs::write(&source_path, &image).expect("source image should write");
    let adopted = File::open(&source_path).expect("source image should open read-only");
    fs::rename(&source_path, &retained_path).expect("opened source path should move");
    fs::write(&source_path, vec![0xa5; image.len()])
        .expect("replacement path should contain unrelated bytes");

    let first = prepared_memory_hotplug_candidate(state_bytes.clone())
        .materialize_memory_file(adopted)
        .expect("adopted descriptor should materialize despite path replacement");
    let second = prepared_memory_hotplug_candidate(state_bytes.clone())
        .materialize_memory_file(
            File::open(&retained_path).expect("retained source should reopen read-only"),
        )
        .expect("same image should materialize independently");

    assert_eq!(first.bytes(), state_bytes);
    assert_eq!(first.memory_binding(), &binding);
    assert_eq!(first.topology().state(), &state);
    let aperture = GuestMemoryRange::new(
        GuestAddress::new(state.config_space().addr()),
        state.config_space().region_size(),
    )
    .expect("fixture aperture should validate");
    let first_reservation = first
        .memory()
        .shared_reservation_capture_state(aperture)
        .expect("first shared reservation should exist");
    let second_reservation = second
        .memory()
        .shared_reservation_capture_state(aperture)
        .expect("second shared reservation should exist");
    assert_ne!(
        first_reservation.mapping_identity(),
        second_reservation.mapping_identity()
    );

    for classified in first.topology().memory().classified_extents() {
        if classified.class()
            == crate::snapshot_memory_hotplug_v2_10::SnapshotV2MemoryHotplugExtentClass::Base
        {
            let region = first
                .memory()
                .regions()
                .iter()
                .find(|region| region.range() == classified.extent().range())
                .expect("every base extent should remain an active region");
            assert_eq!(region.backing(), GuestMemoryRegionBacking::PrivateFile);
        }
    }
    let block_size = state.config_space().block_size();
    for range in first.topology().plugged_ranges() {
        let mut start = range.start().raw_value();
        while start < range.end_exclusive().raw_value() {
            let block = GuestMemoryRange::new(GuestAddress::new(start), block_size)
                .expect("plugged block should validate");
            let region = first
                .memory()
                .regions()
                .iter()
                .find(|region| region.range() == block)
                .expect("every canonical plugged block should be active");
            assert_eq!(region.backing(), GuestMemoryRegionBacking::Shared);
            assert_eq!(
                region.mapping_identity(),
                first_reservation.mapping_identity()
            );
            start = block.end_exclusive().raw_value();
        }
    }

    for range in source_ranges {
        let length = usize::try_from(range.size()).expect("fixture range should fit usize");
        let mut expected = vec![0; length];
        let mut first_actual = vec![0; length];
        let mut second_actual = vec![0; length];
        source_memory
            .read_slice(&mut expected, range.start())
            .expect("source bytes should read");
        first
            .memory()
            .read_slice(&mut first_actual, range.start())
            .expect("first materialized bytes should read");
        second
            .memory()
            .read_slice(&mut second_actual, range.start())
            .expect("second materialized bytes should read");
        assert_eq!(first_actual, expected);
        assert_eq!(second_actual, expected);
    }

    let offline = (0..state.config_space().region_size() / state.config_space().block_size())
        .map(|block| {
            GuestAddress::new(
                state.config_space().addr() + block * state.config_space().block_size(),
            )
        })
        .find(|address| {
            !first
                .topology()
                .plugged_ranges()
                .iter()
                .any(|range| range.contains(*address))
        })
        .expect("active fixture should retain at least one offline block");
    assert!(
        first.memory().read_slice(&mut [0], offline).is_err(),
        "offline aperture bytes must remain inaccessible"
    );
    for materialized in [&first, &second] {
        let tracker = materialized
            .memory()
            .dirty_tracker()
            .expect("materialized memory should have a dirty tracker");
        assert_eq!(tracker.epoch(), 0);
        assert!(
            tracker
                .dirty_pages()
                .expect("clean dirty pages should query")
                .is_empty()
        );
    }

    let debug = format!("{first:?}");
    assert!(debug.contains(REDACTED));
    assert!(!debug.contains("BANGME2"));
    assert!(!debug.contains(&state.config_space().addr().to_string()));

    let (_, _, _, _, _, first_topology, mut first_memory) = first.into_parts();
    let base_range = first_topology
        .memory()
        .classified_extents()
        .find(|classified| {
            classified.class()
                == crate::snapshot_memory_hotplug_v2_10::SnapshotV2MemoryHotplugExtentClass::Base
        })
        .expect("mixed fixture should contain base memory")
        .extent()
        .range();
    let mut second_base_before = [0_u8; 1];
    second
        .memory()
        .read_slice(&mut second_base_before, base_range.start())
        .expect("second base byte should read");
    first_memory
        .write_slice(&[second_base_before[0] ^ 0xff], base_range.start())
        .expect("first private base byte should accept a COW write");
    let mut second_base_after = [0_u8; 1];
    second
        .memory()
        .read_slice(&mut second_base_after, base_range.start())
        .expect("second base byte should remain readable");
    assert_eq!(second_base_after, second_base_before);

    let dynamic_range = *first_topology
        .plugged_ranges()
        .first()
        .expect("active fixture should contain plugged memory");
    let mut second_before = [0_u8; 1];
    second
        .memory()
        .read_slice(&mut second_before, dynamic_range.start())
        .expect("second dynamic byte should read");
    first_memory
        .write_slice(&[second_before[0] ^ 0xff], dynamic_range.start())
        .expect("first dynamic byte should mutate independently");
    let mut second_after = [0_u8; 1];
    second
        .memory()
        .read_slice(&mut second_after, dynamic_range.start())
        .expect("second dynamic byte should remain readable");
    assert_eq!(second_after, second_before);

    let tracker = first_memory
        .dirty_tracker()
        .expect("first dirty tracker should remain attached");
    assert!(
        !tracker
            .dirty_pages()
            .expect("dirty pages should query")
            .is_empty()
    );
    assert_eq!(tracker.clear_quiesced(), 1);
    assert!(
        tracker
            .dirty_pages()
            .expect("cleared dirty pages should query")
            .is_empty()
    );
    first_memory
        .remove_region(dynamic_range)
        .expect("dynamic view should remove");
    assert!(!tracker.contains_range(dynamic_range));
    first_memory
        .insert_region(dynamic_range)
        .expect("dynamic view should reinsert from the retained reservation");
    assert!(tracker.contains_range(dynamic_range));
    assert!(
        !tracker
            .dirty_pages()
            .expect("reinserted dirty pages should query")
            .is_empty(),
        "reinserted dynamic memory must be conservatively dirty"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn exact_minor_ten_materialization_cancels_at_every_accumulated_stage_and_retries() {
    let directory = TestDirectory::new("mixed-cancel");
    let memory_hotplug_payload = fixture_bytes(include_str!(
        "../snapshot_memory_hotplug_v2_10/fixtures/active-pci.hex"
    ));
    let state = SnapshotV2MemoryHotplugState::decode(
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
        &memory_hotplug_payload,
    )
    .expect("active exact 2.10 virtio-mem fixture should decode");
    let memory = test_v2_memory_with_hotplug(&state);
    let mut image = Cursor::new(Vec::new());
    let binding = write_snapshot_v2_memory_image_with_compatibility_version(
        &memory,
        &mut image,
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
    )
    .expect("mixed exact 2.10 memory should encode");
    let state_bytes = memory_hotplug_v2_10_state(
        &binding,
        None,
        &[],
        &[],
        &[(
            NATIVE_V2_MEMORY_HOTPLUG_COMPONENT_KEY,
            SnapshotV2ComponentDisposition::Semantic,
            memory_hotplug_payload.as_slice(),
        )],
    )
    .expect("mixed exact 2.10 state should encode");
    let source_path = directory.path.join("memory.snap");
    fs::write(&source_path, image.into_inner()).expect("source image should write");

    let stages = [
        SnapshotV2MemoryHotplugMaterializationStage::SourceValidation,
        SnapshotV2MemoryHotplugMaterializationStage::BaseInventory,
        SnapshotV2MemoryHotplugMaterializationStage::BaseMappings,
        SnapshotV2MemoryHotplugMaterializationStage::BaseStability,
        SnapshotV2MemoryHotplugMaterializationStage::ApertureReservation,
        SnapshotV2MemoryHotplugMaterializationStage::PluggedViews,
        SnapshotV2MemoryHotplugMaterializationStage::CopyBuffer,
        SnapshotV2MemoryHotplugMaterializationStage::DynamicCopy,
        SnapshotV2MemoryHotplugMaterializationStage::DirtyTracking,
        SnapshotV2MemoryHotplugMaterializationStage::FinalStability,
        SnapshotV2MemoryHotplugMaterializationStage::Complete,
    ];
    for target in stages {
        let error = prepared_memory_hotplug_candidate(state_bytes.clone())
            .materialize_memory_file_with_cancel(
                File::open(&source_path).expect("source should reopen read-only"),
                |stage| stage == target,
            )
            .expect_err("targeted checkpoint should cancel");
        assert!(matches!(
            error,
            SnapshotV2MemoryHotplugMaterializationError::Cancelled { stage }
                if stage == target
        ));
        let diagnostic = format!("{error:?} {error}");
        assert!(!diagnostic.contains(&state.config_space().addr().to_string()));
        assert!(!diagnostic.contains("memory.snap"));
    }

    for (target, target_occurrence) in [
        (SnapshotV2MemoryHotplugMaterializationStage::PluggedViews, 2),
        (SnapshotV2MemoryHotplugMaterializationStage::DynamicCopy, 2),
    ] {
        let mut occurrence = 0;
        let error = prepared_memory_hotplug_candidate(state_bytes.clone())
            .materialize_memory_file_with_cancel(
                File::open(&source_path).expect("source should reopen read-only"),
                |stage| {
                    if stage == target {
                        occurrence += 1;
                    }
                    stage == target && occurrence == target_occurrence
                },
            )
            .expect_err("later repeated checkpoint should cancel");
        assert!(matches!(
            error,
            SnapshotV2MemoryHotplugMaterializationError::Cancelled { stage }
                if stage == target
        ));
        assert_eq!(occurrence, target_occurrence);
    }

    prepared_memory_hotplug_candidate(state_bytes)
        .materialize_memory_file(
            File::open(&source_path).expect("source should reopen after every rollback"),
        )
        .expect("a fresh transaction should succeed after every cancellation");
}

#[cfg(target_os = "macos")]
#[test]
fn exact_minor_ten_materialization_detects_both_mutation_windows_and_short_reads() {
    let directory = TestDirectory::new("mixed-mutate");
    let memory_hotplug_payload = fixture_bytes(include_str!(
        "../snapshot_memory_hotplug_v2_10/fixtures/active-pci.hex"
    ));
    let state = SnapshotV2MemoryHotplugState::decode(
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
        &memory_hotplug_payload,
    )
    .expect("active exact 2.10 virtio-mem fixture should decode");
    let memory = test_v2_memory_with_hotplug(&state);
    let mut image = Cursor::new(Vec::new());
    let binding = write_snapshot_v2_memory_image_with_compatibility_version(
        &memory,
        &mut image,
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
    )
    .expect("mixed exact 2.10 memory should encode");
    let image = image.into_inner();
    let state_bytes = memory_hotplug_v2_10_state(
        &binding,
        None,
        &[],
        &[],
        &[(
            NATIVE_V2_MEMORY_HOTPLUG_COMPONENT_KEY,
            SnapshotV2ComponentDisposition::Semantic,
            memory_hotplug_payload.as_slice(),
        )],
    )
    .expect("mixed exact 2.10 state should encode");

    for target in [
        SnapshotV2MemoryHotplugMaterializationStage::BaseStability,
        SnapshotV2MemoryHotplugMaterializationStage::FinalStability,
    ] {
        let source_path = directory.path.join(format!("mutation-{target:?}.snap"));
        fs::write(&source_path, &image).expect("source image should write");
        let adopted = File::open(&source_path).expect("source should open read-only");
        let writable = fs::OpenOptions::new()
            .write(true)
            .open(&source_path)
            .expect("test mutation handle should open");
        let mut mutated = false;
        let error = prepared_memory_hotplug_candidate(state_bytes.clone())
            .materialize_memory_file_with_cancel(adopted, |stage| {
                if stage == target && !mutated {
                    writable
                        .write_at(&[0x5a], binding.extents()[0].file_offset())
                        .expect("test source mutation should write");
                    mutated = true;
                }
                false
            })
            .expect_err("source mutation should fail the adjacent stability gate");
        assert!(matches!(
            error,
            SnapshotV2MemoryHotplugMaterializationError::Source { stage, .. }
                if stage == target
        ));
    }

    let short_path = directory.path.join("short.snap");
    fs::write(&short_path, &image).expect("short-read source should write");
    let adopted = File::open(&short_path).expect("short-read source should open read-only");
    let writable = fs::OpenOptions::new()
        .write(true)
        .open(&short_path)
        .expect("truncate handle should open");
    let mut truncated = false;
    let error = prepared_memory_hotplug_candidate(state_bytes)
        .materialize_memory_file_with_cancel(adopted, |stage| {
            if stage == SnapshotV2MemoryHotplugMaterializationStage::DynamicCopy && !truncated {
                writable
                    .set_len(0)
                    .expect("test source truncation should succeed");
                truncated = true;
            }
            false
        })
        .expect_err("truncation during positional copy should fail");
    assert!(matches!(
        error,
        SnapshotV2MemoryHotplugMaterializationError::Read {
            stage: SnapshotV2MemoryHotplugMaterializationStage::DynamicCopy,
            kind: io::ErrorKind::UnexpectedEof,
        }
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn exact_minor_ten_dynamic_only_materialization_uses_no_placeholder_or_private_mapping() {
    let directory = TestDirectory::new("mixed-dynamic");
    let memory_hotplug_payload = fixture_bytes(include_str!(
        "../snapshot_memory_hotplug_v2_10/fixtures/inactive-mmio.hex"
    ));
    let state = SnapshotV2MemoryHotplugState::decode(
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
        &memory_hotplug_payload,
    )
    .expect("inactive exact 2.10 virtio-mem fixture should decode");
    let memory = test_v2_memory_with_hotplug_data_only(&state);
    let mut image = Cursor::new(Vec::new());
    let binding = write_snapshot_v2_memory_image_with_compatibility_version(
        &memory,
        &mut image,
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
    )
    .expect("dynamic-only exact 2.10 memory should encode");
    let state_bytes = memory_hotplug_v2_10_state(
        &binding,
        None,
        &[],
        &[],
        &[(
            NATIVE_V2_MEMORY_HOTPLUG_COMPONENT_KEY,
            SnapshotV2ComponentDisposition::Semantic,
            memory_hotplug_payload.as_slice(),
        )],
    )
    .expect("dynamic-only exact 2.10 state should encode");
    let source_path = directory.path.join("memory.snap");
    fs::write(&source_path, image.into_inner()).expect("source image should write");

    let materialized = prepared_memory_hotplug_candidate(state_bytes)
        .materialize_memory_file(
            File::open(&source_path).expect("dynamic-only source should open read-only"),
        )
        .expect("dynamic-only topology should materialize");
    let config = materialized.topology().state().config_space();
    let plugged_block_count = usize::try_from(config.plugged_size() / config.block_size())
        .expect("plugged block count should fit usize");
    assert_eq!(materialized.memory().regions().len(), plugged_block_count);
    assert!(
        materialized
            .memory()
            .regions()
            .iter()
            .all(|region| region.backing() == GuestMemoryRegionBacking::Shared)
    );
    assert!(
        materialized
            .memory()
            .dirty_tracker()
            .expect("dynamic-only dirty tracker should exist")
            .dirty_pages()
            .expect("dynamic-only dirty pages should query")
            .is_empty()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn exact_minor_ten_fresh_process_uses_distinct_unlinked_backing_and_independent_bytes() {
    let directory = TestDirectory::new("mixed-process");
    let memory_hotplug_payload = fixture_bytes(include_str!(
        "../snapshot_memory_hotplug_v2_10/fixtures/active-pci.hex"
    ));
    let state = SnapshotV2MemoryHotplugState::decode(
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
        &memory_hotplug_payload,
    )
    .expect("active exact 2.10 virtio-mem fixture should decode");
    let memory = test_v2_memory_with_hotplug(&state);
    let mut image = Cursor::new(Vec::new());
    let binding = write_snapshot_v2_memory_image_with_compatibility_version(
        &memory,
        &mut image,
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
    )
    .expect("mixed exact 2.10 memory should encode");
    let state_bytes = memory_hotplug_v2_10_state(
        &binding,
        None,
        &[],
        &[],
        &[(
            NATIVE_V2_MEMORY_HOTPLUG_COMPONENT_KEY,
            SnapshotV2ComponentDisposition::Semantic,
            memory_hotplug_payload.as_slice(),
        )],
    )
    .expect("mixed exact 2.10 state should encode");
    let state_path = directory.path.join("state.snap");
    let memory_path = directory.path.join("memory.snap");
    fs::write(&state_path, &state_bytes).expect("child state should write");
    fs::write(&memory_path, image.into_inner()).expect("child memory should write");

    let parent = prepared_memory_hotplug_candidate(state_bytes)
        .materialize_memory_file(
            File::open(&memory_path).expect("parent source should open read-only"),
        )
        .expect("parent memory should materialize");
    let aperture = GuestMemoryRange::new(
        GuestAddress::new(state.config_space().addr()),
        state.config_space().region_size(),
    )
    .expect("fixture aperture should validate");
    let parent_metadata = shared_reservation_metadata(parent.memory(), aperture);
    assert_eq!(parent_metadata.nlink(), 0);

    let executable = std::env::current_exe().expect("test executable should resolve");
    let mut child = Command::new(executable)
        .arg("--ignored")
        .arg("--exact")
        .arg("snapshot_artifact::tests::mixed_memory_materialization_child")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env("BANGBANG_MIXED_STATE", &state_path)
        .env("BANGBANG_MIXED_MEMORY", &memory_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("mixed-memory child should spawn");
    let mut child_stdin = child.stdin.take().expect("child stdin should exist");
    let mut child_stdout = BufReader::new(child.stdout.take().expect("child stdout should exist"));
    let child_identity = loop {
        let mut line = String::new();
        let count = child_stdout
            .read_line(&mut line)
            .expect("child ready output should read");
        assert_ne!(count, 0, "child exited before publishing its identity");
        let Some(marker) = line.find("mixed-child:ready ") else {
            continue;
        };
        let values = line[marker + "mixed-child:ready ".len()..]
            .split_whitespace()
            .map(|value| value.parse::<u64>().expect("child identity should parse"))
            .collect::<Vec<_>>();
        assert_eq!(values.len(), 4);
        break (values[0], values[1], values[2], values[3]);
    };
    assert_eq!(child_identity.2, 0);
    assert_ne!(
        (parent_metadata.dev(), parent_metadata.ino()),
        (child_identity.0, child_identity.1),
        "concurrently live fresh-process reservations must be different unlinked objects"
    );
    assert_eq!(child_identity.3, 0);

    let (_, _, _, _, _, topology, mut parent_memory) = parent.into_parts();
    let dynamic_start = topology
        .plugged_ranges()
        .first()
        .expect("active fixture should contain plugged memory")
        .start();
    parent_memory
        .write_slice(&[0x5a], dynamic_start)
        .expect("parent destination byte should mutate");
    child_stdin
        .write_all(&[1])
        .expect("child continuation signal should write");
    drop(child_stdin);
    let mut remainder = String::new();
    child_stdout
        .read_to_string(&mut remainder)
        .expect("child completion output should read");
    let status = child.wait().expect("mixed-memory child should wait");
    assert!(status.success(), "{remainder}");
    assert!(
        remainder.contains("mixed-child:byte 0"),
        "child destination should not observe parent mutation: {remainder}"
    );
    let mut source_byte = [0_u8; 1];
    File::open(&memory_path)
        .expect("source should reopen read-only")
        .read_exact_at(&mut source_byte, binding.extents()[1].file_offset())
        .expect("source sentinel should read");
    assert_eq!(source_byte, [0]);
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "launched by the mixed-memory fresh-process parent"]
fn mixed_memory_materialization_child() {
    let Some(state_path) = std::env::var_os("BANGBANG_MIXED_STATE") else {
        return;
    };
    let Some(memory_path) = std::env::var_os("BANGBANG_MIXED_MEMORY") else {
        return;
    };
    let state_bytes = fs::read(state_path).expect("child state should read");
    let materialized = prepared_memory_hotplug_candidate(state_bytes)
        .materialize_memory_file(
            File::open(memory_path).expect("child source should open read-only"),
        )
        .expect("child memory should materialize");
    let config = materialized.topology().state().config_space();
    let aperture = GuestMemoryRange::new(GuestAddress::new(config.addr()), config.region_size())
        .expect("child aperture should validate");
    let metadata = shared_reservation_metadata(materialized.memory(), aperture);
    let dynamic_start = materialized
        .topology()
        .plugged_ranges()
        .first()
        .expect("child should contain plugged memory")
        .start();
    let mut byte = [0_u8; 1];
    materialized
        .memory()
        .read_slice(&mut byte, dynamic_start)
        .expect("child destination byte should read");
    println!(
        "mixed-child:ready {} {} {} {}",
        metadata.dev(),
        metadata.ino(),
        metadata.nlink(),
        byte[0]
    );
    io::stdout()
        .flush()
        .expect("child ready signal should flush");
    let mut start = [0_u8; 1];
    io::stdin()
        .read_exact(&mut start)
        .expect("parent continuation signal should arrive");
    materialized
        .memory()
        .read_slice(&mut byte, dynamic_start)
        .expect("child destination byte should remain readable");
    println!("mixed-child:byte {}", byte[0]);
}

#[cfg(target_os = "macos")]
#[test]
fn exact_minor_ten_candidate_rejects_mismatched_memory_and_virtio_mem_coverage() {
    let memory = test_v2_memory();
    let mut image = Cursor::new(Vec::new());
    let binding = write_snapshot_v2_memory_image_with_compatibility_version(
        &memory,
        &mut image,
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
    )
    .expect("base exact 2.10 memory should encode");
    let memory_hotplug_payload = fixture_bytes(include_str!(
        "../snapshot_memory_hotplug_v2_10/fixtures/inactive-mmio.hex"
    ));
    let bytes = memory_hotplug_v2_10_state(
        &binding,
        None,
        &[],
        &[],
        &[(
            NATIVE_V2_MEMORY_HOTPLUG_COMPONENT_KEY,
            SnapshotV2ComponentDisposition::Semantic,
            memory_hotplug_payload.as_slice(),
        )],
    )
    .expect("mismatched product should remain structurally encodable");

    assert!(matches!(
        NativeV2MemoryHotplugSnapshotCandidateState::from_memory_hotplug_state_v2_10(bytes),
        Err(NativeV2SnapshotCandidateStateError::MemoryHotplugBinding(_))
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn exact_minor_ten_preparation_rejects_queue_missing_from_the_closed_binding() {
    let memory_hotplug_payload = fixture_bytes(include_str!(
        "../snapshot_memory_hotplug_v2_10/fixtures/active-pci.hex"
    ));
    let state = SnapshotV2MemoryHotplugState::decode(
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
        &memory_hotplug_payload,
    )
    .expect("active exact 2.10 virtio-mem fixture should decode");
    let memory = test_v2_memory_with_hotplug_data_only(&state);
    let binding = write_snapshot_v2_memory_image_with_compatibility_version(
        &memory,
        &mut Cursor::new(Vec::new()),
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
    )
    .expect("dynamic-only exact 2.10 memory should encode");
    let bytes = memory_hotplug_v2_10_state(
        &binding,
        None,
        &[],
        &[],
        &[(
            NATIVE_V2_MEMORY_HOTPLUG_COMPONENT_KEY,
            SnapshotV2ComponentDisposition::Semantic,
            memory_hotplug_payload.as_slice(),
        )],
    )
    .expect("queue-missing product should remain structurally encodable");
    let candidate =
        NativeV2MemoryHotplugSnapshotCandidateState::from_memory_hotplug_state_v2_10(bytes)
            .expect("kind-1 and plugged union should close before queue preparation");
    let error = candidate
        .prepare()
        .expect_err("queue addresses missing from kind 1 should reject");
    assert!(matches!(
        error,
        NativeV2MemoryHotplugSnapshotPreparationError::Topology(
            SnapshotV2MemoryHotplugPreparationError::QueueMemory
        )
    ));
    let diagnostic = format!("{error:?} {error}");
    assert!(!diagnostic.contains(&state.config_space().addr().to_string()));
    assert!(!diagnostic.contains("BANGME2"));
}

#[cfg(target_os = "macos")]
#[test]
fn exact_minor_ten_candidate_rejects_virtio_mem_cardinality_payload_and_version_mismatches() {
    let memory = test_v2_memory();
    let mut image = Cursor::new(Vec::new());
    let binding = write_snapshot_v2_memory_image_with_compatibility_version(
        &memory,
        &mut image,
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
    )
    .expect("exact 2.10 memory should encode internally");
    let binding_payload = binding.encode().expect("exact 2.10 binding should encode");
    let memory_hotplug_payload = fixture_bytes(include_str!(
        "../snapshot_memory_hotplug_v2_10/fixtures/inactive-mmio.hex"
    ));

    let missing_serial = encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
        &[],
        &[
            SnapshotV2Component::new(
                NATIVE_V2_MEMORY_COMPONENT_KEY,
                SnapshotV2ComponentDisposition::Semantic,
                &binding_payload,
            ),
            SnapshotV2Component::new(
                NATIVE_V2_MEMORY_HOTPLUG_COMPONENT_KEY,
                SnapshotV2ComponentDisposition::Semantic,
                &memory_hotplug_payload,
            ),
        ],
    )
    .expect("missing serial should remain structurally encodable");
    assert!(matches!(
        NativeV2MemoryHotplugSnapshotCandidateState::from_memory_hotplug_state_v2_10(
            missing_serial
        ),
        Err(NativeV2SnapshotCandidateStateError::MissingSerialState)
    ));

    for components in [
        vec![(
            SnapshotV2ComponentKey::new(NATIVE_V2_MEMORY_HOTPLUG_COMPONENT_KEY.kind(), 1),
            SnapshotV2ComponentDisposition::Semantic,
            memory_hotplug_payload.as_slice(),
        )],
        vec![(
            NATIVE_V2_MEMORY_HOTPLUG_COMPONENT_KEY,
            SnapshotV2ComponentDisposition::NonSemantic,
            memory_hotplug_payload.as_slice(),
        )],
        vec![
            (
                NATIVE_V2_MEMORY_HOTPLUG_COMPONENT_KEY,
                SnapshotV2ComponentDisposition::Semantic,
                memory_hotplug_payload.as_slice(),
            ),
            (
                SnapshotV2ComponentKey::new(NATIVE_V2_MEMORY_HOTPLUG_COMPONENT_KEY.kind(), 1),
                SnapshotV2ComponentDisposition::Semantic,
                memory_hotplug_payload.as_slice(),
            ),
        ],
    ] {
        let bytes = memory_hotplug_v2_10_state(&binding, None, &[], &[], &components)
            .expect("structural exact 2.10 fixture should encode");
        assert!(matches!(
            NativeV2MemoryHotplugSnapshotCandidateState::from_memory_hotplug_state_v2_10(bytes),
            Err(NativeV2SnapshotCandidateStateError::InvalidMemoryHotplugComponent)
        ));
    }

    let invalid_payload = [0_u8; 192];
    let invalid = memory_hotplug_v2_10_state(
        &binding,
        None,
        &[],
        &[],
        &[(
            NATIVE_V2_MEMORY_HOTPLUG_COMPONENT_KEY,
            SnapshotV2ComponentDisposition::Semantic,
            invalid_payload.as_slice(),
        )],
    )
    .expect("invalid nested virtio-mem should remain structurally encodable");
    assert!(matches!(
        NativeV2MemoryHotplugSnapshotCandidateState::from_memory_hotplug_state_v2_10(invalid),
        Err(NativeV2SnapshotCandidateStateError::MemoryHotplugState(_))
    ));

    let mut mismatched_image = Cursor::new(Vec::new());
    let mismatched_binding = write_snapshot_v2_memory_image_with_compatibility_version(
        &memory,
        &mut mismatched_image,
        NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
    )
    .expect("exact 2.9 memory fixture should encode");
    let mismatched = memory_hotplug_v2_10_state(&mismatched_binding, None, &[], &[], &[])
        .expect("mismatch should encode structurally");
    assert!(matches!(
        NativeV2MemoryHotplugSnapshotCandidateState::from_memory_hotplug_state_v2_10(mismatched),
        Err(NativeV2SnapshotCandidateStateError::VersionMismatch { .. })
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn compatible_v2_artifact_boundary_accepts_exact_minor_six_storage() {
    let memory = test_v2_memory();
    let mut image = Cursor::new(Vec::new());
    let binding = write_snapshot_v2_memory_image_with_compatibility_version(
        &memory,
        &mut image,
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
    )
    .expect("internal minor-six memory should encode");
    let binding_payload = binding
        .encode()
        .expect("internal minor-six binding should encode");
    let graph_payload = fixture_bytes(include_str!(
        "../snapshot_device_v2_6/fixtures/pmem-root-mmio.hex"
    ));
    let components = [
        SnapshotV2Component::new(
            NATIVE_V2_MEMORY_COMPONENT_KEY,
            SnapshotV2ComponentDisposition::Semantic,
            &binding_payload,
        ),
        SnapshotV2Component::new(
            NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY,
            SnapshotV2ComponentDisposition::Semantic,
            &graph_payload,
        ),
    ];
    let bytes = encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        &[],
        &components,
    )
    .expect("internal minor-six state should encode explicitly");

    let state = NativeSnapshotArtifactState::from_compatible_bytes(bytes)
        .expect("exact minor-six storage state should remain compatible");
    assert_eq!(
        state
            .v2_profile()
            .expect("exact minor-six state should classify"),
        NativeV2SnapshotArtifactProfile::StorageDeviceGraphV2_6
    );
}

#[cfg(target_os = "macos")]
#[test]
fn exact_minor_four_candidate_closes_memory_graph_and_compatible_state() {
    let memory = test_v2_memory();
    let mut image = Cursor::new(Vec::new());
    let binding = write_snapshot_v2_memory_image_with_compatibility_version(
        &memory,
        &mut image,
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
    )
    .expect("candidate memory should encode");
    let binding_payload = binding.encode().expect("candidate binding should encode");
    let graph_payload = fixture_bytes(include_str!("../snapshot_device_v2/fixtures/mmio.hex"));
    let components = [
        SnapshotV2Component::new(
            NATIVE_V2_MEMORY_COMPONENT_KEY,
            SnapshotV2ComponentDisposition::Semantic,
            &binding_payload,
        ),
        SnapshotV2Component::new(
            NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY,
            SnapshotV2ComponentDisposition::Semantic,
            &graph_payload,
        ),
    ];
    let bytes = encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        &[],
        &components,
    )
    .expect("candidate state should encode");
    let candidate = NativeV2SnapshotCandidateState::from_device_graph_v2_4(bytes.clone())
        .expect("exact candidate should close");

    assert_eq!(
        candidate.version(),
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION
    );
    assert_eq!(candidate.bytes(), bytes);
    assert_eq!(candidate.memory_binding(), &binding);
    assert_eq!(
        candidate.device_graph().transport_kind(),
        SnapshotV2DeviceTransportKind::Mmio
    );
    let debug = format!("{candidate:?}");
    assert!(debug.contains(REDACTED));
    assert!(!debug.contains("/srv/guests/rootfs.ext4"));
    assert!(!debug.contains("rootfs"));
    let compatible = candidate.into_compatible_artifact_state();
    assert_eq!(
        compatible.version(),
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION
    );
    assert_eq!(
        compatible
            .v2_profile()
            .expect("exact minor-four profile should classify"),
        NativeV2SnapshotArtifactProfile::DeviceGraphV2_4
    );
    assert!(matches!(
        NativeSnapshotArtifactState::from_current_v2(bytes),
        Err(NativeSnapshotArtifactStateError::CurrentV2Profile(
            NativeV2SnapshotCandidateStateError::UnexpectedVersion {
                found: NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION
            }
        ))
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn exact_minor_four_candidate_rejects_missing_invalid_and_mismatched_graph_state() {
    let memory = test_v2_memory();
    let mut current_image = Cursor::new(Vec::new());
    let current_binding = write_snapshot_v2_memory_image_with_compatibility_version(
        &memory,
        &mut current_image,
        crate::snapshot_format_v2::NATIVE_V2_LEGACY_PLATFORM_VERSION,
    )
    .expect("legacy binding should encode");
    let current_binding_payload = current_binding
        .encode()
        .expect("current binding payload should encode");
    let current_memory = SnapshotV2Component::new(
        NATIVE_V2_MEMORY_COMPONENT_KEY,
        SnapshotV2ComponentDisposition::Semantic,
        &current_binding_payload,
    );
    let graph_payload = fixture_bytes(include_str!("../snapshot_device_v2/fixtures/mmio.hex"));
    let graph = SnapshotV2Component::new(
        NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY,
        SnapshotV2ComponentDisposition::Semantic,
        &graph_payload,
    );
    let mismatched = encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        &[],
        &[current_memory, graph],
    )
    .expect("structural encoder should retain mismatched fixture");
    assert!(matches!(
        NativeV2SnapshotCandidateState::from_device_graph_v2_4(mismatched),
        Err(NativeV2SnapshotCandidateStateError::VersionMismatch {
            state: NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            memory: crate::snapshot_format_v2::NATIVE_V2_LEGACY_PLATFORM_VERSION,
        })
    ));

    let mut candidate_image = Cursor::new(Vec::new());
    let candidate_binding = write_snapshot_v2_memory_image_with_compatibility_version(
        &memory,
        &mut candidate_image,
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
    )
    .expect("candidate binding should encode");
    let candidate_binding_payload = candidate_binding
        .encode()
        .expect("candidate binding payload should encode");
    let candidate_memory = SnapshotV2Component::new(
        NATIVE_V2_MEMORY_COMPONENT_KEY,
        SnapshotV2ComponentDisposition::Semantic,
        &candidate_binding_payload,
    );
    let missing = encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        &[],
        &[candidate_memory],
    )
    .expect("structural encoder should retain missing graph fixture");
    assert!(matches!(
        NativeV2SnapshotCandidateState::from_device_graph_v2_4(missing),
        Err(NativeV2SnapshotCandidateStateError::MissingDeviceGraph)
    ));

    let nonsemantic_graph = SnapshotV2Component::new(
        NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY,
        SnapshotV2ComponentDisposition::NonSemantic,
        &graph_payload,
    );
    let nonsemantic = encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        &[],
        &[candidate_memory, nonsemantic_graph],
    )
    .expect("structural encoder should retain nonsemantic graph fixture");
    assert!(matches!(
        NativeV2SnapshotCandidateState::from_device_graph_v2_4(nonsemantic),
        Err(NativeV2SnapshotCandidateStateError::InvalidDeviceGraphComponent)
    ));

    let invalid_graph = [0_u8; 64];
    let invalid = SnapshotV2Component::new(
        NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY,
        SnapshotV2ComponentDisposition::Semantic,
        &invalid_graph,
    );
    let invalid = encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        &[],
        &[candidate_memory, invalid],
    )
    .expect("structural encoder should retain invalid graph fixture");
    assert!(matches!(
        NativeV2SnapshotCandidateState::from_device_graph_v2_4(invalid),
        Err(NativeV2SnapshotCandidateStateError::DeviceGraph(_))
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn exact_profile_classifier_retains_minor_three_and_rejects_cross_minor_graphs() {
    fn encoded_state(
        version: SnapshotFormatVersion,
        binding: &SnapshotV2MemoryBinding,
        graph: Option<&[u8]>,
    ) -> Vec<u8> {
        let binding = binding.encode().expect("fixture binding should encode");
        let memory = SnapshotV2Component::new(
            NATIVE_V2_MEMORY_COMPONENT_KEY,
            SnapshotV2ComponentDisposition::Semantic,
            &binding,
        );
        let graph = graph.map(|payload| {
            SnapshotV2Component::new(
                NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY,
                SnapshotV2ComponentDisposition::Semantic,
                payload,
            )
        });
        let components = match graph {
            Some(graph) => vec![memory, graph],
            None => vec![memory],
        };
        encode_snapshot_v2_state_with_compatibility_version(version, &[], &components)
            .expect("profile classifier fixture should encode")
    }

    let memory = test_v2_memory();
    let mut legacy_image = Cursor::new(Vec::new());
    let legacy_binding = write_snapshot_v2_memory_image_with_compatibility_version(
        &memory,
        &mut legacy_image,
        NATIVE_V2_LEGACY_PLATFORM_VERSION,
    )
    .expect("legacy memory binding should encode");
    let legacy = NativeSnapshotArtifactState::from_compatible_bytes(encoded_state(
        NATIVE_V2_LEGACY_PLATFORM_VERSION,
        &legacy_binding,
        None,
    ))
    .expect("legacy graphless state should prepare");
    assert_eq!(
        legacy
            .v2_profile()
            .expect("legacy graphless state should classify"),
        NativeV2SnapshotArtifactProfile::LegacyPlatformV2_3
    );

    let old_graph = fixture_bytes(include_str!("../snapshot_device_v2/fixtures/mmio.hex"));
    let mut current_image = Cursor::new(Vec::new());
    let current_binding = write_snapshot_v2_memory_image_with_compatibility_version(
        &memory,
        &mut current_image,
        NATIVE_V2_SNAPSHOT_VERSION,
    )
    .expect("current memory binding should encode");
    let current_with_old_graph = NativeSnapshotArtifactState::from_compatible_bytes(encoded_state(
        NATIVE_V2_SNAPSHOT_VERSION,
        &current_binding,
        Some(&old_graph),
    ))
    .expect("cross-minor graph should remain structurally loadable");
    assert!(matches!(
        current_with_old_graph.v2_profile(),
        Err(NativeSnapshotArtifactStateError::V2Profile(
            NativeV2SnapshotCandidateStateError::StorageDeviceGraph(_)
        ))
    ));

    let new_graph = fixture_bytes(include_str!(
        "../snapshot_device_v2_5/fixtures/root-mmio.hex"
    ));
    let mut compatibility_image = Cursor::new(Vec::new());
    let compatibility_binding = write_snapshot_v2_memory_image_with_compatibility_version(
        &memory,
        &mut compatibility_image,
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
    )
    .expect("compatibility memory binding should encode");
    let compatibility_with_new_graph =
        NativeSnapshotArtifactState::from_compatible_bytes(encoded_state(
            NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &compatibility_binding,
            Some(&new_graph),
        ))
        .expect("reverse cross-minor graph should remain structurally loadable");
    assert!(matches!(
        compatibility_with_new_graph.v2_profile(),
        Err(NativeSnapshotArtifactStateError::V2Profile(
            NativeV2SnapshotCandidateStateError::DeviceGraph(_)
        ))
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn publishes_and_loads_same_directory_pair() {
    let directory = TestDirectory::new("same-directory");
    let paths = directory.paths("state.snap", "memory.snap");
    let memory = test_memory();

    let outcome = publish_snapshot_artifacts(&paths, &memory).expect("publish should succeed");
    assert_eq!(outcome.durability(), SnapshotCommitDurability::Durable);
    assert!(paths.state().is_file());
    assert!(paths.memory().is_file());
    assert_eq!(
        fs::metadata(paths.state())
            .expect("state metadata should exist")
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(paths.memory())
            .expect("memory metadata should exist")
            .mode()
            & 0o777,
        0o600
    );
    assert_no_staging(&directory.path);

    let loaded = load_snapshot_artifacts(&paths).expect("committed pair should load");
    assert_eq!(loaded.record(), outcome.record());
    let mut actual = vec![0; TEST_MEMORY_BYTES];
    loaded
        .memory()
        .read_slice(&mut actual, GuestAddress::new(0x4000))
        .expect("loaded memory should be readable");
    assert_eq!(actual, test_bytes());

    let native =
        load_native_snapshot_artifacts(&paths).expect("v1 pair should use native-family loader");
    assert_eq!(native.family(), NativeSnapshotArtifactFamily::V1);
    assert_eq!(
        native
            .state()
            .v1_record()
            .expect("native-family v1 load should retain its record"),
        outcome.record()
    );
}

#[cfg(target_os = "macos")]
fn fixture_bytes(hex: &str) -> Vec<u8> {
    let hex = hex.split_whitespace().collect::<String>();
    assert!(hex.len().is_multiple_of(2));
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).expect("fixture hex should be UTF-8");
            u8::from_str_radix(pair, 16).expect("fixture hex should decode")
        })
        .collect()
}

#[cfg(target_os = "macos")]
#[test]
fn native_v1_adapter_preserves_legacy_publication_bytes_and_outcome() {
    let directory = TestDirectory::new("native-v1-adapter");
    let legacy_paths = directory.paths("legacy-state.snap", "legacy-memory.snap");
    let native_paths = directory.paths("native-state.snap", "native-memory.snap");
    let mut image = Cursor::new(Vec::new());
    let binding =
        write_snapshot_memory_image(&test_memory(), &mut image).expect("v1 memory should encode");
    let image = image.into_inner();
    let record = SnapshotCommitRecord::new(binding);

    let legacy = publish_snapshot_artifacts_with(&legacy_paths, |mut writer| {
        writer
            .write_all(&image)
            .expect("legacy adapter should write the fixture image");
        Ok::<_, io::Error>(record.clone())
    })
    .expect("legacy adapter should publish");
    let native = publish_native_snapshot_artifacts_with(&native_paths, |mut writer| {
        writer
            .write_all(&image)
            .expect("native adapter should write the fixture image");
        Ok::<_, io::Error>(NativeSnapshotArtifactState::from_v1(record.clone()))
    })
    .expect("native family adapter should publish v1");

    assert_eq!(native.family(), NativeSnapshotArtifactFamily::V1);
    assert_eq!(native.durability(), legacy.durability());
    assert_eq!(
        native
            .state()
            .v1_record()
            .expect("native outcome should retain the exact v1 record"),
        legacy.record()
    );
    assert_eq!(
        fs::read(native_paths.state()).expect("native state should read"),
        fs::read(legacy_paths.state()).expect("legacy state should read")
    );
    assert_eq!(
        fs::read(native_paths.memory()).expect("native memory should read"),
        fs::read(legacy_paths.memory()).expect("legacy memory should read")
    );
    load_snapshot_artifacts(&native_paths)
        .expect("legacy loader should accept the native-v1 adapter output");
    load_native_snapshot_artifacts(&legacy_paths)
        .expect("native-family loader should accept the legacy output");

    let prepared = prepare_native_snapshot_state_path(native_paths.state())
        .expect("native-family state preparation should accept v1");
    let prepared = prepared
        .into_v1()
        .expect("prepared v1 state should convert without re-encoding");
    assert_eq!(prepared.record(), &record);

    let loaded = load_native_snapshot_artifacts(&native_paths)
        .expect("native-family pair should load through the shared dispatcher");
    let loaded = loaded
        .into_v1()
        .expect("loaded v1 pair should convert without copying its guest memory");
    assert_eq!(loaded.record(), &record);
    let mut actual = vec![0; TEST_MEMORY_BYTES];
    loaded
        .memory()
        .read_slice(&mut actual, GuestAddress::new(0x4000))
        .expect("converted v1 guest memory should remain readable");
    assert_eq!(actual, test_bytes());
}

#[cfg(target_os = "macos")]
#[test]
fn publishes_and_loads_native_v2_with_retained_private_cow_memory() {
    let directory = TestDirectory::new("native-v2");
    let paths = directory.paths("state.snap", "memory.snap");
    let outcome = publish_test_v2(&paths);

    assert_eq!(outcome.family(), NativeSnapshotArtifactFamily::V2);
    assert_eq!(outcome.durability(), SnapshotCommitDurability::Durable);
    assert_eq!(
        fs::read(paths.state()).expect("published state should read"),
        outcome
            .state()
            .v2_bytes()
            .expect("v2 outcome should retain exact state bytes")
    );
    assert_no_staging(&directory.path);

    let prepared = prepare_native_snapshot_state_path(paths.state())
        .expect("native-v2 state should prepare independently");
    assert_eq!(prepared.family(), NativeSnapshotArtifactFamily::V2);
    let prepared_loaded = load_prepared_native_snapshot_memory_path(prepared, paths.memory())
        .expect("prepared native-v2 state should load its memory independently");
    assert_eq!(prepared_loaded.family(), NativeSnapshotArtifactFamily::V2);

    let first =
        load_native_snapshot_artifacts(&paths).expect("native-v2 pair should load directly");
    let second =
        load_native_snapshot_artifacts(&paths).expect("native-v2 pair should load repeatedly");
    assert_eq!(first.family(), NativeSnapshotArtifactFamily::V2);
    assert_eq!(first.memory().backing(), GuestMemoryBacking::Anonymous);
    assert!(
        first
            .memory()
            .regions()
            .iter()
            .all(|region| region.backing() == GuestMemoryRegionBacking::PrivateFile)
    );
    let (first_state, mut first_memory) = first.into_parts();
    assert_eq!(first_state.family(), NativeSnapshotArtifactFamily::V2);

    let address = GuestAddress::new(aarch64::DRAM_MEM_START);
    let mut original = vec![0; TEST_MEMORY_BYTES];
    first_memory
        .read_slice(&mut original, address)
        .expect("first v2 mapping should read");
    assert_eq!(original, test_bytes());

    let replacement = vec![0xa5; TEST_MEMORY_BYTES];
    first_memory
        .write_slice(&replacement, address)
        .expect("first v2 mapping should accept private writes");
    let mut observed = vec![0; TEST_MEMORY_BYTES];
    second
        .memory()
        .read_slice(&mut observed, address)
        .expect("second v2 mapping should remain isolated");
    assert_eq!(observed, original);

    let binding = outcome
        .state()
        .v2_memory_binding()
        .expect("published v2 state should retain its binding");
    let source = fs::read(paths.memory()).expect("source memory image should read");
    let start = usize::try_from(binding.extents()[0].file_offset())
        .expect("fixture file offset should fit usize");
    assert_eq!(
        source
            .get(start..start + TEST_MEMORY_BYTES)
            .expect("source guest bytes should exist"),
        original
    );
}

#[cfg(target_os = "macos")]
#[test]
fn native_v2_opened_pair_survives_path_replacement_and_rejects_rw_memory() {
    let directory = TestDirectory::new("v2-opened");
    let paths = directory.paths("state.snap", "memory.snap");
    publish_test_v2(&paths);
    let state = File::open(paths.state()).expect("state should open");
    let memory = File::open(paths.memory()).expect("memory should open");

    let moved_state = directory.path.join("state-original.snap");
    fs::rename(paths.state(), &moved_state).expect("state should move");
    fs::write(paths.state(), b"replacement state").expect("replacement state should create");
    let moved_memory = directory.path.join("memory-original.snap");
    fs::rename(paths.memory(), &moved_memory).expect("memory should move");
    fs::write(paths.memory(), b"replacement memory").expect("replacement memory should create");

    let loaded = load_native_snapshot_artifact_files(state, memory)
        .expect("opened v2 pair should retain original identities");
    assert_eq!(loaded.family(), NativeSnapshotArtifactFamily::V2);
    let mut bytes = vec![0; TEST_MEMORY_BYTES];
    loaded
        .memory()
        .read_slice(&mut bytes, GuestAddress::new(aarch64::DRAM_MEM_START))
        .expect("retained v2 memory should read");
    assert_eq!(bytes, test_bytes());

    let state = File::open(&moved_state).expect("original state should reopen");
    let memory = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&moved_memory)
        .expect("memory should open read-write");
    let error = load_native_snapshot_artifact_files(state, memory)
        .expect_err("v2 retained loader must reject read-write descriptors");
    assert_eq!(error.stage(), SnapshotArtifactLoadStage::MemoryLoad);
    assert!(matches!(
        error.failure(),
        SnapshotArtifactLoadFailure::MemoryV2(SnapshotV2MemoryLoadError::DescriptorNotReadOnly)
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn supplied_files_preserve_opened_identity_after_path_replacement() {
    let directory = TestDirectory::new("supplied-load");
    let paths = directory.paths("state.snap", "memory.snap");
    let outcome = publish_snapshot_artifacts(&paths, &test_memory()).expect("pair should publish");
    let state = File::open(paths.state()).expect("state should open");
    let memory = File::open(paths.memory()).expect("memory should open");

    let moved_state = directory.path.join("state-original.snap");
    fs::rename(paths.state(), &moved_state).expect("state should move after opening");
    fs::write(paths.state(), b"replacement").expect("replacement state should create");
    let moved_memory = directory.path.join("memory-original.snap");
    fs::rename(paths.memory(), &moved_memory).expect("memory should move after opening");
    fs::write(paths.memory(), b"replacement").expect("replacement memory should create");

    let loaded = load_snapshot_artifact_files(state, memory)
        .expect("opened exact pair should load after replacement");
    assert_eq!(loaded.record(), outcome.record());
    let mut actual = vec![0; TEST_MEMORY_BYTES];
    loaded
        .memory()
        .read_slice(&mut actual, GuestAddress::new(0x4000))
        .expect("loaded memory should read");
    assert_eq!(actual, test_bytes());
}

#[cfg(target_os = "macos")]
#[test]
fn supplied_directory_anchors_publish_into_the_opened_identity() {
    let directory = TestDirectory::new("supplied-output");
    let state_anchor = File::open(&directory.path).expect("state anchor should open");
    let memory_anchor = File::open(&directory.path).expect("memory anchor should open");
    let moved = directory.path.with_extension("opened");
    fs::rename(&directory.path, &moved).expect("opened directory should move");
    fs::create_dir(&directory.path).expect("replacement directory should create");

    let outputs = SnapshotArtifactOutputs::new(
        SnapshotArtifactOutput::anchored(state_anchor, b"state.snap".to_vec()),
        SnapshotArtifactOutput::anchored(memory_anchor, b"memory.snap".to_vec()),
    );
    let debug = format!("{outputs:?}");
    assert!(!debug.contains("state.snap") && !debug.contains("memory.snap"));
    publish_snapshot_artifacts_to_with(&outputs, |mut writer| {
        let binding =
            write_snapshot_memory_image(&test_memory(), &mut writer).expect("memory should write");
        Ok::<_, io::Error>(SnapshotCommitRecord::new(binding))
    })
    .expect("anchored pair should publish");

    assert!(moved.join("state.snap").is_file());
    assert!(moved.join("memory.snap").is_file());
    assert!(!directory.path.join("state.snap").exists());
    assert!(!directory.path.join("memory.snap").exists());

    let v2_outputs = SnapshotArtifactOutputs::new(
        SnapshotArtifactOutput::anchored(
            File::open(&moved).expect("v2 state anchor should open"),
            b"state-v2.snap".to_vec(),
        ),
        SnapshotArtifactOutput::anchored(
            File::open(&moved).expect("v2 memory anchor should open"),
            b"memory-v2.snap".to_vec(),
        ),
    );
    let v2 = publish_native_snapshot_artifacts_to_with(&v2_outputs, produce_test_v2)
        .expect("anchored native-v2 pair should publish");
    assert_eq!(v2.family(), NativeSnapshotArtifactFamily::V2);
    let v2_paths =
        SnapshotArtifactPaths::new(moved.join("state-v2.snap"), moved.join("memory-v2.snap"));
    load_native_snapshot_artifacts(&v2_paths).expect("anchored native-v2 pair should load");

    fs::remove_dir_all(moved).expect("opened directory should clean up");
}

#[cfg(target_os = "macos")]
#[test]
fn supplied_directory_children_are_revalidated_before_staging() {
    for (index, child) in [b"".as_slice(), b".", b"..", b"nested/state", b"nul\0state"]
        .into_iter()
        .enumerate()
    {
        let directory = TestDirectory::new(&format!("badchild{index}"));
        let outputs = SnapshotArtifactOutputs::new(
            SnapshotArtifactOutput::anchored(
                File::open(&directory.path).expect("state anchor should open"),
                child.to_vec(),
            ),
            SnapshotArtifactOutput::anchored(
                File::open(&directory.path).expect("memory anchor should open"),
                b"memory.snap".to_vec(),
            ),
        );
        let error = publish_snapshot_artifacts_to_with::<io::Error, _>(&outputs, |_writer| {
            panic!("invalid child must fail before producer")
        })
        .expect_err("invalid supplied child should fail");
        let publication = error
            .publication()
            .expect("child rejection should be a publication error");
        assert_eq!(
            publication.stage(),
            SnapshotPublicationStage::StatePathValidation
        );
        assert!(matches!(
            publication.failure(),
            SnapshotPublicationFailure::InvalidFinalPath {
                artifact: SnapshotArtifactKind::State
            }
        ));
        assert_no_staging(&directory.path);
    }
}

#[cfg(target_os = "macos")]
#[derive(Debug, Default)]
struct TestSnapshotStagingTracker {
    active: Mutex<Vec<SnapshotStagingOwnership>>,
    records: AtomicUsize,
    clears: AtomicUsize,
    reject: Mutex<Option<SnapshotArtifactKind>>,
}

#[cfg(target_os = "macos")]
impl TestSnapshotStagingTracker {
    fn rejecting(artifact: SnapshotArtifactKind) -> Self {
        Self {
            reject: Mutex::new(Some(artifact)),
            ..Self::default()
        }
    }

    fn active(&self) -> Vec<SnapshotStagingOwnership> {
        self.active
            .lock()
            .expect("tracker state should lock")
            .clone()
    }
}

#[cfg(target_os = "macos")]
impl SnapshotStagingTracker for TestSnapshotStagingTracker {
    fn record(
        &self,
        ownership: &SnapshotStagingOwnership,
    ) -> Result<(), SnapshotStagingTrackingError> {
        if self
            .reject
            .lock()
            .map_err(|_| SnapshotStagingTrackingError)?
            .as_ref()
            == Some(&ownership.artifact())
        {
            return Err(SnapshotStagingTrackingError);
        }
        self.active
            .lock()
            .map_err(|_| SnapshotStagingTrackingError)?
            .push(ownership.clone());
        self.records.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn clear(
        &self,
        ownership: &SnapshotStagingOwnership,
    ) -> Result<(), SnapshotStagingTrackingError> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| SnapshotStagingTrackingError)?;
        let index = active
            .iter()
            .position(|candidate| candidate == ownership)
            .ok_or(SnapshotStagingTrackingError)?;
        active.swap_remove(index);
        self.clears.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn tracked_outputs(
    directory: &TestDirectory,
    tracker: &Arc<TestSnapshotStagingTracker>,
) -> SnapshotArtifactOutputs {
    SnapshotArtifactOutputs::new(
        SnapshotArtifactOutput::anchored_tracked(
            File::open(&directory.path).expect("state anchor should open"),
            b"state.snap".to_vec(),
            tracker.clone(),
        ),
        SnapshotArtifactOutput::anchored_tracked(
            File::open(&directory.path).expect("memory anchor should open"),
            b"memory.snap".to_vec(),
            tracker.clone(),
        ),
    )
}

#[cfg(target_os = "macos")]
#[test]
fn tracked_outputs_record_both_staging_inodes_before_production_and_clear_on_success() {
    let directory = TestDirectory::new("tracked-success");
    let tracker = Arc::new(TestSnapshotStagingTracker::default());
    let outputs = tracked_outputs(&directory, &tracker);

    publish_snapshot_artifacts_to_with(&outputs, |mut writer| {
        let active = tracker.active();
        assert_eq!(active.len(), 2);
        assert!(
            active
                .iter()
                .any(|ownership| ownership.artifact() == SnapshotArtifactKind::State)
        );
        assert!(
            active
                .iter()
                .any(|ownership| ownership.artifact() == SnapshotArtifactKind::Memory)
        );
        let binding =
            write_snapshot_memory_image(&test_memory(), &mut writer).expect("memory should write");
        Ok::<_, io::Error>(SnapshotCommitRecord::new(binding))
    })
    .expect("tracked pair should publish");

    assert!(tracker.active().is_empty());
    assert_eq!(tracker.records.load(Ordering::Relaxed), 2);
    assert_eq!(tracker.clears.load(Ordering::Relaxed), 2);
    assert_no_staging(&directory.path);
}

#[cfg(target_os = "macos")]
#[test]
fn tracked_outputs_abort_before_production_when_evidence_cannot_be_recorded() {
    let directory = TestDirectory::new("tracked-reject");
    let tracker = Arc::new(TestSnapshotStagingTracker::rejecting(
        SnapshotArtifactKind::State,
    ));
    let outputs = tracked_outputs(&directory, &tracker);
    let called = std::cell::Cell::new(false);

    let error = publish_snapshot_artifacts_to_with::<io::Error, _>(&outputs, |_writer| {
        called.set(true);
        Err(io::Error::other(
            "producer ran without complete durable evidence",
        ))
    })
    .expect_err("record rejection should abort publication");

    assert!(!called.get());
    assert_eq!(
        error
            .publication()
            .expect("tracking rejection should be a publication failure")
            .stage(),
        SnapshotPublicationStage::StateStagingCreate
    );
    assert!(tracker.active().is_empty());
    assert_eq!(tracker.records.load(Ordering::Relaxed), 1);
    assert_eq!(tracker.clears.load(Ordering::Relaxed), 1);
    assert_no_staging(&directory.path);
}

#[cfg(target_os = "macos")]
#[test]
fn producer_publishes_exact_composite_record_after_staging_creation() {
    use crate::snapshot_commit::SnapshotCommitKind;

    let directory = TestDirectory::new("producer-composite");
    let paths = directory.paths("state.snap", "memory.snap");
    let calls = std::cell::Cell::new(0_u8);

    let outcome = publish_snapshot_artifacts_with(&paths, |mut writer| {
        calls.set(calls.get() + 1);
        assert_eq!(staging_entry_count(&directory.path), 2);
        let binding = write_snapshot_memory_image(&test_memory(), &mut writer)
            .expect("producer memory should write");
        let record = SnapshotCommitRecord::try_new_composite(binding, b"composite-state".to_vec())
            .expect("composite record should validate");
        Ok::<_, io::Error>(record)
    })
    .expect("composite producer should publish");

    assert_eq!(calls.get(), 1);
    assert_eq!(outcome.record().kind(), SnapshotCommitKind::Composite);
    assert_eq!(
        outcome.record().composite_state(),
        Some(b"composite-state".as_slice())
    );
    let loaded = load_snapshot_artifacts(&paths).expect("composite pair should load");
    assert_eq!(loaded.record(), outcome.record());
    assert_no_staging(&directory.path);
}

#[cfg(target_os = "macos")]
#[test]
fn producer_is_not_called_before_private_staging_is_ready() {
    for (index, stage) in [
        SnapshotPublicationStage::StatePathValidation,
        SnapshotPublicationStage::MemoryPathValidation,
        SnapshotPublicationStage::StateDirectoryOpen,
        SnapshotPublicationStage::MemoryDirectoryOpen,
        SnapshotPublicationStage::AliasCheck,
        SnapshotPublicationStage::StateFinalPreflight,
        SnapshotPublicationStage::MemoryFinalPreflight,
        SnapshotPublicationStage::MemoryStagingCreate,
        SnapshotPublicationStage::StateStagingCreate,
        SnapshotPublicationStage::MemoryWrite,
    ]
    .into_iter()
    .enumerate()
    {
        let directory = TestDirectory::new(&format!("producer-not-called-{index}"));
        let paths = directory.paths("state.snap", "memory.snap");
        let calls = std::cell::Cell::new(0_u8);
        let (result, _) = macos::with_publication_failure(stage, || {
            publish_snapshot_artifacts_with(&paths, |mut writer| {
                calls.set(calls.get() + 1);
                let binding = write_snapshot_memory_image(&test_memory(), &mut writer)
                    .expect("fixture memory should write");
                Ok::<_, io::Error>(SnapshotCommitRecord::new(binding))
            })
        });

        let error = result.expect_err("injected pre-producer stage should fail");
        assert_eq!(
            error
                .publication()
                .expect("stage injection should be a publication failure")
                .stage(),
            stage
        );
        assert_eq!(calls.get(), 0);
        assert_no_staging(&directory.path);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn producer_explicit_close_satisfies_publication_gate() {
    let directory = TestDirectory::new("producer-explicit-close");
    let paths = directory.paths("state.snap", "memory.snap");

    publish_snapshot_artifacts_with(&paths, |mut writer| {
        let binding = write_snapshot_memory_image(&test_memory(), &mut writer)
            .expect("producer memory should write");
        writer.close();
        Ok::<_, io::Error>(SnapshotCommitRecord::new(binding))
    })
    .expect("explicitly closed producer should publish");

    load_snapshot_artifacts(&paths).expect("explicit-close pair should load");
}

#[cfg(target_os = "macos")]
#[test]
fn retained_or_forgotten_success_writer_never_publishes() {
    let retained_directory = TestDirectory::new("producer-retained");
    let retained_paths = retained_directory.paths("state.snap", "memory.snap");
    let retained = std::cell::RefCell::new(None);
    let record = test_memory_only_record();
    let error = publish_snapshot_artifacts_with(&retained_paths, |writer| {
        *retained.borrow_mut() = Some(writer);
        Ok::<_, io::Error>(record)
    })
    .expect_err("retained writer should reject publication");
    let publication = error
        .publication()
        .expect("retained writer should be a publication failure");
    assert_eq!(
        publication.stage(),
        SnapshotPublicationStage::MemoryWriterClose
    );
    assert!(matches!(
        publication.failure(),
        SnapshotPublicationFailure::StagingWriterRetained
    ));
    assert!(!retained_paths.state().exists());
    assert!(!retained_paths.memory().exists());
    assert_no_staging(&retained_directory.path);
    drop(retained.borrow_mut().take());

    let forgotten_directory = TestDirectory::new("producer-forgotten");
    let forgotten_paths = forgotten_directory.paths("state.snap", "memory.snap");
    let record = test_memory_only_record();
    let error = publish_snapshot_artifacts_with(&forgotten_paths, |writer| {
        std::mem::forget(writer);
        Ok::<_, io::Error>(record)
    })
    .expect_err("forgotten writer should reject publication");
    assert!(matches!(
        error.publication().map(SnapshotPublicationError::failure),
        Some(SnapshotPublicationFailure::StagingWriterRetained)
    ));
    assert!(!forgotten_paths.state().exists());
    assert!(!forgotten_paths.memory().exists());
    assert_no_staging(&forgotten_directory.path);
}

#[cfg(target_os = "macos")]
#[test]
fn producer_error_owns_writer_without_leaking_diagnostics_or_staging_name() {
    struct ProducerFailure {
        _writer: SnapshotMemoryStagingWriter,
        private: &'static str,
    }

    let directory = TestDirectory::new("producer-error-writer");
    let paths = directory.paths("private-state-sentinel", "private-memory-sentinel");
    let error = publish_snapshot_artifacts_with(&paths, |writer| {
        Err::<SnapshotCommitRecord, _>(ProducerFailure {
            _writer: writer,
            private: "private-producer-sentinel",
        })
    })
    .expect_err("producer failure should abort publication");

    let producer = error
        .producer()
        .expect("typed producer error should be retained");
    assert_eq!(producer.source().private, "private-producer-sentinel");
    assert_eq!(
        producer.memory_cleanup(),
        Some(SnapshotStagingCleanup::Removed)
    );
    assert_eq!(
        producer.state_cleanup(),
        Some(SnapshotStagingCleanup::Removed)
    );
    let diagnostics = format!("{error:?} {error}");
    assert!(!diagnostics.contains("private-producer-sentinel"));
    assert!(!diagnostics.contains("private-state-sentinel"));
    assert!(!diagnostics.contains("private-memory-sentinel"));
    let diagnostic_source = std::error::Error::source(&error)
        .expect("transaction should expose only its redacted producer wrapper");
    assert!(diagnostic_source.source().is_none());
    assert_no_staging(&directory.path);

    publish_snapshot_artifacts_with(&paths, |mut writer| {
        let binding = write_snapshot_memory_image(&test_memory(), &mut writer)
            .expect("retry memory should write");
        Ok::<_, io::Error>(SnapshotCommitRecord::new(binding))
    })
    .expect("producer failure should leave the final names retryable");
    load_snapshot_artifacts(&paths).expect("retry pair should load");
}

#[cfg(target_os = "macos")]
#[test]
fn producer_error_remains_primary_when_staging_cleanup_fails() {
    for (index, (cleanup_stage, artifact)) in [
        (
            SnapshotPublicationStage::MemoryStagingCleanup,
            SnapshotArtifactKind::Memory,
        ),
        (
            SnapshotPublicationStage::StateStagingCleanup,
            SnapshotArtifactKind::State,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let directory = TestDirectory::new(&format!("producer-cleanup-failure-{index}"));
        let paths = directory.paths("state.snap", "memory.snap");
        let (result, _) = macos::with_publication_failure(cleanup_stage, || {
            publish_snapshot_artifacts_with(&paths, |_writer| {
                Err::<SnapshotCommitRecord, _>("typed producer sentinel")
            })
        });
        let error = result.expect_err("producer failure should remain primary");
        let producer = error
            .producer()
            .expect("typed producer failure should be retained");

        assert_eq!(producer.source(), &"typed producer sentinel");
        let disposition = match artifact {
            SnapshotArtifactKind::State => producer.state_cleanup(),
            SnapshotArtifactKind::Memory => producer.memory_cleanup(),
        };
        let other_disposition = match artifact {
            SnapshotArtifactKind::State => producer.memory_cleanup(),
            SnapshotArtifactKind::Memory => producer.state_cleanup(),
        };
        assert_eq!(
            disposition,
            Some(SnapshotStagingCleanup::Failed(io::ErrorKind::Other))
        );
        assert_eq!(other_disposition, Some(SnapshotStagingCleanup::Removed));
        assert!(!paths.state().exists());
        assert!(!paths.memory().exists());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn producer_panic_unwinds_staging_without_publishing_and_allows_retry() {
    let directory = TestDirectory::new("producer-panic");
    let paths = directory.paths("state.snap", "memory.snap");
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = publish_snapshot_artifacts_with::<io::Error, _>(&paths, |_writer| {
            panic!("private producer panic sentinel")
        });
    }));

    assert!(panic.is_err());
    assert!(!paths.state().exists());
    assert!(!paths.memory().exists());
    assert_no_staging(&directory.path);

    publish_snapshot_artifacts_with(&paths, |mut writer| {
        let binding = write_snapshot_memory_image(&test_memory(), &mut writer)
            .expect("retry memory should write");
        Ok::<_, io::Error>(SnapshotCommitRecord::new(binding))
    })
    .expect("panic unwind should leave final names retryable");
    load_snapshot_artifacts(&paths).expect("retry pair should load");
}

#[cfg(target_os = "macos")]
#[test]
fn producer_output_mismatch_fails_before_any_final_publication() {
    for (name, operation) in [
        ("empty", ProducerMismatch::ReturnOtherBindingWithoutWrite),
        ("extra", ProducerMismatch::AppendTrailingByte),
        ("identity", ProducerMismatch::ReturnOtherBindingAfterWrite),
        (
            "data-length",
            ProducerMismatch::ReturnDifferentLengthBindingAfterWrite,
        ),
        ("trailer", ProducerMismatch::CorruptTrailer),
    ] {
        let directory = TestDirectory::new(&format!("producer-mismatch-{name}"));
        let paths = directory.paths("state.snap", "memory.snap");
        let error = publish_snapshot_artifacts_with(&paths, |mut writer| {
            let record = match operation {
                ProducerMismatch::ReturnOtherBindingWithoutWrite => test_memory_only_record(),
                ProducerMismatch::AppendTrailingByte => {
                    let binding = write_snapshot_memory_image(&test_memory(), &mut writer)
                        .expect("fixture memory should write");
                    writer
                        .write_all(&[0xaa])
                        .expect("extra fixture byte should write");
                    SnapshotCommitRecord::new(binding)
                }
                ProducerMismatch::ReturnOtherBindingAfterWrite => {
                    write_snapshot_memory_image(&test_memory(), &mut writer)
                        .expect("fixture memory should write");
                    test_memory_only_record()
                }
                ProducerMismatch::ReturnDifferentLengthBindingAfterWrite => {
                    write_snapshot_memory_image(&test_memory(), &mut writer)
                        .expect("fixture memory should write");
                    test_memory_only_record_with_bytes(TEST_MEMORY_BYTES * 2)
                }
                ProducerMismatch::CorruptTrailer => {
                    let binding = write_snapshot_memory_image(&test_memory(), &mut writer)
                        .expect("fixture memory should write");
                    let trailer = binding
                        .file_length()
                        .checked_sub(8)
                        .expect("fixture should contain a trailer");
                    writer
                        .seek(SeekFrom::Start(trailer))
                        .expect("fixture trailer should seek");
                    writer
                        .write_all(&(binding.checksum() ^ u64::MAX).to_le_bytes())
                        .expect("fixture trailer should overwrite");
                    writer
                        .seek(SeekFrom::End(0))
                        .expect("fixture should return to end");
                    SnapshotCommitRecord::new(binding)
                }
            };
            Ok::<_, io::Error>(record)
        })
        .expect_err("mismatched producer output should fail");

        assert_eq!(
            error
                .publication()
                .expect("mismatch should be a publication failure")
                .stage(),
            SnapshotPublicationStage::MemoryWriteVerify
        );
        assert!(!paths.state().exists());
        assert!(!paths.memory().exists());
        assert_no_staging(&directory.path);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn native_v2_state_from_another_image_fails_before_publication() {
    let directory = TestDirectory::new("v2-mismatch");
    let paths = directory.paths("state.snap", "memory.snap");
    let error = publish_native_snapshot_artifacts_with(&paths, |mut writer| {
        write_snapshot_v2_memory_image(&test_v2_memory(), &mut writer)
            .map_err(|source| source.to_string())?;
        let mut other_image = Cursor::new(Vec::new());
        let other_binding = write_snapshot_v2_memory_image(&test_v2_memory(), &mut other_image)
            .map_err(|source| source.to_string())?;
        let other_state = current_v2_state(&other_binding).map_err(|source| source.to_string())?;
        NativeSnapshotArtifactState::from_current_v2(other_state)
            .map_err(|source| source.to_string())
    })
    .expect_err("unrelated native-v2 state and memory must reject");

    let publication = error
        .publication()
        .expect("binding mismatch should be a publication failure");
    assert_eq!(
        publication.stage(),
        SnapshotPublicationStage::MemoryWriteVerify
    );
    assert!(matches!(
        publication.failure(),
        SnapshotPublicationFailure::MemoryV2Verify(SnapshotV2MemoryLoadError::MemoryHeaderMismatch)
    ));
    assert!(!paths.state().exists());
    assert!(!paths.memory().exists());
    assert_no_staging(&directory.path);
}

#[cfg(target_os = "macos")]
#[test]
fn publishes_and_loads_across_directories() {
    let root = TestDirectory::new("cross-directory");
    let state_directory = root.path.join("state");
    let memory_directory = root.path.join("memory");
    fs::create_dir(&state_directory).expect("state directory should create");
    fs::create_dir(&memory_directory).expect("memory directory should create");
    let paths = SnapshotArtifactPaths::new(
        state_directory.join("state.snap"),
        memory_directory.join("memory.snap"),
    );

    publish_snapshot_artifacts(&paths, &test_memory()).expect("publish should succeed");
    load_snapshot_artifacts(&paths).expect("committed pair should load");

    let v2_paths = SnapshotArtifactPaths::new(
        state_directory.join("state-v2.snap"),
        memory_directory.join("memory-v2.snap"),
    );
    publish_test_v2(&v2_paths);
    load_native_snapshot_artifacts(&v2_paths).expect("cross-directory v2 pair should load");
    assert_no_staging(&state_directory);
    assert_no_staging(&memory_directory);
}

#[cfg(target_os = "macos")]
#[test]
fn rejects_exact_alias_before_staging() {
    let directory = TestDirectory::new("alias");
    let path = directory.path.join("same.snap");
    let paths = SnapshotArtifactPaths::new(&path, &path);

    let error =
        publish_snapshot_artifacts(&paths, &test_memory()).expect_err("alias should be rejected");
    assert_eq!(error.stage(), SnapshotPublicationStage::AliasCheck);
    assert_eq!(
        error.visibility(),
        SnapshotArtifactVisibility::NoFinalArtifact
    );
    assert!(matches!(
        error.failure(),
        SnapshotPublicationFailure::SameArtifact
    ));
    assert_no_staging(&directory.path);
}

#[cfg(target_os = "macos")]
#[test]
fn existing_final_entries_are_never_replaced() {
    for artifact in [SnapshotArtifactKind::State, SnapshotArtifactKind::Memory] {
        let directory = TestDirectory::new(match artifact {
            SnapshotArtifactKind::State => "existing-state",
            SnapshotArtifactKind::Memory => "existing-memory",
        });
        let paths = directory.paths("state.snap", "memory.snap");
        let existing = match artifact {
            SnapshotArtifactKind::State => paths.state(),
            SnapshotArtifactKind::Memory => paths.memory(),
        };
        fs::write(existing, b"sentinel").expect("fixture should create");

        let error = publish_snapshot_artifacts(&paths, &test_memory())
            .expect_err("existing final should fail");
        assert!(matches!(
            error.failure(),
            SnapshotPublicationFailure::FinalAlreadyExists { artifact: actual }
                if *actual == artifact
        ));
        assert_eq!(
            fs::read(existing).expect("fixture should remain"),
            b"sentinel"
        );
        assert_no_staging(&directory.path);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn final_symlinks_are_not_followed_or_replaced() {
    let directory = TestDirectory::new("symlink");
    let paths = directory.paths("state.snap", "memory.snap");
    let target = directory.path.join("target");
    fs::write(&target, b"sentinel").expect("target should create");
    symlink(&target, paths.state()).expect("symlink should create");

    publish_snapshot_artifacts(&paths, &test_memory()).expect_err("symlink final should fail");
    assert_eq!(
        fs::read(&target).expect("target should remain"),
        b"sentinel"
    );
    assert!(
        fs::symlink_metadata(paths.state())
            .expect("symlink should remain")
            .file_type()
            .is_symlink()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn all_existing_special_entry_types_are_preserved_on_both_paths() {
    for artifact in [SnapshotArtifactKind::State, SnapshotArtifactKind::Memory] {
        for entry_kind in [
            ExistingEntryKind::Directory,
            ExistingEntryKind::Fifo,
            ExistingEntryKind::Socket,
            ExistingEntryKind::ValidSymlink,
            ExistingEntryKind::BrokenSymlink,
        ] {
            let directory = TestDirectory::new(&format!("{artifact}-{entry_kind:?}"));
            let paths = directory.paths("state.snap", "memory.snap");
            let path = match artifact {
                SnapshotArtifactKind::State => paths.state(),
                SnapshotArtifactKind::Memory => paths.memory(),
            };
            let _guard = create_special_entry(path, entry_kind, &directory.path);
            let before = fs::symlink_metadata(path)
                .expect("special entry should exist")
                .mode()
                & u32::from(libc::S_IFMT);

            let error = publish_snapshot_artifacts(&paths, &test_memory())
                .expect_err("existing special entry should fail");

            assert!(matches!(
                error.failure(),
                SnapshotPublicationFailure::FinalAlreadyExists { artifact: actual }
                    if *actual == artifact
            ));
            let after = fs::symlink_metadata(path)
                .expect("special entry should remain")
                .mode()
                & u32::from(libc::S_IFMT);
            assert_eq!(after, before);
            assert_no_staging(&directory.path);
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn parent_symlink_aliases_use_opened_directory_identity() {
    let root = TestDirectory::new("parent-symlink-alias");
    let destination = root.path.join("destination");
    let first_parent = root.path.join("first-parent");
    let second_parent = root.path.join("second-parent");
    fs::create_dir(&destination).expect("destination should create");
    symlink(&destination, &first_parent).expect("first parent symlink should create");
    symlink(&destination, &second_parent).expect("second parent symlink should create");

    let alias = SnapshotArtifactPaths::new(
        first_parent.join("same.snap"),
        second_parent.join("same.snap"),
    );
    let error = publish_snapshot_artifacts(&alias, &test_memory())
        .expect_err("opened-directory alias should fail");
    assert!(matches!(
        error.failure(),
        SnapshotPublicationFailure::SameArtifact
    ));

    let distinct = SnapshotArtifactPaths::new(
        first_parent.join("state.snap"),
        second_parent.join("memory.snap"),
    );
    publish_snapshot_artifacts(&distinct, &test_memory())
        .expect("distinct entries in aliased parent should publish");
    load_snapshot_artifacts(&distinct).expect("aliased-parent pair should load");
}

#[cfg(target_os = "macos")]
#[test]
fn parent_path_replacement_cannot_redirect_opened_directories() {
    let root = TestDirectory::new("parent-replace");
    let parent = root.path.join("destination");
    let moved = root.path.join("opened-destination");
    fs::create_dir(&parent).expect("destination should create");
    let paths = SnapshotArtifactPaths::new(parent.join("state.snap"), parent.join("memory.snap"));
    let outcome = macos::with_parent_replacement(
        SnapshotPublicationStage::AliasCheck,
        parent.clone(),
        moved.clone(),
        || publish_snapshot_artifacts(&paths, &test_memory()),
    )
    .expect("opened directory should remain usable");

    assert_eq!(outcome.durability(), SnapshotCommitDurability::Durable);
    assert!(!paths.state().exists());
    assert!(!paths.memory().exists());
    let moved_paths =
        SnapshotArtifactPaths::new(moved.join("state.snap"), moved.join("memory.snap"));
    load_snapshot_artifacts(&moved_paths).expect("opened-directory pair should load");
    assert_no_staging(&parent);
    assert_no_staging(&moved);
}

#[cfg(target_os = "macos")]
#[test]
fn case_equivalent_names_fail_safe_or_publish_when_distinct() {
    let directory = TestDirectory::new("case-policy");
    let probe_upper = directory.path.join("CASE-PROBE");
    let probe_lower = directory.path.join("case-probe");
    fs::write(&probe_upper, b"probe").expect("probe should create");
    let case_insensitive = probe_lower.exists();
    fs::remove_file(&probe_upper).expect("probe should remove");

    let paths = directory.paths("PAIR.snap", "pair.snap");
    let result = publish_snapshot_artifacts(&paths, &test_memory());
    if case_insensitive {
        let error = result.expect_err("equivalent names should collide safely");
        assert_eq!(
            error.visibility(),
            SnapshotArtifactVisibility::MemoryOrphanVisible
        );
        assert!(paths.memory().exists());
    } else {
        result.expect("case-distinct names should publish");
        load_snapshot_artifacts(&paths).expect("case-distinct pair should load");
    }
}

#[cfg(target_os = "macos")]
#[test]
fn rejects_non_normal_final_components_before_io() {
    let directory = TestDirectory::new("invalid-final-components");
    let invalid = [
        PathBuf::new(),
        PathBuf::from("/"),
        PathBuf::from("."),
        PathBuf::from(".."),
        PathBuf::from("trailing/"),
        PathBuf::from("trailing/."),
        PathBuf::from("trailing/.."),
        PathBuf::from(std::ffi::OsString::from_vec(b"nul\0component".to_vec())),
        PathBuf::from(std::ffi::OsString::from_vec(
            b"nul\0parent/state.snap".to_vec(),
        )),
    ];

    for (index, state) in invalid.into_iter().enumerate() {
        let paths =
            SnapshotArtifactPaths::new(state, directory.path.join(format!("memory-{index}.snap")));
        let error = publish_snapshot_artifacts(&paths, &test_memory())
            .expect_err("invalid final component should fail");
        assert_eq!(error.stage(), SnapshotPublicationStage::StatePathValidation);
        assert!(matches!(
            error.failure(),
            SnapshotPublicationFailure::InvalidFinalPath {
                artifact: SnapshotArtifactKind::State
            }
        ));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn missing_and_unwritable_parents_fail_with_owned_staging_cleanup() {
    let missing_root = TestDirectory::new("missing-parent");
    let missing_state = SnapshotArtifactPaths::new(
        missing_root.path.join("missing/state.snap"),
        missing_root.path.join("memory.snap"),
    );
    let error = publish_snapshot_artifacts(&missing_state, &test_memory())
        .expect_err("missing state parent should fail");
    assert_eq!(error.stage(), SnapshotPublicationStage::StateDirectoryOpen);

    let missing_memory = SnapshotArtifactPaths::new(
        missing_root.path.join("state.snap"),
        missing_root.path.join("missing/memory.snap"),
    );
    let error = publish_snapshot_artifacts(&missing_memory, &test_memory())
        .expect_err("missing memory parent should fail");
    assert_eq!(error.stage(), SnapshotPublicationStage::MemoryDirectoryOpen);
    assert_no_staging(&missing_root.path);

    // Root bypasses ordinary mode permission checks.
    // SAFETY: `geteuid` has no arguments and does not mutate memory.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    for artifact in [SnapshotArtifactKind::Memory, SnapshotArtifactKind::State] {
        let root = TestDirectory::new(match artifact {
            SnapshotArtifactKind::State => "unwritable-state",
            SnapshotArtifactKind::Memory => "unwritable-memory",
        });
        let state_directory = root.path.join("state");
        let memory_directory = root.path.join("memory");
        fs::create_dir(&state_directory).expect("state directory should create");
        fs::create_dir(&memory_directory).expect("memory directory should create");
        let restricted = match artifact {
            SnapshotArtifactKind::State => &state_directory,
            SnapshotArtifactKind::Memory => &memory_directory,
        };
        fs::set_permissions(restricted, fs::Permissions::from_mode(0o500))
            .expect("directory should become unwritable");
        let paths = SnapshotArtifactPaths::new(
            state_directory.join("state.snap"),
            memory_directory.join("memory.snap"),
        );

        let error = publish_snapshot_artifacts(&paths, &test_memory())
            .expect_err("unwritable destination should fail");

        fs::set_permissions(restricted, fs::Permissions::from_mode(0o700))
            .expect("directory permissions should restore");
        let expected = match artifact {
            SnapshotArtifactKind::State => SnapshotPublicationStage::StateStagingCreate,
            SnapshotArtifactKind::Memory => SnapshotPublicationStage::MemoryStagingCreate,
        };
        assert_eq!(error.stage(), expected);
        assert_eq!(
            error.visibility(),
            SnapshotArtifactVisibility::NoFinalArtifact
        );
        assert_no_staging(&state_directory);
        assert_no_staging(&memory_directory);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn publication_and_load_errors_redact_paths_and_staging_names() {
    let directory = TestDirectory::new("diagnostic-redaction");
    let paths = directory.paths("SENTINEL-STATE.snap", "SENTINEL-MEMORY.snap");
    let (result, _) =
        macos::with_publication_failure(SnapshotPublicationStage::StateFileSync, || {
            publish_snapshot_artifacts(&paths, &test_memory())
        });
    let error = result.expect_err("injected sync should fail");
    for diagnostic in [format!("{error}"), format!("{error:?}")] {
        assert!(!diagnostic.contains("SENTINEL"));
        assert!(!diagnostic.contains(".bangbang-snapshot-"));
    }

    let load_paths = SnapshotArtifactPaths::new(
        directory.path.join("MISSING-STATE.snap"),
        directory.path.join("MISSING-MEMORY.snap"),
    );
    let error = load_snapshot_artifacts(&load_paths).expect_err("missing state should fail");
    for diagnostic in [format!("{error}"), format!("{error:?}")] {
        assert!(!diagnostic.contains("MISSING"));
        assert!(!diagnostic.contains(directory.path.to_string_lossy().as_ref()));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn state_publish_failure_leaves_typed_memory_orphan() {
    let directory = TestDirectory::new("state-publish-failure");
    let paths = directory.paths("state.snap", "memory.snap");
    let (result, _) =
        macos::with_publication_failure(SnapshotPublicationStage::StatePublish, || {
            publish_snapshot_artifacts(&paths, &test_memory())
        });
    let error = result.expect_err("injected state publish should fail");

    assert_eq!(
        error.visibility(),
        SnapshotArtifactVisibility::MemoryOrphanVisible
    );
    assert!(paths.memory().is_file());
    assert!(!paths.state().exists());
    assert_eq!(error.memory_cleanup(), None);
    assert_eq!(error.state_cleanup(), Some(SnapshotStagingCleanup::Removed));
    assert_no_staging(&directory.path);
}

#[cfg(target_os = "macos")]
#[test]
fn state_directory_sync_failure_is_committed_uncertain_not_error() {
    let directory = TestDirectory::new("state-directory-sync-failure");
    let paths = directory.paths("state.snap", "memory.snap");
    let (result, _) =
        macos::with_publication_failure(SnapshotPublicationStage::StateDirectorySync, || {
            publish_snapshot_artifacts(&paths, &test_memory())
        });
    let outcome = result.expect("state rename should remain committed");

    assert_eq!(
        outcome.durability(),
        SnapshotCommitDurability::Uncertain {
            kind: io::ErrorKind::Other
        }
    );
    assert!(paths.state().is_file());
    assert!(paths.memory().is_file());
    load_snapshot_artifacts(&paths).expect("visible committed pair should load");
}

#[cfg(target_os = "macos")]
#[test]
fn successful_trace_orders_file_and_directory_barriers() {
    let directory = TestDirectory::new("trace");
    let paths = directory.paths("state.snap", "memory.snap");
    let (result, order) =
        macos::with_publication_trace(|| publish_snapshot_artifacts(&paths, &test_memory()));
    result.expect("publish should succeed");

    assert_before(
        &order,
        SnapshotPublicationStage::MemoryFileSync,
        SnapshotPublicationStage::MemoryPublish,
    );
    assert_before(
        &order,
        SnapshotPublicationStage::StateFileSync,
        SnapshotPublicationStage::MemoryPublish,
    );
    assert_before(
        &order,
        SnapshotPublicationStage::MemoryPublish,
        SnapshotPublicationStage::MemoryDirectorySync,
    );
    assert_before(
        &order,
        SnapshotPublicationStage::MemoryDirectorySync,
        SnapshotPublicationStage::StatePublish,
    );
    assert_before(
        &order,
        SnapshotPublicationStage::StatePublish,
        SnapshotPublicationStage::StateDirectorySync,
    );

    let v2_directory = TestDirectory::new("trace-v2");
    let v2_paths = v2_directory.paths("state.snap", "memory.snap");
    let (v2_outcome, v2_order) = macos::with_publication_trace(|| publish_test_v2(&v2_paths));
    assert_eq!(
        v2_outcome.family(),
        NativeSnapshotArtifactFamily::V2,
        "the shared trace should still return the selected family"
    );
    assert_eq!(
        v2_order, order,
        "both family adapters must traverse the same transaction stages"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn every_pre_memory_publication_stage_failure_leaves_no_final() {
    let stages = [
        SnapshotPublicationStage::StatePathValidation,
        SnapshotPublicationStage::MemoryPathValidation,
        SnapshotPublicationStage::StateDirectoryOpen,
        SnapshotPublicationStage::MemoryDirectoryOpen,
        SnapshotPublicationStage::AliasCheck,
        SnapshotPublicationStage::StateFinalPreflight,
        SnapshotPublicationStage::MemoryFinalPreflight,
        SnapshotPublicationStage::MemoryStagingCreate,
        SnapshotPublicationStage::StateStagingCreate,
        SnapshotPublicationStage::MemoryWrite,
        SnapshotPublicationStage::MemoryWriterClose,
        SnapshotPublicationStage::MemoryWriteVerify,
        SnapshotPublicationStage::StateEncode,
        SnapshotPublicationStage::StateWrite,
        SnapshotPublicationStage::StateWriteVerify,
        SnapshotPublicationStage::MemoryFileSync,
        SnapshotPublicationStage::StateFileSync,
        SnapshotPublicationStage::MemoryPublishCheck,
        SnapshotPublicationStage::MemoryPublish,
    ];

    for (index, stage) in stages.into_iter().enumerate() {
        let directory = TestDirectory::new(&format!("pre-memory-failure-{index}"));
        let paths = directory.paths("state.snap", "memory.snap");
        let (result, order) = macos::with_publication_failure(stage, || {
            publish_snapshot_artifacts(&paths, &test_memory())
        });
        let error = result.expect_err("injected stage should fail");

        assert_eq!(error.stage(), stage);
        assert_eq!(
            error.visibility(),
            SnapshotArtifactVisibility::NoFinalArtifact
        );
        assert!(!paths.state().exists());
        assert!(!paths.memory().exists());
        assert!(order.contains(&stage));
        assert_no_staging(&directory.path);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn post_memory_pre_state_failures_leave_one_memory_orphan() {
    for (index, stage) in [
        SnapshotPublicationStage::MemoryDirectorySync,
        SnapshotPublicationStage::StatePublishCheck,
        SnapshotPublicationStage::StatePublish,
    ]
    .into_iter()
    .enumerate()
    {
        let directory = TestDirectory::new(&format!("memory-orphan-failure-{index}"));
        let paths = directory.paths("state.snap", "memory.snap");
        let (result, _) = macos::with_publication_failure(stage, || {
            publish_snapshot_artifacts(&paths, &test_memory())
        });
        let error = result.expect_err("injected stage should fail");

        assert_eq!(error.stage(), stage);
        assert_eq!(
            error.visibility(),
            SnapshotArtifactVisibility::MemoryOrphanVisible
        );
        assert!(!paths.state().exists());
        assert!(paths.memory().is_file());
        assert_eq!(error.memory_cleanup(), None);
        assert_no_staging(&directory.path);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn final_collisions_after_preflight_never_replace_the_winner() {
    let memory_directory = TestDirectory::new("late-memory-collision");
    let memory_paths = memory_directory.paths("state.snap", "memory.snap");
    let memory_result = macos::with_final_collision(
        SnapshotPublicationStage::MemoryPublish,
        memory_paths.memory().to_path_buf(),
        || publish_snapshot_artifacts(&memory_paths, &test_memory()),
    );
    let memory_error = memory_result.expect_err("late memory collision should fail");
    assert_eq!(
        memory_error.visibility(),
        SnapshotArtifactVisibility::NoFinalArtifact
    );
    assert_eq!(
        fs::read(memory_paths.memory()).expect("winner should remain"),
        b"concurrent-final"
    );
    assert!(!memory_paths.state().exists());
    assert_no_staging(&memory_directory.path);

    let state_directory = TestDirectory::new("late-state-collision");
    let state_paths = state_directory.paths("state.snap", "memory.snap");
    let state_result = macos::with_final_collision(
        SnapshotPublicationStage::StatePublish,
        state_paths.state().to_path_buf(),
        || publish_snapshot_artifacts(&state_paths, &test_memory()),
    );
    let state_error = state_result.expect_err("late state collision should fail");
    assert_eq!(
        state_error.visibility(),
        SnapshotArtifactVisibility::MemoryOrphanVisible
    );
    assert_eq!(
        fs::read(state_paths.state()).expect("winner should remain"),
        b"concurrent-final"
    );
    assert!(state_paths.memory().is_file());
    assert_no_staging(&state_directory.path);
}

#[cfg(target_os = "macos")]
#[test]
fn observed_staging_replacement_is_retained_and_refused() {
    let directory = TestDirectory::new("staging-replacement");
    let paths = directory.paths("state.snap", "memory.snap");
    let result = macos::with_staging_replacement(
        SnapshotPublicationStage::MemoryPublishCheck,
        directory.path.clone(),
        SnapshotArtifactKind::Memory,
        || publish_snapshot_artifacts(&paths, &test_memory()),
    );
    let error = result.expect_err("observed replacement should fail");

    assert_eq!(error.stage(), SnapshotPublicationStage::MemoryPublishCheck);
    assert!(matches!(
        error.failure(),
        SnapshotPublicationFailure::StagingChanged {
            artifact: SnapshotArtifactKind::Memory
        }
    ));
    assert_eq!(
        error.memory_cleanup(),
        Some(SnapshotStagingCleanup::ChangedRefused)
    );
    assert_eq!(error.state_cleanup(), Some(SnapshotStagingCleanup::Removed));
    assert!(!paths.state().exists());
    assert!(!paths.memory().exists());
    assert_eq!(
        find_staging_contents(&directory.path),
        b"replacement-staging"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn missing_staging_entry_is_reported_without_unlink_retry() {
    let directory = TestDirectory::new("staging-missing");
    let paths = directory.paths("state.snap", "memory.snap");
    let result = macos::with_staging_removal(
        SnapshotPublicationStage::MemoryPublishCheck,
        directory.path.clone(),
        SnapshotArtifactKind::Memory,
        || publish_snapshot_artifacts(&paths, &test_memory()),
    );
    let error = result.expect_err("missing staging entry should fail publication");

    assert_eq!(error.stage(), SnapshotPublicationStage::MemoryPublishCheck);
    assert_eq!(
        error.memory_cleanup(),
        Some(SnapshotStagingCleanup::AlreadyAbsent)
    );
    assert_eq!(error.state_cleanup(), Some(SnapshotStagingCleanup::Removed));
    assert!(!paths.state().exists());
    assert!(!paths.memory().exists());
    assert_no_staging(&directory.path);
}

#[cfg(target_os = "macos")]
#[test]
fn cleanup_failures_do_not_mask_the_primary_failure() {
    for (cleanup_stage, artifact) in [
        (
            SnapshotPublicationStage::MemoryStagingCleanup,
            SnapshotArtifactKind::Memory,
        ),
        (
            SnapshotPublicationStage::StateStagingCleanup,
            SnapshotArtifactKind::State,
        ),
    ] {
        let directory = TestDirectory::new(match artifact {
            SnapshotArtifactKind::State => "state-cleanup-failure",
            SnapshotArtifactKind::Memory => "memory-cleanup-failure",
        });
        let paths = directory.paths("state.snap", "memory.snap");
        let (result, _) = macos::with_publication_failures(
            vec![SnapshotPublicationStage::MemoryFileSync, cleanup_stage],
            || publish_snapshot_artifacts(&paths, &test_memory()),
        );
        let error = result.expect_err("injected file sync should fail");

        assert_eq!(error.stage(), SnapshotPublicationStage::MemoryFileSync);
        let disposition = match artifact {
            SnapshotArtifactKind::State => error.state_cleanup(),
            SnapshotArtifactKind::Memory => error.memory_cleanup(),
        };
        assert_eq!(
            disposition,
            Some(SnapshotStagingCleanup::Failed(io::ErrorKind::Other))
        );
        assert!(!paths.state().exists());
        assert!(!paths.memory().exists());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn staging_name_collisions_retry_boundedly_and_exhaust_without_clobber() {
    let retry_directory = TestDirectory::new("staging-retry");
    let retry_paths = retry_directory.paths("state.snap", "memory.snap");
    let collision = [0x11; 16];
    let collision_path = staging_fixture_path(
        &retry_directory.path,
        SnapshotArtifactKind::Memory,
        collision,
    );
    fs::write(&collision_path, b"collision-winner").expect("collision fixture should create");
    let result = macos::with_staging_random_names(vec![collision, [0x22; 16], [0x33; 16]], || {
        publish_snapshot_artifacts(&retry_paths, &test_memory())
    });
    result.expect("publisher should retry one staging collision");
    assert_eq!(
        fs::read(&collision_path).expect("collision winner should remain"),
        b"collision-winner"
    );
    fs::remove_file(&collision_path).expect("collision fixture should remove");
    assert_no_staging(&retry_directory.path);

    let exhausted_directory = TestDirectory::new("staging-exhaust");
    let exhausted_paths = exhausted_directory.paths("state.snap", "memory.snap");
    let exhausted = [0x44; 16];
    let exhausted_path = staging_fixture_path(
        &exhausted_directory.path,
        SnapshotArtifactKind::Memory,
        exhausted,
    );
    fs::write(&exhausted_path, b"collision-winner").expect("exhaustion fixture should create");
    let result = macos::with_staging_random_names(vec![exhausted; 16], || {
        publish_snapshot_artifacts(&exhausted_paths, &test_memory())
    });
    let error = result.expect_err("bounded collisions should exhaust");
    assert_eq!(error.stage(), SnapshotPublicationStage::MemoryStagingCreate);
    assert!(matches!(
        error.failure(),
        SnapshotPublicationFailure::Io(io::ErrorKind::AlreadyExists)
    ));
    assert_eq!(
        fs::read(&exhausted_path).expect("collision winner should remain"),
        b"collision-winner"
    );
    assert!(!exhausted_paths.state().exists());
    assert!(!exhausted_paths.memory().exists());
}

#[cfg(target_os = "macos")]
#[test]
fn staging_randomness_failure_precedes_creation() {
    let directory = TestDirectory::new("staging-random");
    let paths = directory.paths("state.snap", "memory.snap");
    let result =
        macos::with_staging_random_failure(|| publish_snapshot_artifacts(&paths, &test_memory()));
    let error = result.expect_err("randomness failure should abort staging");

    assert_eq!(error.stage(), SnapshotPublicationStage::MemoryStagingCreate);
    assert!(matches!(
        error.failure(),
        SnapshotPublicationFailure::RandomnessUnavailable {
            artifact: SnapshotArtifactKind::Memory
        }
    ));
    assert!(!paths.state().exists());
    assert!(!paths.memory().exists());
    assert_no_staging(&directory.path);
}

#[cfg(target_os = "macos")]
#[test]
fn multiprocess_contention_has_exactly_one_durable_winner() {
    const CHILD_COUNT: usize = 6;

    let directory = TestDirectory::new("multiprocess");
    let paths = directory.paths("state.snap", "memory.snap");
    let executable = std::env::current_exe().expect("test executable should resolve");
    let mut children = Vec::new();
    for _ in 0..CHILD_COUNT {
        let mut child = Command::new(&executable)
            .arg("--ignored")
            .arg("--exact")
            .arg("snapshot_artifact::tests::multiprocess_publication_child")
            .arg("--nocapture")
            .arg("--test-threads=1")
            .env("BANGBANG_SNAPSHOT_CHILD_STATE", paths.state())
            .env("BANGBANG_SNAPSHOT_CHILD_MEMORY", paths.memory())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("publication child should spawn");
        let stdin = child.stdin.take().expect("child stdin should exist");
        let stdout = child.stdout.take().expect("child stdout should exist");
        let mut run = PublicationChild {
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
        };
        run.wait_ready();
        children.push(run);
    }

    for child in &mut children {
        child.start();
    }

    let mut winners = 0;
    for child in children {
        let output = child.finish();
        if output.contains("publication-result:winner") {
            winners += 1;
        } else {
            assert!(output.contains("publication-result:loser"), "{output}");
        }
    }
    assert_eq!(winners, 1);
    load_snapshot_artifacts(&paths).expect("winner pair should load");
    assert_no_staging(&directory.path);
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "launched by the multiprocess contention parent"]
fn multiprocess_publication_child() {
    let Some(state) = std::env::var_os("BANGBANG_SNAPSHOT_CHILD_STATE") else {
        return;
    };
    let Some(memory) = std::env::var_os("BANGBANG_SNAPSHOT_CHILD_MEMORY") else {
        return;
    };
    println!("publication-child:ready");
    io::stdout().flush().expect("ready signal should flush");
    let mut start = [0_u8; 1];
    io::stdin()
        .read_exact(&mut start)
        .expect("start signal should arrive");

    let paths = SnapshotArtifactPaths::new(state, memory);
    match publish_snapshot_artifacts(&paths, &test_memory()) {
        Ok(outcome) => {
            assert_eq!(outcome.durability(), SnapshotCommitDurability::Durable);
            println!("publication-result:winner");
        }
        Err(error) => {
            assert_eq!(
                error.visibility(),
                SnapshotArtifactVisibility::NoFinalArtifact
            );
            assert!(matches!(
                error.failure(),
                SnapshotPublicationFailure::FinalAlreadyExists { .. }
            ));
            println!("publication-result:loser");
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn load_stops_at_absent_state_before_memory() {
    let directory = TestDirectory::new("state-absent");
    let paths = directory.paths("state.snap", "memory.snap");
    fs::write(paths.memory(), b"orphan").expect("orphan should create");

    let error = load_snapshot_artifacts(&paths).expect_err("absent state should fail");
    assert_eq!(error.stage(), SnapshotArtifactLoadStage::StateOpen);
}

#[cfg(target_os = "macos")]
#[test]
fn native_family_load_rejects_firecracker_and_unknown_state_before_memory_open() {
    let fixtures: [(&str, &[u8]); 2] = [
        (
            "firecracker",
            &[0x00, 0x00, 0x00, 0xaa, 0xaa, 0x84, 0x19, 0x10, 0x07],
        ),
        ("unknown", b"not-a-native-snapshot"),
    ];
    for (name, state) in fixtures {
        let directory = TestDirectory::new(name);
        let paths = directory.paths("state.snap", "missing-memory.snap");
        fs::write(paths.state(), state).expect("state fixture should write");

        let error = load_native_snapshot_artifacts(&paths)
            .expect_err("incompatible state must fail before memory open");
        assert_eq!(error.stage(), SnapshotArtifactLoadStage::StateDecode);
        assert!(matches!(
            error.failure(),
            SnapshotArtifactLoadFailure::NativeState(NativeSnapshotArtifactStateError::Format(
                NativeSnapshotFormatError::IncompatibleFirecrackerFormat
                    | NativeSnapshotFormatError::IncompatibleFormat
            ))
        ));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn load_rejects_corrupt_state_and_mismatched_memory() {
    let directory = TestDirectory::new("corruption");
    let paths = directory.paths("state.snap", "memory.snap");
    publish_snapshot_artifacts(&paths, &test_memory()).expect("publish should succeed");
    let mut state = fs::read(paths.state()).expect("state should read");
    *state.get_mut(0).expect("state byte should exist") ^= 0xff;
    fs::write(paths.state(), &state).expect("state should rewrite");
    let error = load_snapshot_artifacts(&paths).expect_err("corrupt state should fail");
    assert_eq!(error.stage(), SnapshotArtifactLoadStage::StateDecode);

    fs::remove_file(paths.state()).expect("state should remove");
    fs::remove_file(paths.memory()).expect("memory should remove");
    publish_snapshot_artifacts(&paths, &test_memory()).expect("republish should succeed");
    let memory_file = OpenOptionsForTest::append(paths.memory());
    drop(memory_file);
    let error = load_snapshot_artifacts(&paths).expect_err("extended memory should fail");
    assert_eq!(error.stage(), SnapshotArtifactLoadStage::MemoryLoad);
}

#[cfg(target_os = "macos")]
#[test]
fn load_rejects_nonregular_state_and_memory_without_blocking() {
    for entry_kind in [
        ExistingEntryKind::Directory,
        ExistingEntryKind::Fifo,
        ExistingEntryKind::Socket,
        ExistingEntryKind::ValidSymlink,
        ExistingEntryKind::BrokenSymlink,
    ] {
        let state_directory = TestDirectory::new(&format!("load-state-{entry_kind:?}"));
        let state_paths = state_directory.paths("state.snap", "memory.snap");
        let _guard = create_special_entry(state_paths.state(), entry_kind, &state_directory.path);
        let error =
            load_snapshot_artifacts(&state_paths).expect_err("nonregular state should be rejected");
        assert!(matches!(
            error.stage(),
            SnapshotArtifactLoadStage::StateOpen | SnapshotArtifactLoadStage::StateTypeCheck
        ));

        let memory_directory = TestDirectory::new(&format!("load-memory-{entry_kind:?}"));
        let memory_paths = memory_directory.paths("state.snap", "memory.snap");
        publish_snapshot_artifacts(&memory_paths, &test_memory())
            .expect("fixture pair should publish");
        fs::remove_file(memory_paths.memory()).expect("memory fixture should remove");
        let _guard =
            create_special_entry(memory_paths.memory(), entry_kind, &memory_directory.path);
        let error = load_snapshot_artifacts(&memory_paths)
            .expect_err("nonregular memory should be rejected");
        assert!(matches!(
            error.stage(),
            SnapshotArtifactLoadStage::MemoryOpen | SnapshotArtifactLoadStage::MemoryTypeCheck
        ));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn load_rejects_oversized_state_before_reading() {
    let directory = TestDirectory::new("oversized-state");
    let paths = directory.paths("state.snap", "memory.snap");
    let file = fs::File::create(paths.state()).expect("state fixture should create");
    file.set_len(
        u64::try_from(NATIVE_V1_SNAPSHOT_MAX_FILE_BYTES).expect("maximum should fit u64") + 1,
    )
    .expect("state fixture should resize");

    let error = load_snapshot_artifacts(&paths).expect_err("oversized state should fail");
    assert_eq!(error.stage(), SnapshotArtifactLoadStage::StateSizeCheck);
    assert!(matches!(
        error.failure(),
        SnapshotArtifactLoadFailure::StateTooLarge { .. }
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn load_rejects_swapped_truncated_and_corrupt_memory_images() {
    let first = TestDirectory::new("first-pair");
    let second = TestDirectory::new("second-pair");
    let first_paths = first.paths("state.snap", "memory.snap");
    let second_paths = second.paths("state.snap", "memory.snap");
    publish_snapshot_artifacts(&first_paths, &test_memory()).expect("first pair should publish");
    publish_snapshot_artifacts(&second_paths, &test_memory()).expect("second pair should publish");
    let temporary = first.path.join("temporary-memory");
    fs::rename(first_paths.memory(), &temporary).expect("first memory should move");
    fs::rename(second_paths.memory(), first_paths.memory()).expect("second memory should swap in");
    fs::rename(&temporary, second_paths.memory()).expect("first memory should swap out");
    let error = load_snapshot_artifacts(&first_paths).expect_err("swapped pair should fail");
    assert_eq!(error.stage(), SnapshotArtifactLoadStage::MemoryLoad);

    let truncated = TestDirectory::new("truncated-memory");
    let truncated_paths = truncated.paths("state.snap", "memory.snap");
    publish_snapshot_artifacts(&truncated_paths, &test_memory())
        .expect("truncated fixture should publish");
    let file = fs::OpenOptions::new()
        .write(true)
        .open(truncated_paths.memory())
        .expect("memory should open");
    let length = file.metadata().expect("memory metadata should read").len();
    file.set_len(length - 1).expect("memory should truncate");
    let error =
        load_snapshot_artifacts(&truncated_paths).expect_err("truncated memory should fail");
    assert_eq!(error.stage(), SnapshotArtifactLoadStage::MemoryLoad);

    let corrupt = TestDirectory::new("corrupt-memory");
    let corrupt_paths = corrupt.paths("state.snap", "memory.snap");
    publish_snapshot_artifacts(&corrupt_paths, &test_memory())
        .expect("corrupt fixture should publish");
    let mut bytes = fs::read(corrupt_paths.memory()).expect("memory should read");
    let byte = bytes.get_mut(64).expect("guest data byte should exist");
    *byte ^= 0xff;
    fs::write(corrupt_paths.memory(), bytes).expect("memory should rewrite");
    let error = load_snapshot_artifacts(&corrupt_paths).expect_err("corrupt memory should fail");
    assert_eq!(error.stage(), SnapshotArtifactLoadStage::MemoryLoad);
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct TestDirectory {
    path: PathBuf,
}

#[cfg(target_os = "macos")]
impl TestDirectory {
    fn new(name: &str) -> Self {
        let mut random = [0_u8; 8];
        getrandom::fill(&mut random).expect("test randomness should be available");
        let suffix = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let short_name = name
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .take(8)
            .collect::<String>();
        let path = Path::new("/tmp").join(format!(
            "bb-sa-{}-{short_name}-{suffix}",
            std::process::id(),
        ));
        fs::create_dir(&path).expect("test directory should create");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .expect("test permissions should set");
        Self { path }
    }

    fn paths(&self, state: &str, memory: &str) -> SnapshotArtifactPaths {
        SnapshotArtifactPaths::new(self.path.join(state), self.path.join(memory))
    }
}

#[cfg(target_os = "macos")]
impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(target_os = "macos")]
struct OpenOptionsForTest;

#[cfg(target_os = "macos")]
impl OpenOptionsForTest {
    fn append(path: &Path) -> fs::File {
        use std::fs::OpenOptions;
        let mut file = OpenOptions::new()
            .append(true)
            .open(path)
            .expect("memory should open");
        file.write_all(&[0]).expect("memory should extend");
        file
    }
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy)]
enum ExistingEntryKind {
    Directory,
    Fifo,
    Socket,
    ValidSymlink,
    BrokenSymlink,
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct ExistingEntryGuard {
    _listener: Option<UnixListener>,
}

#[cfg(target_os = "macos")]
fn create_special_entry(
    path: &Path,
    kind: ExistingEntryKind,
    directory: &Path,
) -> ExistingEntryGuard {
    let listener = match kind {
        ExistingEntryKind::Directory => {
            fs::create_dir(path).expect("directory entry should create");
            None
        }
        ExistingEntryKind::Fifo => {
            let path = std::ffi::CString::new(path.as_os_str().as_bytes())
                .expect("fixture path should not contain NUL");
            // SAFETY: the fixture path is a live NUL-terminated string and
            // the test owns its private parent directory.
            let result = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
            assert_eq!(result, 0, "FIFO fixture should create");
            None
        }
        ExistingEntryKind::Socket => {
            Some(UnixListener::bind(path).expect("Unix socket entry should create"))
        }
        ExistingEntryKind::ValidSymlink => {
            let target = directory.join("special-target");
            fs::write(&target, b"target").expect("symlink target should create");
            symlink(target, path).expect("valid symlink should create");
            None
        }
        ExistingEntryKind::BrokenSymlink => {
            symlink(directory.join("missing-target"), path).expect("broken symlink should create");
            None
        }
    };
    ExistingEntryGuard {
        _listener: listener,
    }
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct PublicationChild {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

#[cfg(target_os = "macos")]
impl PublicationChild {
    fn wait_ready(&mut self) {
        loop {
            let mut line = String::new();
            let bytes = self
                .stdout
                .read_line(&mut line)
                .expect("child ready output should read");
            assert_ne!(bytes, 0, "child exited before ready");
            if line.contains("publication-child:ready") {
                break;
            }
        }
    }

    fn start(&mut self) {
        let mut stdin = self.stdin.take().expect("child stdin should be retained");
        stdin
            .write_all(&[1])
            .expect("child start signal should write");
    }

    fn finish(mut self) -> String {
        let mut output = String::new();
        self.stdout
            .read_to_string(&mut output)
            .expect("child output should read");
        let status = self.child.wait().expect("child should wait");
        assert!(status.success(), "{output}");
        output
    }
}

#[cfg(target_os = "macos")]
fn test_memory() -> GuestMemory {
    let layout = GuestMemoryLayout::new(vec![
        GuestMemoryRange::new(
            GuestAddress::new(0x4000),
            u64::try_from(TEST_MEMORY_BYTES).expect("fixture size should fit u64"),
        )
        .expect("fixture range should be valid"),
    ])
    .expect("fixture layout should be valid");
    let mut memory = GuestMemory::allocate(&layout).expect("fixture memory should allocate");
    memory
        .write_slice(&test_bytes(), GuestAddress::new(0x4000))
        .expect("fixture bytes should write");
    memory
}

#[cfg(target_os = "macos")]
fn test_v2_memory() -> GuestMemory {
    let layout = GuestMemoryLayout::new(vec![
        GuestMemoryRange::new(
            GuestAddress::new(aarch64::DRAM_MEM_START),
            u64::try_from(TEST_MEMORY_BYTES).expect("fixture size should fit u64"),
        )
        .expect("native-v2 fixture range should be valid"),
    ])
    .expect("native-v2 fixture layout should be valid");
    let mut memory = GuestMemory::allocate(&layout).expect("native-v2 memory should allocate");
    memory
        .write_slice(&test_bytes(), GuestAddress::new(aarch64::DRAM_MEM_START))
        .expect("native-v2 fixture bytes should write");
    memory
}

#[cfg(target_os = "macos")]
fn test_v2_memory_with_hotplug(state: &SnapshotV2MemoryHotplugState) -> GuestMemory {
    let mut ranges = Vec::new();
    if let Some(queue) = state.virtio().queues().first()
        && let Some(queue_ranges) = crate::snapshot_device_v2_5::queue_ranges(queue)
            .expect("fixture queue ranges should validate")
    {
        let alignment = 4 * aarch64::GUEST_PAGE_SIZE;
        let start = queue_ranges
            .iter()
            .map(|range| range.start().raw_value())
            .min()
            .expect("active queue should have ranges")
            & !(alignment - 1);
        let end = queue_ranges
            .iter()
            .map(|range| range.end_exclusive().raw_value())
            .max()
            .expect("active queue should have ranges")
            .checked_add(alignment - 1)
            .expect("fixture queue end should fit")
            & !(alignment - 1);
        ranges.push(
            GuestMemoryRange::new(GuestAddress::new(start), end - start)
                .expect("queue base-memory fixture should validate"),
        );
    }
    ranges.push(
        GuestMemoryRange::new(
            GuestAddress::new(aarch64::DRAM_MEM_START),
            u64::try_from(TEST_MEMORY_BYTES).expect("fixture size should fit u64"),
        )
        .expect("native-v2 fixture range should be valid"),
    );
    let config_space = state.config_space();
    for plugged in state.plugged_ranges() {
        let offset = plugged
            .start_block()
            .checked_mul(config_space.block_size())
            .expect("fixture plugged offset should fit");
        let start = config_space
            .addr()
            .checked_add(offset)
            .expect("fixture plugged start should fit");
        let length = plugged
            .block_count()
            .checked_mul(config_space.block_size())
            .expect("fixture plugged length should fit");
        ranges.push(
            GuestMemoryRange::new(GuestAddress::new(start), length)
                .expect("fixture plugged range should validate"),
        );
    }
    ranges.sort_by_key(|range| range.start());
    let layout = GuestMemoryLayout::new(ranges).expect("hotplug fixture layout should validate");
    GuestMemory::allocate(&layout).expect("hotplug fixture memory should allocate")
}

#[cfg(target_os = "macos")]
fn test_v2_memory_with_hotplug_data_only(state: &SnapshotV2MemoryHotplugState) -> GuestMemory {
    let config_space = state.config_space();
    let ranges = state
        .plugged_ranges()
        .map(|plugged| {
            let offset = plugged
                .start_block()
                .checked_mul(config_space.block_size())
                .expect("fixture plugged offset should fit");
            let start = config_space
                .addr()
                .checked_add(offset)
                .expect("fixture plugged start should fit");
            let length = plugged
                .block_count()
                .checked_mul(config_space.block_size())
                .expect("fixture plugged length should fit");
            GuestMemoryRange::new(GuestAddress::new(start), length)
                .expect("fixture plugged range should validate")
        })
        .collect::<Vec<_>>();
    let layout =
        GuestMemoryLayout::new(ranges).expect("dynamic-only fixture layout should validate");
    GuestMemory::allocate(&layout).expect("dynamic-only fixture memory should allocate")
}

#[cfg(target_os = "macos")]
fn current_v2_state(binding: &SnapshotV2MemoryBinding) -> Result<Vec<u8>, String> {
    let binding_payload = binding.encode().map_err(|source| source.to_string())?;
    let graph_payload = fixture_bytes(include_str!(
        "../snapshot_device_v2_6/fixtures/block-root-mmio.hex"
    ));
    let serial_device = SerialMmioDevice::discarding()
        .capture_state()
        .map_err(|source| source.to_string())?;
    let serial_payload = SnapshotV2SerialState::try_from_capture_ready(
        CaptureReadySerialState::new(SerialConfig::default(), serial_device),
    )
    .map_err(|source| source.to_string())?
    .encode(NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION)
    .map_err(|source| source.to_string())?;
    let components = [
        SnapshotV2Component::new(
            NATIVE_V2_MEMORY_COMPONENT_KEY,
            SnapshotV2ComponentDisposition::Semantic,
            &binding_payload,
        ),
        SnapshotV2Component::new(
            NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY,
            SnapshotV2ComponentDisposition::Semantic,
            &graph_payload,
        ),
        SnapshotV2Component::new(
            NATIVE_V2_SERIAL_COMPONENT_KEY,
            SnapshotV2ComponentDisposition::Semantic,
            &serial_payload,
        ),
    ];
    encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_SNAPSHOT_VERSION,
        &[],
        &components,
    )
    .map_err(|source| source.to_string())
}

#[cfg(target_os = "macos")]
fn entropy_v2_8_state(
    binding: &SnapshotV2MemoryBinding,
    storage_payload: Option<&[u8]>,
    entropy_components: &[(
        SnapshotV2ComponentKey,
        SnapshotV2ComponentDisposition,
        &[u8],
    )],
) -> Result<Vec<u8>, String> {
    let binding_payload = binding.encode().map_err(|source| source.to_string())?;
    let serial_device = SerialMmioDevice::discarding()
        .capture_state()
        .map_err(|source| source.to_string())?;
    let serial_payload = SnapshotV2SerialState::try_from_capture_ready(
        CaptureReadySerialState::new(SerialConfig::default(), serial_device),
    )
    .map_err(|source| source.to_string())?
    .encode(NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION)
    .map_err(|source| source.to_string())?;
    let mut components = vec![SnapshotV2Component::new(
        NATIVE_V2_MEMORY_COMPONENT_KEY,
        SnapshotV2ComponentDisposition::Semantic,
        &binding_payload,
    )];
    if let Some(storage_payload) = storage_payload {
        components.push(SnapshotV2Component::new(
            NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY,
            SnapshotV2ComponentDisposition::Semantic,
            storage_payload,
        ));
    }
    components.push(SnapshotV2Component::new(
        NATIVE_V2_SERIAL_COMPONENT_KEY,
        SnapshotV2ComponentDisposition::Semantic,
        &serial_payload,
    ));
    for (key, disposition, payload) in entropy_components {
        components.push(SnapshotV2Component::new(*key, *disposition, payload));
    }
    encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
        &[],
        &components,
    )
    .map_err(|source| source.to_string())
}

#[cfg(target_os = "macos")]
fn balloon_v2_9_state(
    binding: &SnapshotV2MemoryBinding,
    storage_payload: Option<&[u8]>,
    entropy_components: &[(
        SnapshotV2ComponentKey,
        SnapshotV2ComponentDisposition,
        &[u8],
    )],
    balloon_components: &[(
        SnapshotV2ComponentKey,
        SnapshotV2ComponentDisposition,
        &[u8],
    )],
) -> Result<Vec<u8>, String> {
    let binding_payload = binding.encode().map_err(|source| source.to_string())?;
    let serial_device = SerialMmioDevice::discarding()
        .capture_state()
        .map_err(|source| source.to_string())?;
    let serial_payload = SnapshotV2SerialState::try_from_capture_ready(
        CaptureReadySerialState::new(SerialConfig::default(), serial_device),
    )
    .map_err(|source| source.to_string())?
    .encode(NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION)
    .map_err(|source| source.to_string())?;
    let mut components = vec![SnapshotV2Component::new(
        NATIVE_V2_MEMORY_COMPONENT_KEY,
        SnapshotV2ComponentDisposition::Semantic,
        &binding_payload,
    )];
    if let Some(storage_payload) = storage_payload {
        components.push(SnapshotV2Component::new(
            NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY,
            SnapshotV2ComponentDisposition::Semantic,
            storage_payload,
        ));
    }
    components.push(SnapshotV2Component::new(
        NATIVE_V2_SERIAL_COMPONENT_KEY,
        SnapshotV2ComponentDisposition::Semantic,
        &serial_payload,
    ));
    for (key, disposition, payload) in entropy_components {
        components.push(SnapshotV2Component::new(*key, *disposition, payload));
    }
    for (key, disposition, payload) in balloon_components {
        components.push(SnapshotV2Component::new(*key, *disposition, payload));
    }
    encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
        &[],
        &components,
    )
    .map_err(|source| source.to_string())
}

#[cfg(target_os = "macos")]
fn memory_hotplug_v2_10_state(
    binding: &SnapshotV2MemoryBinding,
    storage_payload: Option<&[u8]>,
    entropy_components: &[(
        SnapshotV2ComponentKey,
        SnapshotV2ComponentDisposition,
        &[u8],
    )],
    balloon_components: &[(
        SnapshotV2ComponentKey,
        SnapshotV2ComponentDisposition,
        &[u8],
    )],
    memory_hotplug_components: &[(
        SnapshotV2ComponentKey,
        SnapshotV2ComponentDisposition,
        &[u8],
    )],
) -> Result<Vec<u8>, String> {
    let binding_payload = binding.encode().map_err(|source| source.to_string())?;
    let serial_device = SerialMmioDevice::discarding()
        .capture_state()
        .map_err(|source| source.to_string())?;
    let serial_payload = SnapshotV2SerialState::try_from_capture_ready(
        CaptureReadySerialState::new(SerialConfig::default(), serial_device),
    )
    .map_err(|source| source.to_string())?
    .encode(NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION)
    .map_err(|source| source.to_string())?;
    let mut components = vec![SnapshotV2Component::new(
        NATIVE_V2_MEMORY_COMPONENT_KEY,
        SnapshotV2ComponentDisposition::Semantic,
        &binding_payload,
    )];
    if let Some(storage_payload) = storage_payload {
        components.push(SnapshotV2Component::new(
            NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY,
            SnapshotV2ComponentDisposition::Semantic,
            storage_payload,
        ));
    }
    components.push(SnapshotV2Component::new(
        NATIVE_V2_SERIAL_COMPONENT_KEY,
        SnapshotV2ComponentDisposition::Semantic,
        &serial_payload,
    ));
    for (key, disposition, payload) in entropy_components {
        components.push(SnapshotV2Component::new(*key, *disposition, payload));
    }
    for (key, disposition, payload) in balloon_components {
        components.push(SnapshotV2Component::new(*key, *disposition, payload));
    }
    for (key, disposition, payload) in memory_hotplug_components {
        components.push(SnapshotV2Component::new(*key, *disposition, payload));
    }
    encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
        &[],
        &components,
    )
    .map_err(|source| source.to_string())
}

#[cfg(target_os = "macos")]
fn prepared_memory_hotplug_candidate(
    bytes: Vec<u8>,
) -> PreparedNativeV2MemoryHotplugSnapshotCandidateState {
    let preparation =
        NativeV2MemoryHotplugSnapshotCandidateState::from_memory_hotplug_state_v2_10(bytes)
            .expect("exact 2.10 candidate should validate")
            .prepare()
            .expect("exact 2.10 topology should prepare");
    match preparation {
        NativeV2MemoryHotplugSnapshotPreparation::Prepared(candidate) => Some(candidate),
        NativeV2MemoryHotplugSnapshotPreparation::Compatible(_) => None,
    }
    .expect("kind-11 fixture should produce a prepared candidate")
}

#[cfg(target_os = "macos")]
fn shared_reservation_metadata(memory: &GuestMemory, aperture: GuestMemoryRange) -> fs::Metadata {
    let reservation = memory
        .shared_export_regions()
        .find(|region| region.range() == aperture)
        .expect("shared aperture reservation should be exportable");
    let backing = reservation
        .try_clone_shared_backing()
        .expect("shared reservation descriptor should duplicate")
        .expect("reservation should be descriptor-backed");
    let descriptor = backing
        .as_fd()
        .try_clone_to_owned()
        .expect("reservation descriptor should duplicate for inspection");
    File::from(descriptor)
        .metadata()
        .expect("reservation descriptor metadata should inspect")
}

#[cfg(target_os = "macos")]
fn produce_test_v2(
    mut writer: SnapshotMemoryStagingWriter,
) -> Result<NativeSnapshotArtifactState, String> {
    let binding = write_snapshot_v2_memory_image(&test_v2_memory(), &mut writer)
        .map_err(|source| source.to_string())?;
    let state = current_v2_state(&binding).map_err(|source| source.to_string())?;
    NativeSnapshotArtifactState::from_current_v2(state).map_err(|source| source.to_string())
}

#[cfg(target_os = "macos")]
fn publish_test_v2(paths: &SnapshotArtifactPaths) -> NativeSnapshotPublicationOutcome {
    publish_native_snapshot_artifacts_with(paths, produce_test_v2)
        .expect("native-v2 pair should publish")
}

#[cfg(target_os = "macos")]
fn test_bytes() -> Vec<u8> {
    (0..TEST_MEMORY_BYTES)
        .map(|value| u8::try_from(value % 251).expect("fixture byte should fit"))
        .collect()
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy)]
enum ProducerMismatch {
    ReturnOtherBindingWithoutWrite,
    AppendTrailingByte,
    ReturnOtherBindingAfterWrite,
    ReturnDifferentLengthBindingAfterWrite,
    CorruptTrailer,
}

#[cfg(target_os = "macos")]
fn test_memory_only_record() -> SnapshotCommitRecord {
    test_memory_only_record_with_bytes(TEST_MEMORY_BYTES)
}

#[cfg(target_os = "macos")]
fn test_memory_only_record_with_bytes(bytes: usize) -> SnapshotCommitRecord {
    let layout = GuestMemoryLayout::new(vec![
        GuestMemoryRange::new(
            GuestAddress::new(0x4000),
            u64::try_from(bytes).expect("fixture size should fit u64"),
        )
        .expect("fixture range should be valid"),
    ])
    .expect("fixture layout should be valid");
    let memory = GuestMemory::allocate(&layout).expect("fixture memory should allocate");
    let mut output = Cursor::new(Vec::new());
    let binding = write_snapshot_memory_image(&memory, &mut output)
        .expect("fixture memory record should encode");
    SnapshotCommitRecord::new(binding)
}

#[cfg(target_os = "macos")]
fn staging_entry_count(directory: &Path) -> usize {
    fs::read_dir(directory)
        .expect("directory should read")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".bangbang-snapshot-")
        })
        .count()
}

#[cfg(target_os = "macos")]
fn assert_no_staging(directory: &Path) {
    let entries = fs::read_dir(directory).expect("directory should read");
    for entry in entries {
        let name = entry
            .expect("entry should read")
            .file_name()
            .to_string_lossy()
            .into_owned();
        assert!(!name.starts_with(".bangbang-snapshot-"), "{name}");
    }
}

#[cfg(target_os = "macos")]
fn find_staging_contents(directory: &Path) -> Vec<u8> {
    let entries = fs::read_dir(directory).expect("directory should read");
    for entry in entries {
        let entry = entry.expect("entry should read");
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(".bangbang-snapshot-") {
            return fs::read(entry.path()).expect("retained staging should read");
        }
    }
    Vec::new()
}

#[cfg(target_os = "macos")]
fn staging_fixture_path(
    directory: &Path,
    artifact: SnapshotArtifactKind,
    random: [u8; 16],
) -> PathBuf {
    let role = match artifact {
        SnapshotArtifactKind::State => "state",
        SnapshotArtifactKind::Memory => "memory",
    };
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    directory.join(format!(".bangbang-snapshot-{role}-{suffix}"))
}

#[cfg(target_os = "macos")]
fn assert_before(
    order: &[SnapshotPublicationStage],
    first: SnapshotPublicationStage,
    second: SnapshotPublicationStage,
) {
    let first = order
        .iter()
        .position(|stage| *stage == first)
        .expect("first stage should be recorded");
    let second = order
        .iter()
        .position(|stage| *stage == second)
        .expect("second stage should be recorded");
    assert!(first < second);
}
