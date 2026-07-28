use super::codec::{self, ReservePolicy};
use super::*;

use crate::interrupt::GuestInterruptLine;
use crate::memory::{GuestAddress, GuestMemoryRange};
use crate::mmio::{MmioRegion, MmioRegionId};
use crate::pci::{
    PCI_BAR64_START, PCI_BUS_ZERO, PCI_FIRST_ENDPOINT_DEVICE, PCI_FUNCTION_ZERO, PCI_SEGMENT_ZERO,
    PciBarAddressSpace, PciBarPrefetchable, PciSbdf,
};
use crate::snapshot_device_v2::{
    SnapshotV2InterruptIntent, SnapshotV2MmioDeviceState, SnapshotV2PciBarProbeState,
    SnapshotV2PciDeviceState, SnapshotV2PciDeviceStateParts, SnapshotV2PciMsixState,
    SnapshotV2PciMsixStateParts, SnapshotV2PciMsixTableEntry, SnapshotV2PciWritableByte,
    SnapshotV2VirtioQueueState, SnapshotV2VirtioStateParts,
};
use crate::virtio::{
    VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER, VIRTIO_DEVICE_STATUS_DRIVER_OK,
    VIRTIO_DEVICE_STATUS_FEATURES_OK, VIRTIO_DEVICE_STATUS_INIT,
};
use crate::virtio_mmio::VIRTIO_MMIO_DEVICE_WINDOW_SIZE;
use crate::virtio_pci::{
    VIRTIO_PCI_CAPABILITY_BAR_INDEX, VIRTIO_PCI_CAPABILITY_BAR_SIZE, VirtioPciEndpointPhase,
};

const HEALTHY_DRIVER_OK: u32 = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
    | VIRTIO_DEVICE_STATUS_DRIVER
    | VIRTIO_DEVICE_STATUS_FEATURES_OK
    | VIRTIO_DEVICE_STATUS_DRIVER_OK;
const INACTIVE_MMIO_FIXTURE_HEX: &str = include_str!("fixtures/inactive-mmio.hex");
const ACTIVE_PCI_FIXTURE_HEX: &str = include_str!("fixtures/active-pci.hex");

fn inactive_mmio_state() -> SnapshotV2EntropyState {
    let bandwidth = EntropyTokenBucketConfig::new(0, Some(7), 100);
    let ops = EntropyTokenBucketConfig::new(8, Some(3), 0);
    let rate_limiter = EntropyRateLimiterConfig::new(Some(bandwidth), Some(ops));
    let config = EntropyConfig::new().with_rate_limiter(rate_limiter);
    let limiter = SnapshotV2EntropyLimiterState::try_new(Some(rate_limiter), None, None)
        .expect("disabled limiter configuration should be retained without live state");
    SnapshotV2EntropyState::try_new(
        config,
        None,
        limiter,
        SnapshotV2EntropyRetryState::None,
        false,
        inactive_virtio(),
        SnapshotV2DeviceTransport::Mmio(mmio_transport()),
    )
    .expect("inactive MMIO fixture should validate")
}

fn active_pci_state() -> SnapshotV2EntropyState {
    let bandwidth_config = EntropyTokenBucketConfig::new(4, Some(2), 100);
    let ops_config = EntropyTokenBucketConfig::new(1, Some(1), 100);
    let rate_limiter = EntropyRateLimiterConfig::new(Some(bandwidth_config), Some(ops_config));
    let config = EntropyConfig::new().with_rate_limiter(rate_limiter);
    let bandwidth = SnapshotV2EntropyBucketState::try_new(bandwidth_config, 3, 1, 25_000_000)
        .expect("bandwidth continuation should validate");
    let ops = SnapshotV2EntropyBucketState::try_new(ops_config, 0, 0, 10_000_000)
        .expect("operations continuation should validate");
    let limiter =
        SnapshotV2EntropyLimiterState::try_new(Some(rate_limiter), Some(bandwidth), Some(ops))
            .expect("enabled limiter continuation should validate");
    SnapshotV2EntropyState::try_new(
        config,
        Some(
            SnapshotV2EntropyQueueState::try_new(7, 6, VIRTIO_RNG_QUEUE_SIZE)
                .expect("active cursors should validate"),
        ),
        limiter,
        SnapshotV2EntropyRetryState::try_after(75_000_000).expect("delayed retry should validate"),
        true,
        active_virtio(),
        SnapshotV2DeviceTransport::Pci(pci_transport()),
    )
    .expect("active PCI fixture should validate")
}

