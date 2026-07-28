use super::codec::{self, ReservePolicy};
use super::*;

use crate::serial::{
    CaptureReadySerialState, SERIAL_INTERRUPT_IDENTIFICATION_NO_INTERRUPT_PENDING,
    SERIAL_INTERRUPT_IDENTIFICATION_RECEIVED_DATA_AVAILABLE, SERIAL_LINE_STATUS_DATA_READY,
    SERIAL_LINE_STATUS_DEFAULT, SERIAL_LINE_STATUS_OVERRUN_ERROR, SerialConfig, SerialConfigInput,
    SerialMmioCaptureStateParts, SerialMmioState,
};

const DEFAULT_HEX: &str = include_str!("fixtures/default.hex");
const CONFIGURED_HEX: &str = include_str!("fixtures/configured.hex");

const ENDPOINT_OFFSET: usize = 16;
const RATE_LIMITER_PRESENT_OFFSET: usize = 17;
const BURST_PRESENT_OFFSET: usize = 18;
const RECEIVE_INTERRUPT_INTENT_OFFSET: usize = 19;
const INPUT_READY_INTENT_OFFSET: usize = 20;
const RESERVED_A_OFFSET: usize = 21;
const TOTAL_LENGTH_OFFSET: usize = 24;
const SELECTOR_LENGTH_OFFSET: usize = 32;
const RECEIVE_LENGTH_OFFSET: usize = 36;
const RESERVED_B_OFFSET: usize = 38;
const RATE_LIMITER_SIZE_OFFSET: usize = 40;
const RATE_LIMITER_BURST_OFFSET: usize = 48;
const RATE_LIMITER_REFILL_OFFSET: usize = 56;
const INTERRUPT_IDENTIFICATION_OFFSET: usize = 67;
const LINE_STATUS_OFFSET: usize = 69;
const MODEM_STATUS_OFFSET: usize = 71;
const RESERVED_C_OFFSET: usize = 73;

fn default_state() -> SnapshotV2SerialState {
    let device = SerialMmioCaptureState::try_from_parts(SerialMmioCaptureStateParts {
        legacy_state: SerialMmioState::new(0, 3, 0, 0, 12, 0),
        interrupt_identification: SERIAL_INTERRUPT_IDENTIFICATION_NO_INTERRUPT_PENDING,
        line_status: SERIAL_LINE_STATUS_DEFAULT,
        modem_status: 0,
        receive_bytes: Vec::new(),
        receive_interrupt_intent_pending: false,
        input_ready_intent_pending: true,
    })
    .expect("default fixture device should validate");
    SnapshotV2SerialState::try_new(
        SnapshotV2SerialEndpointIntent::default_process_stdio(),
        None,
        device,
    )
    .expect("default fixture should validate")
}

fn configured_state() -> SnapshotV2SerialState {
    let device = SerialMmioCaptureState::try_from_parts(SerialMmioCaptureStateParts {
        legacy_state: SerialMmioState::new(1, 3, 8, 0x5a, 12, 0),
        interrupt_identification: SERIAL_INTERRUPT_IDENTIFICATION_RECEIVED_DATA_AVAILABLE,
        line_status: SERIAL_LINE_STATUS_DEFAULT
            | SERIAL_LINE_STATUS_DATA_READY
            | SERIAL_LINE_STATUS_OVERRUN_ERROR,
        modem_status: 0,
        receive_bytes: b"abc".to_vec(),
        receive_interrupt_intent_pending: true,
        input_ready_intent_pending: false,
    })
    .expect("configured fixture device should validate");
    SnapshotV2SerialState::try_new(
        SnapshotV2SerialEndpointIntent::try_configured_output("serial-log")
            .expect("fixture selector should validate"),
        Some(SerialRateLimiterConfig::new(1024, Some(128), 1000)),
        device,
    )
    .expect("configured fixture should validate")
}

fn encode(state: &SnapshotV2SerialState) -> Vec<u8> {
    state
        .encode(NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION)
        .expect("fixture state should encode")
}

fn decode_hex(value: &str) -> Vec<u8> {
    let value = value.trim();
    assert!(value.len().is_multiple_of(2));
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            u8::from_str_radix(
                std::str::from_utf8(pair).expect("hex pair should be UTF-8"),
                16,
            )
            .expect("fixture should be hexadecimal")
        })
        .collect()
}