fn inactive_virtio() -> SnapshotV2VirtioState {
    SnapshotV2VirtioState::from_parts(SnapshotV2VirtioStateParts {
        available_features: VIRTIO_MMIO_VERSION_1_FEATURE,
        driver_features: 0,
        config_generation: 0,
        status: VIRTIO_DEVICE_STATUS_INIT,
        activated: false,
        queues: vec![SnapshotV2VirtioQueueState::from_parts(
            VIRTIO_RNG_QUEUE_SIZE,
            0,
            false,
            GuestAddress::new(0),
            GuestAddress::new(0),
            GuestAddress::new(0),
        )],
        pending_notifications: Vec::new(),
        interrupt_intents: Vec::new(),
    })
}

fn active_virtio() -> SnapshotV2VirtioState {
    SnapshotV2VirtioState::from_parts(SnapshotV2VirtioStateParts {
        available_features: VIRTIO_MMIO_VERSION_1_FEATURE,
        driver_features: VIRTIO_MMIO_VERSION_1_FEATURE,
        config_generation: 0,
        status: HEALTHY_DRIVER_OK,
        activated: true,
        queues: vec![SnapshotV2VirtioQueueState::from_parts(
            VIRTIO_RNG_QUEUE_SIZE,
            VIRTIO_RNG_QUEUE_SIZE,
            true,
            GuestAddress::new(0x10_0000),
            GuestAddress::new(0x12_0000),
            GuestAddress::new(0x14_0000),
        )],
        pending_notifications: vec![0],
        interrupt_intents: vec![
            SnapshotV2InterruptIntent::Queue { queue_index: 0 },
            SnapshotV2InterruptIntent::Configuration,
        ],
    })
}

fn mmio_transport() -> SnapshotV2MmioDeviceState {
    let region = MmioRegion::new(
        MmioRegionId::new(100),
        GuestAddress::new(0xd000_0000),
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
    )
    .expect("fixture MMIO region should validate");
    SnapshotV2MmioDeviceState::from_parts(
        0,
        1,
        0,
        region,
        GuestInterruptLine::new(32).expect("fixture SPI should validate"),
    )
}

fn pci_transport() -> SnapshotV2PciDeviceState {
    let sbdf = PciSbdf::new(
        PCI_SEGMENT_ZERO,
        PCI_BUS_ZERO,
        PCI_FIRST_ENDPOINT_DEVICE,
        PCI_FUNCTION_ZERO,
    )
    .expect("fixture SBDF should validate");
    let bar_range = GuestMemoryRange::new(
        GuestAddress::new(PCI_BAR64_START),
        VIRTIO_PCI_CAPABILITY_BAR_SIZE,
    )
    .expect("fixture BAR should validate");
    let msix = SnapshotV2PciMsixState::from_parts(SnapshotV2PciMsixStateParts {
        entries: vec![
            SnapshotV2PciMsixTableEntry::from_parts(0x0800_0040, 0, 64, 0),
            SnapshotV2PciMsixTableEntry::from_parts(0x0800_0040, 0, 96, 1),
        ],
        pending_words: vec![0b10],
        enabled: true,
        function_masked: false,
        config_vector: 0,
        queue_vectors: vec![1],
        pending_transition_observed: true,
    });
    SnapshotV2PciDeviceState::from_parts(SnapshotV2PciDeviceStateParts {
        phase: VirtioPciEndpointPhase::Active,
        origin: StorageDeviceOrigin::Startup,
        sbdf,
        bar_index: VIRTIO_PCI_CAPABILITY_BAR_INDEX,
        bar_address_space: PciBarAddressSpace::Memory64,
        bar_prefetchable: PciBarPrefetchable::No,
        bar_range,
        device_feature_select: 1,
        driver_feature_select: 0,
        queue_select: 0,
        pci_cfg_bar: VIRTIO_PCI_CAPABILITY_BAR_INDEX,
        pci_cfg_offset: 0x24,
        pci_cfg_length: 4,
        writable_bytes: vec![
            SnapshotV2PciWritableByte::from_parts(0x04, 0x07),
            SnapshotV2PciWritableByte::from_parts(0x05, 0x80),
            SnapshotV2PciWritableByte::from_parts(0x0c, 0x40),
            SnapshotV2PciWritableByte::from_parts(0x3c, 0x2a),
        ],
        bar_probes: vec![
            SnapshotV2PciBarProbeState::from_parts(0, false),
            SnapshotV2PciBarProbeState::from_parts(1, true),
        ],
        msix,
    })
}

#[test]
fn inactive_mmio_and_active_pci_round_trip_canonically() {
    for (state, fixture) in [
        (inactive_mmio_state(), INACTIVE_MMIO_FIXTURE_HEX),
        (active_pci_state(), ACTIVE_PCI_FIXTURE_HEX),
    ] {
        let encoded = state
            .encode(NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION)
            .expect("fixture should encode");
        assert_eq!(encoded, fixture_bytes(fixture));
        let decoded =
            SnapshotV2EntropyState::decode(NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION, &encoded)
                .expect("fixture should decode");
        assert_eq!(decoded, state);
        assert_eq!(
            decoded
                .encode(NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION)
                .expect("decoded fixture should re-encode"),
            encoded
        );
    }
}

fn fixture_bytes(hex: &str) -> Vec<u8> {
    let hex = hex.trim();
    assert!(hex.len().is_multiple_of(2));
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).expect("fixture hex should be UTF-8");
            u8::from_str_radix(pair, 16).expect("fixture hex should decode")
        })
        .collect()
}

#[test]
fn disabled_external_buckets_remain_configured_without_live_state() {
    let state = inactive_mmio_state();
    let config = state
        .config()
        .rate_limiter()
        .expect("disabled external limiter should be retained");
    assert_eq!(
        config.bandwidth(),
        Some(EntropyTokenBucketConfig::new(0, Some(7), 100))
    );
    assert_eq!(
        config.ops(),
        Some(EntropyTokenBucketConfig::new(8, Some(3), 0))
    );
    assert!(!state.limiter().is_enabled());
}

#[test]
fn exact_outer_version_is_required() {
    let state = inactive_mmio_state();
    for version in [
        SnapshotFormatVersion::new(2, 7, 0),
        SnapshotFormatVersion::new(2, 9, 0),
        SnapshotFormatVersion::new(3, 8, 0),
    ] {
        assert!(matches!(
            state.encode(version),
            Err(SnapshotV2EntropyStateEncodeError::UnsupportedVersion)
        ));
        assert!(matches!(
            SnapshotV2EntropyState::decode(version, &[0; 160]),
            Err(SnapshotV2EntropyStateDecodeError::UnsupportedVersion)
        ));
    }
}

#[test]
fn header_directory_and_complete_bounds_fail_closed() {
    let encoded = inactive_mmio_state()
        .encode(NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION)
        .expect("fixture should encode");
    for length in [0, 63, 159, encoded.len() - 1] {
        assert!(
            SnapshotV2EntropyState::decode(
                NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
                &encoded[..length],
            )
            .is_err()
        );
    }

    let mut trailing = encoded.clone();
    trailing.push(0);
    assert!(matches!(
        SnapshotV2EntropyState::decode(NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION, &trailing,),
        Err(SnapshotV2EntropyStateDecodeError::InvalidStructure)
    ));

    for offset in [0, 8, 10, 14, 16, 20, 32, 40, 48, 64, 66, 68, 88] {
        let mut mutated = encoded.clone();
        mutated[offset] ^= 1;
        assert!(
            SnapshotV2EntropyState::decode(
                NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
                &mutated,
            )
            .is_err(),
            "mutation at byte {offset} should fail"
        );
    }
}

#[test]
fn every_header_and_directory_control_field_fails_closed() {
    let encoded = inactive_mmio_state()
        .encode(NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION)
        .expect("fixture should encode");
    for offset in [
        8, 10, 12, 14, 16, 20, 24, 32, 40, 48, 64, 66, 68, 72, 80, 88, 96, 98, 100, 104, 112, 120,
        128, 130, 132, 136, 144, 152,
    ] {
        let mut mutated = encoded.clone();
        mutated[offset] ^= 1;
        assert!(
            SnapshotV2EntropyState::decode(
                NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
                &mutated,
            )
            .is_err(),
            "control mutation at byte {offset} should fail"
        );
    }

    let oversized = vec![0; NATIVE_V2_ENTROPY_STATE_MAX_BYTES + 1];
    assert!(matches!(
        SnapshotV2EntropyState::decode(NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION, &oversized,),
        Err(SnapshotV2EntropyStateDecodeError::TooLarge)
    ));
}