fn encode_hex(value: &[u8]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn configured_capture(device: SerialMmioCaptureState) -> CaptureReadySerialState {
    let config = SerialConfigInput::new()
        .with_serial_out_path("serial-log")
        .with_rate_limiter(SerialRateLimiterConfig::new(1024, Some(128), 1000))
        .validate()
        .expect("configured capture input should validate");
    CaptureReadySerialState::new(config, device)
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + size_of::<u16>()].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + size_of::<u32>()].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + size_of::<u64>()].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn canonical_default_and_configured_fixtures_round_trip() {
    for (state, fixture) in [
        (default_state(), DEFAULT_HEX),
        (configured_state(), CONFIGURED_HEX),
    ] {
        let encoded = encode(&state);
        assert_eq!(encode_hex(&encoded), fixture.trim());
        assert_eq!(
            SnapshotV2SerialState::decode(
                NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
                &decode_hex(fixture),
            )
            .expect("canonical fixture should decode"),
            state
        );
    }
}

#[test]
fn exact_version_and_endpoint_profile_fail_closed() {
    let state = default_state();
    for version in [
        SnapshotFormatVersion::new(2, 6, 0),
        SnapshotFormatVersion::new(2, 8, 0),
        SnapshotFormatVersion::new(3, 7, 0),
    ] {
        assert!(matches!(
            state.encode(version),
            Err(SnapshotV2SerialStateEncodeError::UnsupportedVersion)
        ));
        assert!(matches!(
            SnapshotV2SerialState::decode(version, &encode(&state)),
            Err(SnapshotV2SerialStateDecodeError::UnsupportedVersion)
        ));
    }

    for selector in ["", "\n", "serial\u{7f}"] {
        assert_eq!(
            SnapshotV2SerialEndpointIntent::try_configured_output(selector),
            Err(SnapshotV2SerialStateBuildError::InvalidEndpointIntent)
        );
    }
    assert!(
        SnapshotV2SerialEndpointIntent::try_configured_output(
            "x".repeat(NATIVE_V2_SERIAL_STATE_MAX_SELECTOR_BYTES)
        )
        .is_ok()
    );
    assert_eq!(
        SnapshotV2SerialEndpointIntent::try_configured_output(
            "x".repeat(NATIVE_V2_SERIAL_STATE_MAX_SELECTOR_BYTES + 1)
        ),
        Err(SnapshotV2SerialStateBuildError::InvalidEndpointIntent)
    );
}