#[test]
fn local_presence_retry_and_cursor_mutations_fail_closed() {
    let encoded = active_pci_state()
        .encode(NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION)
        .expect("fixture should encode");
    for offset in [
        160, // unknown/presence flags
        162, // retry tag
        163, // reserved
        176, // reserved prefix
        216, // bandwidth budget
    ] {
        let mut mutated = encoded.clone();
        mutated[offset] ^= 0x80;
        assert!(
            SnapshotV2EntropyState::decode(
                NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
                &mutated,
            )
            .is_err(),
            "local mutation at byte {offset} should fail"
        );
    }
}

#[test]
fn local_bucket_presence_and_pending_matrix_fails_closed() {
    let active = active_pci_state()
        .encode(NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION)
        .expect("fixture should encode");
    let mut mutations = Vec::new();

    let mut unknown_flags = active.clone();
    unknown_flags[161] |= 1;
    mutations.push(unknown_flags);

    let mut missing_bandwidth_state = active.clone();
    missing_bandwidth_state[160] &= !(1 << 2);
    mutations.push(missing_bandwidth_state);

    let mut missing_bandwidth_burst = active.clone();
    missing_bandwidth_burst[160] &= !(1 << 1);
    mutations.push(missing_bandwidth_burst);

    let mut budget_overflow = active.clone();
    replace_u64(&mut budget_overflow, 216, 5);
    mutations.push(budget_overflow);

    let mut burst_overflow = active.clone();
    replace_u64(&mut burst_overflow, 224, 3);
    mutations.push(burst_overflow);

    let mut missing_ops_state = active.clone();
    missing_ops_state[160] &= !(1 << 5);
    mutations.push(missing_ops_state);

    let mut inactive_cursors = active.clone();
    inactive_cursors[160] &= !(1 << 6);
    mutations.push(inactive_cursors);

    let mut pending_without_retry = active.clone();
    pending_without_retry[160] &= !(1 << 7);
    mutations.push(pending_without_retry);

    let mut none_with_duration = active.clone();
    none_with_duration[162] = 0;
    mutations.push(none_with_duration);

    let mut zero_delayed_retry = active.clone();
    replace_u64(&mut zero_delayed_retry, 168, 0);
    mutations.push(zero_delayed_retry);

    let mut zero_outstanding = active.clone();
    replace_u16(&mut zero_outstanding, 164, 6);
    mutations.push(zero_outstanding);

    for (index, mutated) in mutations.into_iter().enumerate() {
        assert!(
            SnapshotV2EntropyState::decode(
                NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
                &mutated,
            )
            .is_err(),
            "active local mutation {index} should fail"
        );
    }

    let inactive = inactive_mmio_state()
        .encode(NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION)
        .expect("fixture should encode");
    for state_bit in [1 << 2, 1 << 5] {
        let mut disabled_with_state = inactive.clone();
        disabled_with_state[160] |= state_bit;
        assert!(
            SnapshotV2EntropyState::decode(
                NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
                &disabled_with_state,
            )
            .is_err()
        );
    }
    let mut pending_without_owner = inactive;
    pending_without_owner[160] |= 1 << 7;
    pending_without_owner[162] = 1;
    assert!(matches!(
        SnapshotV2EntropyState::decode(
            NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
            &pending_without_owner,
        ),
        Err(SnapshotV2EntropyStateDecodeError::InvalidState(
            SnapshotV2EntropyStateBuildError::Retry
        ))
    ));
}

#[test]
fn common_virtio_hostile_fields_fail_closed() {
    let encoded = active_pci_state()
        .encode(NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION)
        .expect("fixture should encode");
    let mut mutations = Vec::new();
    for offset in [
        288, 296, 304, 308, 312, 313, 325, 328, 336, 344, 352, 354, 355, 356, 362,
    ] {
        let mut mutated = encoded.clone();
        mutated[offset] ^= 1;
        mutations.push(mutated);
    }
    for (offset, value) in [(314, 2_u16), (316, 2), (318, 3), (320, 255), (322, 3)] {
        let mut mutated = encoded.clone();
        replace_u16(&mut mutated, offset, value);
        mutations.push(mutated);
    }
    let mut duplicate_interrupt = encoded;
    duplicate_interrupt[354] = 2;
    mutations.push(duplicate_interrupt);

    for (index, mutated) in mutations.into_iter().enumerate() {
        assert!(
            SnapshotV2EntropyState::decode(
                NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
                &mutated,
            )
            .is_err(),
            "common mutation {index} should fail"
        );
    }
}