#[test]
fn header_tags_reserved_fields_and_lengths_are_rejected() {
    let canonical = encode(&configured_state());
    let invalid_header_mutations: &[fn(&mut Vec<u8>)] = &[
        |bytes| bytes[0] ^= 0xff,
        |bytes| put_u16(bytes, 8, 79),
        |bytes| put_u16(bytes, 10, 2),
        |bytes| put_u32(bytes, 12, 1),
        |bytes| bytes[RESERVED_A_OFFSET] = 1,
        |bytes| put_u16(bytes, RESERVED_B_OFFSET, 1),
        |bytes| bytes[RESERVED_C_OFFSET] = 1,
        |bytes| bytes[RATE_LIMITER_PRESENT_OFFSET] = 2,
        |bytes| bytes[BURST_PRESENT_OFFSET] = 2,
        |bytes| bytes[RECEIVE_INTERRUPT_INTENT_OFFSET] = 2,
        |bytes| bytes[INPUT_READY_INTENT_OFFSET] = 2,
    ];
    for mutate in invalid_header_mutations {
        let mut invalid = canonical.clone();
        mutate(&mut invalid);
        assert!(matches!(
            SnapshotV2SerialState::decode(NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION, &invalid,),
            Err(SnapshotV2SerialStateDecodeError::InvalidHeader)
        ));
    }

    let mut unknown_endpoint = canonical.clone();
    unknown_endpoint[ENDPOINT_OFFSET] = 2;
    assert!(matches!(
        SnapshotV2SerialState::decode(
            NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
            &unknown_endpoint,
        ),
        Err(SnapshotV2SerialStateDecodeError::InvalidEndpointIntent)
    ));

    let mut default_with_selector = canonical.clone();
    default_with_selector[ENDPOINT_OFFSET] = 0;
    assert!(matches!(
        SnapshotV2SerialState::decode(
            NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
            &default_with_selector,
        ),
        Err(SnapshotV2SerialStateDecodeError::InvalidEndpointIntent)
    ));

    let default = encode(&default_state());
    let mut configured_without_selector = default.clone();
    configured_without_selector[ENDPOINT_OFFSET] = 1;
    assert!(matches!(
        SnapshotV2SerialState::decode(
            NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
            &configured_without_selector,
        ),
        Err(SnapshotV2SerialStateDecodeError::InvalidEndpointIntent)
    ));

    let mut invalid_utf8 = canonical.clone();
    invalid_utf8[NATIVE_V2_SERIAL_STATE_HEADER_BYTES] = 0xff;
    assert!(matches!(
        SnapshotV2SerialState::decode(NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION, &invalid_utf8,),
        Err(SnapshotV2SerialStateDecodeError::InvalidEndpointIntent)
    ));

    let mut control_selector = canonical.clone();
    control_selector[NATIVE_V2_SERIAL_STATE_HEADER_BYTES] = b'\n';
    assert!(matches!(
        SnapshotV2SerialState::decode(
            NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
            &control_selector,
        ),
        Err(SnapshotV2SerialStateDecodeError::InvalidEndpointIntent)
    ));

    let mut wrong_total = canonical.clone();
    put_u64(
        &mut wrong_total,
        TOTAL_LENGTH_OFFSET,
        u64::try_from(canonical.len() - 1).expect("fixture length should fit"),
    );
    assert!(matches!(
        SnapshotV2SerialState::decode(NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION, &wrong_total,),
        Err(SnapshotV2SerialStateDecodeError::LengthMismatch)
    ));

    let mut oversized_total = canonical.clone();
    put_u64(
        &mut oversized_total,
        TOTAL_LENGTH_OFFSET,
        u64::try_from(NATIVE_V2_SERIAL_STATE_MAX_BYTES + 1).expect("profile maximum should fit"),
    );
    assert!(matches!(
        SnapshotV2SerialState::decode(
            NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
            &oversized_total,
        ),
        Err(SnapshotV2SerialStateDecodeError::TooLarge)
    ));

    let mut oversized_selector = canonical.clone();
    put_u32(
        &mut oversized_selector,
        SELECTOR_LENGTH_OFFSET,
        u32::try_from(NATIVE_V2_SERIAL_STATE_MAX_SELECTOR_BYTES + 1)
            .expect("selector maximum should fit"),
    );
    assert!(matches!(
        SnapshotV2SerialState::decode(
            NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
            &oversized_selector,
        ),
        Err(SnapshotV2SerialStateDecodeError::TooLarge)
    ));

    let mut oversized_receive = canonical.clone();
    put_u16(
        &mut oversized_receive,
        RECEIVE_LENGTH_OFFSET,
        u16::try_from(SERIAL_RECEIVE_FIFO_CAPACITY + 1).expect("receive maximum should fit"),
    );
    assert!(matches!(
        SnapshotV2SerialState::decode(
            NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
            &oversized_receive,
        ),
        Err(SnapshotV2SerialStateDecodeError::TooLarge)
    ));

    assert!(matches!(
        SnapshotV2SerialState::decode(
            NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
            &canonical[..NATIVE_V2_SERIAL_STATE_HEADER_BYTES - 1],
        ),
        Err(SnapshotV2SerialStateDecodeError::Truncated)
    ));
    let mut trailing = canonical;
    trailing.push(0);
    assert!(matches!(
        SnapshotV2SerialState::decode(NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION, &trailing,),
        Err(SnapshotV2SerialStateDecodeError::LengthMismatch)
    ));
}

#[test]
fn limiter_presence_and_complete_uart_semantics_are_rejected_when_inconsistent() {
    let canonical = encode(&configured_state());
    let mut absent_with_values = canonical.clone();
    absent_with_values[RATE_LIMITER_PRESENT_OFFSET] = 0;
    absent_with_values[BURST_PRESENT_OFFSET] = 0;
    assert!(matches!(
        SnapshotV2SerialState::decode(
            NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
            &absent_with_values,
        ),
        Err(SnapshotV2SerialStateDecodeError::InvalidRateLimiter)
    ));

    let mut burst_without_rate = canonical.clone();
    burst_without_rate[RATE_LIMITER_PRESENT_OFFSET] = 0;
    assert!(matches!(
        SnapshotV2SerialState::decode(
            NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
            &burst_without_rate,
        ),
        Err(SnapshotV2SerialStateDecodeError::InvalidRateLimiter)
    ));

    let mut no_burst_with_value = canonical.clone();
    no_burst_with_value[BURST_PRESENT_OFFSET] = 0;
    assert!(matches!(
        SnapshotV2SerialState::decode(
            NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
            &no_burst_with_value,
        ),
        Err(SnapshotV2SerialStateDecodeError::InvalidRateLimiter)
    ));

    let mut zero_config_is_preserved = canonical.clone();
    put_u64(&mut zero_config_is_preserved, RATE_LIMITER_SIZE_OFFSET, 0);
    put_u64(&mut zero_config_is_preserved, RATE_LIMITER_BURST_OFFSET, 0);
    put_u64(&mut zero_config_is_preserved, RATE_LIMITER_REFILL_OFFSET, 0);
    assert_eq!(
        SnapshotV2SerialState::decode(
            NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
            &zero_config_is_preserved,
        )
        .expect("present all-zero rate configuration should remain representable")
        .rate_limiter(),
        Some(SerialRateLimiterConfig::new(0, Some(0), 0))
    );

    for mutate in [
        |bytes: &mut Vec<u8>| bytes[INTERRUPT_IDENTIFICATION_OFFSET] = 0xff,
        |bytes: &mut Vec<u8>| bytes[LINE_STATUS_OFFSET] = 0,
        |bytes: &mut Vec<u8>| bytes[MODEM_STATUS_OFFSET] = 1,
        |bytes: &mut Vec<u8>| bytes[LINE_STATUS_OFFSET] &= !SERIAL_LINE_STATUS_DATA_READY,
        |bytes: &mut Vec<u8>| bytes[INPUT_READY_INTENT_OFFSET] = 1,
    ] {
        let mut invalid = canonical.clone();
        mutate(&mut invalid);
        assert!(matches!(
            SnapshotV2SerialState::decode(NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION, &invalid,),
            Err(SnapshotV2SerialStateDecodeError::InvalidDeviceState(_))
        ));
    }

    let mut receive_intent_without_interrupt = encode(&default_state());
    receive_intent_without_interrupt[RECEIVE_INTERRUPT_INTENT_OFFSET] = 1;
    assert!(matches!(
        SnapshotV2SerialState::decode(
            NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
            &receive_intent_without_interrupt,
        ),
        Err(SnapshotV2SerialStateDecodeError::InvalidDeviceState(_))
    ));
}

struct FailingReserve {
    fail_at: usize,
    calls: usize,
}

impl FailingReserve {
    const fn new(fail_at: usize) -> Self {
        Self { fail_at, calls: 0 }
    }

    fn reserve_or_fail(&mut self) -> Result<(), TryReserveError> {
        let call = self.calls;
        self.calls += 1;
        if call == self.fail_at {
            let mut impossible = Vec::<u8>::new();
            Err(impossible
                .try_reserve(usize::MAX)
                .expect_err("impossible reservation should fail"))
        } else {
            Ok(())
        }
    }
}

impl ReservePolicy for FailingReserve {
    fn reserve_vec<T>(
        &mut self,
        values: &mut Vec<T>,
        additional: usize,
    ) -> Result<(), TryReserveError> {
        self.reserve_or_fail()?;
        values.try_reserve_exact(additional)
    }

    fn reserve_string(
        &mut self,
        value: &mut String,
        additional: usize,
    ) -> Result<(), TryReserveError> {
        self.reserve_or_fail()?;
        value.try_reserve_exact(additional)
    }
}

impl CaptureReservePolicy for FailingReserve {
    fn reserve_string(
        &mut self,
        value: &mut String,
        additional: usize,
    ) -> Result<(), TryReserveError> {
        self.reserve_or_fail()?;
        value.try_reserve_exact(additional)
    }
}

#[test]
fn capture_ready_conversion_preserves_endpoint_limiter_and_complete_uart() {
    let default = default_state();
    let default_captured =
        CaptureReadySerialState::new(SerialConfig::default(), default.device().clone());
    assert_eq!(
        SnapshotV2SerialState::try_from_capture_ready(default_captured)
            .expect("default capture should convert"),
        default
    );

    let configured = configured_state();
    let configured_captured = configured_capture(configured.device().clone());
    let converted = SnapshotV2SerialState::try_from_capture_ready(configured_captured)
        .expect("configured capture should convert");
    assert_eq!(converted, configured);
    assert!(converted.device().receive_interrupt_intent_pending());
    assert!(!converted.device().input_ready_intent_pending());
    assert_eq!(converted.device().receive_bytes(), b"abc");
}