#[test]
fn mmio_and_pci_transport_hostile_fields_fail_closed() {
    let mmio = inactive_mmio_state()
        .encode(NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION)
        .expect("fixture should encode");
    let mut mmio_mutations = Vec::new();
    for (offset, value) in [(352, 2_u32), (356, 2), (360, 1), (364, 31)] {
        let mut mutated = mmio.clone();
        replace_u32(&mut mutated, offset, value);
        mmio_mutations.push(mutated);
    }
    for (offset, value) in [(368, 0_u64), (376, 0xd000_0001), (384, 0x2000)] {
        let mut mutated = mmio.clone();
        replace_u64(&mut mutated, offset, value);
        mmio_mutations.push(mutated);
    }
    let mut mmio_reserved = mmio;
    mmio_reserved[392] = 1;
    mmio_mutations.push(mmio_reserved);
    for mutated in mmio_mutations {
        assert!(SnapshotV2EntropyState::decode(
            NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
            &mutated,
        )
        .is_err());
    }

    let pci = active_pci_state()
        .encode(NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION)
        .expect("fixture should encode");
    let mut pci_mutations = Vec::new();
    for offset in [
        368, 369, 370, 371, 372, 374, 375, 376, 378, 379, 380, 384, 392, 408, 420, 435, 438, 506,
    ] {
        let mut mutated = pci.clone();
        mutated[offset] ^= 1;
        pci_mutations.push(mutated);
    }
    for offset in [400, 404] {
        let mut mutated = pci.clone();
        replace_u32(&mut mutated, offset, 2);
        pci_mutations.push(mutated);
    }
    for offset in [432, 433, 434] {
        let mut mutated = pci.clone();
        mutated[offset] = 2;
        pci_mutations.push(mutated);
    }
    for offset in [410, 412, 414, 416, 418] {
        let mut mutated = pci.clone();
        replace_u16(&mut mutated, offset, 0);
        pci_mutations.push(mutated);
    }
    for (index, mutated) in pci_mutations.into_iter().enumerate() {
        assert!(
            SnapshotV2EntropyState::decode(
                NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
                &mutated,
            )
            .is_err(),
            "PCI mutation {index} should fail"
        );
    }
}

#[test]
fn complete_builder_rejects_pending_and_common_disagreement() {
    let mut state = active_pci_state();
    state.pending = false;
    assert_eq!(
        validate_entropy_state(&state),
        Err(SnapshotV2EntropyStateBuildError::Retry)
    );
    assert!(matches!(
        state.encode(NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION),
        Err(SnapshotV2EntropyStateEncodeError::InvalidState(
            SnapshotV2EntropyStateBuildError::Retry
        ))
    ));

    let mut state = active_pci_state();
    state.active_queue = Some(SnapshotV2EntropyQueueState::from_parts(7, 7));
    assert_eq!(
        validate_entropy_state(&state),
        Err(SnapshotV2EntropyStateBuildError::Queue)
    );

    let mut state = active_pci_state();
    state.virtio = SnapshotV2VirtioState::from_parts(SnapshotV2VirtioStateParts {
        config_generation: 1,
        ..parts_from_virtio(active_virtio())
    });
    assert_eq!(
        validate_entropy_state(&state),
        Err(SnapshotV2EntropyStateBuildError::Virtio)
    );

    let mut state = active_pci_state();
    let overlapping = MmioRegion::new(
        MmioRegionId::new(200),
        GuestAddress::new(0x10_0000),
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
    )
    .expect("overlapping MMIO region should be structurally valid");
    state.transport = SnapshotV2DeviceTransport::Mmio(SnapshotV2MmioDeviceState::from_parts(
        0,
        0,
        0,
        overlapping,
        GuestInterruptLine::new(40).expect("fixture SPI should validate"),
    ));
    assert_eq!(
        validate_entropy_state(&state),
        Err(SnapshotV2EntropyStateBuildError::Placement)
    );

    let empty_config = EntropyRateLimiterConfig::new(None, None);
    assert_eq!(
        SnapshotV2EntropyLimiterState::try_new(Some(empty_config), None, None),
        Err(SnapshotV2EntropyStateBuildError::Configuration)
    );
    assert_eq!(
        SnapshotV2EntropyState::try_new(
            EntropyConfig::new().with_rate_limiter(empty_config),
            None,
            SnapshotV2EntropyLimiterState::from_parts(None, None),
            SnapshotV2EntropyRetryState::None,
            false,
            inactive_virtio(),
            SnapshotV2DeviceTransport::Mmio(mmio_transport()),
        ),
        Err(SnapshotV2EntropyStateBuildError::Configuration)
    );

    let enabled = EntropyTokenBucketConfig::new(1, None, 100);
    assert_eq!(
        SnapshotV2EntropyLimiterState::try_new(
            Some(EntropyRateLimiterConfig::new(Some(enabled), None)),
            None,
            None,
        ),
        Err(SnapshotV2EntropyStateBuildError::Limiter)
    );
}