#[test]
fn capture_resource_preflight_accounts_for_the_future_configured_sink() {
    let configured = SerialConfigInput::new()
        .with_serial_out_path("serial-log")
        .validate()
        .expect("configured input should validate");

    SnapshotV2SerialState::preflight_capture(
        &SerialConfig::default(),
        MAX_SNAPSHOT_RESTORE_RESOURCES,
    )
    .expect("default serial should consume no restore resource");
    assert!(matches!(
        SnapshotV2SerialState::preflight_capture(
            &SerialConfig::default(),
            MAX_SNAPSHOT_RESTORE_RESOURCES + 1,
        ),
        Err(SnapshotV2SerialStateCaptureError::RestoreResourceCapacity)
    ));
    SnapshotV2SerialState::preflight_capture(&configured, MAX_SNAPSHOT_RESTORE_RESOURCES - 1)
        .expect("configured serial plus 63 storage resources should fit");
    assert!(matches!(
        SnapshotV2SerialState::preflight_capture(&configured, MAX_SNAPSHOT_RESTORE_RESOURCES,),
        Err(SnapshotV2SerialStateCaptureError::RestoreResourceCapacity)
    ));

    let oversized = SerialConfigInput::new()
        .with_serial_out_path("x".repeat(NATIVE_V2_SERIAL_STATE_MAX_SELECTOR_BYTES + 1))
        .validate()
        .expect("live configuration has no snapshot-specific byte bound");
    assert!(matches!(
        SnapshotV2SerialState::preflight_capture(&oversized, 0),
        Err(SnapshotV2SerialStateCaptureError::InvalidEndpointIntent)
    ));
}

#[test]
fn capture_selector_allocation_is_fallible_and_redacted() {
    let configured = configured_state();
    assert!(matches!(
        SnapshotV2SerialState::try_from_capture_ready_with_policy(
            configured_capture(configured.device().clone()),
            &mut FailingReserve::new(0),
        ),
        Err(SnapshotV2SerialStateCaptureError::Allocation(_))
    ));

    let error =
        SnapshotV2SerialState::preflight_capture(
            &SerialConfigInput::new()
                .with_serial_out_path("sensitive-selector".repeat(
                    NATIVE_V2_SERIAL_STATE_MAX_SELECTOR_BYTES / "sensitive-selector".len() + 1,
                ))
                .validate()
                .expect("live configuration should validate"),
            0,
        )
        .expect_err("oversized selector should fail preflight");
    let display = error.to_string();
    let debug = format!("{error:?}");
    assert!(!display.contains("sensitive-selector"));
    assert!(!debug.contains("sensitive-selector"));
}

#[test]
fn every_owned_decode_and_encode_allocation_is_fallible() {
    let state = configured_state();
    assert!(matches!(
        codec::encode_with_policy(
            NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
            &state,
            &mut FailingReserve::new(0),
        ),
        Err(SnapshotV2SerialStateEncodeError::Allocation(_))
    ));

    let bytes = encode(&state);
    for fail_at in 0..2 {
        assert!(matches!(
            codec::decode_with_policy(
                NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
                &bytes,
                &mut FailingReserve::new(fail_at),
            ),
            Err(SnapshotV2SerialStateDecodeError::Allocation(_))
        ));
    }
    assert_eq!(
        codec::decode_with_policy(
            NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
            &bytes,
            &mut FailingReserve::new(2),
        )
        .expect("decode should succeed after both allocations"),
        state
    );

    for mutate in [
        |invalid: &mut Vec<u8>| invalid[0] ^= 0xff,
        |invalid: &mut Vec<u8>| invalid[RATE_LIMITER_PRESENT_OFFSET] = 2,
        |invalid: &mut Vec<u8>| invalid[INPUT_READY_INTENT_OFFSET] = 2,
    ] {
        let mut invalid = bytes.clone();
        mutate(&mut invalid);
        let mut reserve = FailingReserve::new(0);
        assert!(matches!(
            codec::decode_with_policy(
                NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
                &invalid,
                &mut reserve,
            ),
            Err(SnapshotV2SerialStateDecodeError::InvalidHeader)
        ));
        assert_eq!(reserve.calls, 0);
    }
}

#[test]
fn debug_output_redacts_endpoint_and_device_values() {
    let state = configured_state();
    let debug = format!("{state:?} {:?}", state.endpoint_intent());
    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("serial-log"));
    assert!(!debug.contains("abc"));
}