fn parts_from_virtio(state: SnapshotV2VirtioState) -> SnapshotV2VirtioStateParts {
    SnapshotV2VirtioStateParts {
        available_features: state.available_features(),
        driver_features: state.driver_features(),
        config_generation: state.config_generation(),
        status: state.status(),
        activated: state.is_activated(),
        queues: state.queues().to_vec(),
        pending_notifications: state.pending_notifications().to_vec(),
        interrupt_intents: state.interrupt_intents().to_vec(),
    }
}

struct FailReserve {
    call: usize,
    fail_at: usize,
}

impl ReservePolicy for FailReserve {
    fn reserve_vec<T>(&mut self, values: &mut Vec<T>, additional: usize) -> Result<(), ()> {
        self.call += 1;
        if self.call == self.fail_at {
            Err(())
        } else {
            values.try_reserve_exact(additional).map_err(|_| ())
        }
    }
}

#[test]
fn allocation_failures_return_no_partial_value() {
    let state = active_pci_state();
    assert!(matches!(
        codec::encode_with_policy(
            NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
            &state,
            &mut FailReserve {
                call: 0,
                fail_at: 1,
            },
        ),
        Err(SnapshotV2EntropyStateEncodeError::Allocation)
    ));
    let encoded = state
        .encode(NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION)
        .expect("fixture should encode");
    for fail_at in 1..=8 {
        assert!(matches!(
            codec::decode_with_policy(
                NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
                &encoded,
                &mut FailReserve { call: 0, fail_at },
            ),
            Err(SnapshotV2EntropyStateDecodeError::Allocation)
        ));
    }

    let mut invalid = encoded;
    invalid[64] = 0;
    let mut reserve = FailReserve {
        call: 0,
        fail_at: 1,
    };
    assert!(matches!(
        codec::decode_with_policy(
            NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
            &invalid,
            &mut reserve,
        ),
        Err(SnapshotV2EntropyStateDecodeError::InvalidStructure)
    ));
    assert_eq!(reserve.call, 0, "preflight must precede allocation");
}

#[test]
fn diagnostics_redact_entropy_values_and_placement() {
    let state = active_pci_state();
    let debug = format!("{state:?}");
    for sentinel in [
        "1048576", "1179648", "1310720", "25000000", "75000000", "BANGEN2",
    ] {
        assert!(!debug.contains(sentinel));
    }
    assert!(debug.contains("<redacted>"));
    assert!(!format!("{:?}", state.retry()).contains("75000000"));
    assert_eq!(
        format!("{:?}", SnapshotV2EntropyStateDecodeError::InvalidValue),
        "InvalidValue"
    );
    assert_eq!(
        capture_common_error(SnapshotV2DeviceGraphCaptureError::Allocation),
        SnapshotV2EntropyStateCaptureError::Allocation
    );
    assert_eq!(
        capture_build_error(SnapshotV2EntropyStateBuildError::Retry),
        SnapshotV2EntropyStateCaptureError::Retry
    );
    assert_eq!(
        format!("{:?}", SnapshotV2EntropyStateCaptureError::Allocation),
        "native-v2 captured entropy state allocation failed"
    );
}

fn replace_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn replace_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn replace_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
