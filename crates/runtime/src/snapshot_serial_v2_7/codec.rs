use std::str;

use super::*;
use crate::serial::{SerialMmioCaptureStateParts, SerialMmioState};

const MAGIC: [u8; 8] = *b"BANGSR2\0";
const PROFILE: u16 = 1;
const FLAGS: u32 = 0;
const ENDPOINT_DEFAULT: u8 = 0;
const ENDPOINT_CONFIGURED: u8 = 1;

const MAGIC_OFFSET: usize = 0;
const HEADER_BYTES_OFFSET: usize = 8;
const PROFILE_OFFSET: usize = 10;
const FLAGS_OFFSET: usize = 12;
const ENDPOINT_OFFSET: usize = 16;
const RATE_LIMITER_PRESENT_OFFSET: usize = 17;
const BURST_PRESENT_OFFSET: usize = 18;
const RECEIVE_INTERRUPT_INTENT_OFFSET: usize = 19;
const INPUT_READY_INTENT_OFFSET: usize = 20;
const RESERVED_A_OFFSET: usize = 21;
const RESERVED_A_BYTES: usize = 3;
const TOTAL_LENGTH_OFFSET: usize = 24;
const SELECTOR_LENGTH_OFFSET: usize = 32;
const RECEIVE_LENGTH_OFFSET: usize = 36;
const RESERVED_B_OFFSET: usize = 38;
const RATE_LIMITER_SIZE_OFFSET: usize = 40;
const RATE_LIMITER_BURST_OFFSET: usize = 48;
const RATE_LIMITER_REFILL_OFFSET: usize = 56;
const DIVISOR_LATCH_LOW_OFFSET: usize = 64;
const DIVISOR_LATCH_HIGH_OFFSET: usize = 65;
const INTERRUPT_ENABLE_OFFSET: usize = 66;
const INTERRUPT_IDENTIFICATION_OFFSET: usize = 67;
const LINE_CONTROL_OFFSET: usize = 68;
const LINE_STATUS_OFFSET: usize = 69;
const MODEM_CONTROL_OFFSET: usize = 70;
const MODEM_STATUS_OFFSET: usize = 71;
const SCRATCH_OFFSET: usize = 72;
const RESERVED_C_OFFSET: usize = 73;
const RESERVED_C_BYTES: usize = 7;

pub(super) trait ReservePolicy {
    fn reserve_vec<T>(
        &mut self,
        values: &mut Vec<T>,
        additional: usize,
    ) -> Result<(), TryReserveError>;

    fn reserve_string(
        &mut self,
        value: &mut String,
        additional: usize,
    ) -> Result<(), TryReserveError>;
}

struct FallibleReserve;

impl ReservePolicy for FallibleReserve {
    fn reserve_vec<T>(
        &mut self,
        values: &mut Vec<T>,
        additional: usize,
    ) -> Result<(), TryReserveError> {
        values.try_reserve_exact(additional)
    }

    fn reserve_string(
        &mut self,
        value: &mut String,
        additional: usize,
    ) -> Result<(), TryReserveError> {
        value.try_reserve_exact(additional)
    }
}

pub(super) fn encode(
    version: SnapshotFormatVersion,
    state: &SnapshotV2SerialState,
) -> Result<Vec<u8>, SnapshotV2SerialStateEncodeError> {
    encode_with_policy(version, state, &mut FallibleReserve)
}

pub(super) fn decode(
    version: SnapshotFormatVersion,
    bytes: &[u8],
) -> Result<SnapshotV2SerialState, SnapshotV2SerialStateDecodeError> {
    decode_with_policy(version, bytes, &mut FallibleReserve)
}

pub(super) fn encode_with_policy<R: ReservePolicy>(
    version: SnapshotFormatVersion,
    state: &SnapshotV2SerialState,
    reserve: &mut R,
) -> Result<Vec<u8>, SnapshotV2SerialStateEncodeError> {
    if version != NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION {
        return Err(SnapshotV2SerialStateEncodeError::UnsupportedVersion);
    }
    validate_endpoint_intent(state.endpoint_intent())
        .map_err(SnapshotV2SerialStateEncodeError::InvalidState)?;

    let selector = state
        .endpoint_intent()
        .configured_selector()
        .unwrap_or_default()
        .as_bytes();
    let receive = state.device().receive_bytes();
    let total_length = NATIVE_V2_SERIAL_STATE_HEADER_BYTES
        .checked_add(selector.len())
        .and_then(|value| value.checked_add(receive.len()))
        .ok_or(SnapshotV2SerialStateEncodeError::LengthOverflow)?;
    if total_length > NATIVE_V2_SERIAL_STATE_MAX_BYTES {
        return Err(SnapshotV2SerialStateEncodeError::TooLarge);
    }

    let mut output = Vec::new();
    reserve
        .reserve_vec(&mut output, total_length)
        .map_err(SnapshotV2SerialStateEncodeError::Allocation)?;
    output.extend_from_slice(&MAGIC);
    push_u16(&mut output, NATIVE_V2_SERIAL_STATE_HEADER_BYTES as u16);
    push_u16(&mut output, PROFILE);
    push_u32(&mut output, FLAGS);
    output.push(if state.endpoint_intent().is_default_process_stdio() {
        ENDPOINT_DEFAULT
    } else {
        ENDPOINT_CONFIGURED
    });
    output.push(u8::from(state.rate_limiter().is_some()));
    output.push(u8::from(
        state
            .rate_limiter()
            .and_then(SerialRateLimiterConfig::one_time_burst)
            .is_some(),
    ));
    output.push(u8::from(state.device().receive_interrupt_intent_pending()));
    output.push(u8::from(state.device().input_ready_intent_pending()));
    output.extend_from_slice(&[0; RESERVED_A_BYTES]);
    push_u64(
        &mut output,
        u64::try_from(total_length)
            .map_err(|_| SnapshotV2SerialStateEncodeError::LengthOverflow)?,
    );
    push_u32(
        &mut output,
        u32::try_from(selector.len())
            .map_err(|_| SnapshotV2SerialStateEncodeError::LengthOverflow)?,
    );
    push_u16(
        &mut output,
        u16::try_from(receive.len())
            .map_err(|_| SnapshotV2SerialStateEncodeError::LengthOverflow)?,
    );
    push_u16(&mut output, 0);
    let rate_limiter = state.rate_limiter();
    push_u64(
        &mut output,
        rate_limiter.map_or(0, SerialRateLimiterConfig::size),
    );
    push_u64(
        &mut output,
        rate_limiter
            .and_then(SerialRateLimiterConfig::one_time_burst)
            .unwrap_or(0),
    );
    push_u64(
        &mut output,
        rate_limiter.map_or(0, SerialRateLimiterConfig::refill_time),
    );
    let legacy = state.device().legacy_state();
    output.extend_from_slice(&[
        legacy.divisor_latch_low(),
        legacy.divisor_latch_high(),
        legacy.interrupt_enable(),
        state.device().interrupt_identification(),
        legacy.line_control(),
        state.device().line_status(),
        legacy.modem_control(),
        state.device().modem_status(),
        legacy.scratch(),
    ]);
    output.extend_from_slice(&[0; RESERVED_C_BYTES]);
    debug_assert_eq!(output.len(), NATIVE_V2_SERIAL_STATE_HEADER_BYTES);
    output.extend_from_slice(selector);
    output.extend_from_slice(receive);
    debug_assert_eq!(output.len(), total_length);
    Ok(output)
}

pub(super) fn decode_with_policy<R: ReservePolicy>(
    version: SnapshotFormatVersion,
    bytes: &[u8],
    reserve: &mut R,
) -> Result<SnapshotV2SerialState, SnapshotV2SerialStateDecodeError> {
    if version != NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION {
        return Err(SnapshotV2SerialStateDecodeError::UnsupportedVersion);
    }
    if bytes.len() < NATIVE_V2_SERIAL_STATE_HEADER_BYTES {
        return Err(SnapshotV2SerialStateDecodeError::Truncated);
    }
    if bytes.get(MAGIC_OFFSET..MAGIC_OFFSET + MAGIC.len()) != Some(MAGIC.as_slice())
        || read_u16(bytes, HEADER_BYTES_OFFSET)? as usize != NATIVE_V2_SERIAL_STATE_HEADER_BYTES
        || read_u16(bytes, PROFILE_OFFSET)? != PROFILE
        || read_u32(bytes, FLAGS_OFFSET)? != FLAGS
        || !all_zero(bytes, RESERVED_A_OFFSET, RESERVED_A_BYTES)
        || read_u16(bytes, RESERVED_B_OFFSET)? != 0
        || !all_zero(bytes, RESERVED_C_OFFSET, RESERVED_C_BYTES)
    {
        return Err(SnapshotV2SerialStateDecodeError::InvalidHeader);
    }

    let total_length = usize::try_from(read_u64(bytes, TOTAL_LENGTH_OFFSET)?)
        .map_err(|_| SnapshotV2SerialStateDecodeError::LengthOverflow)?;
    if total_length > NATIVE_V2_SERIAL_STATE_MAX_BYTES {
        return Err(SnapshotV2SerialStateDecodeError::TooLarge);
    }
    if total_length != bytes.len() {
        return Err(SnapshotV2SerialStateDecodeError::LengthMismatch);
    }
    let selector_length = usize::try_from(read_u32(bytes, SELECTOR_LENGTH_OFFSET)?)
        .map_err(|_| SnapshotV2SerialStateDecodeError::LengthOverflow)?;
    let receive_length = usize::from(read_u16(bytes, RECEIVE_LENGTH_OFFSET)?);
    if selector_length > NATIVE_V2_SERIAL_STATE_MAX_SELECTOR_BYTES
        || receive_length > SERIAL_RECEIVE_FIFO_CAPACITY
    {
        return Err(SnapshotV2SerialStateDecodeError::TooLarge);
    }
    let expected_length = NATIVE_V2_SERIAL_STATE_HEADER_BYTES
        .checked_add(selector_length)
        .and_then(|value| value.checked_add(receive_length))
        .ok_or(SnapshotV2SerialStateDecodeError::LengthOverflow)?;
    if expected_length != total_length {
        return Err(SnapshotV2SerialStateDecodeError::LengthMismatch);
    }
    let selector_end = NATIVE_V2_SERIAL_STATE_HEADER_BYTES
        .checked_add(selector_length)
        .ok_or(SnapshotV2SerialStateDecodeError::LengthOverflow)?;
    let selector_bytes = bytes
        .get(NATIVE_V2_SERIAL_STATE_HEADER_BYTES..selector_end)
        .ok_or(SnapshotV2SerialStateDecodeError::LengthMismatch)?;
    let receive_bytes = bytes
        .get(selector_end..expected_length)
        .ok_or(SnapshotV2SerialStateDecodeError::LengthMismatch)?;

    let selector = match read_u8(bytes, ENDPOINT_OFFSET)? {
        ENDPOINT_DEFAULT if selector_bytes.is_empty() => None,
        ENDPOINT_CONFIGURED if !selector_bytes.is_empty() => {
            let selector = str::from_utf8(selector_bytes)
                .map_err(|_| SnapshotV2SerialStateDecodeError::InvalidEndpointIntent)?;
            if selector.chars().any(char::is_control) {
                return Err(SnapshotV2SerialStateDecodeError::InvalidEndpointIntent);
            }
            Some(selector)
        }
        _ => return Err(SnapshotV2SerialStateDecodeError::InvalidEndpointIntent),
    };

    let rate_present = read_bool(bytes, RATE_LIMITER_PRESENT_OFFSET)?;
    let burst_present = read_bool(bytes, BURST_PRESENT_OFFSET)?;
    let size = read_u64(bytes, RATE_LIMITER_SIZE_OFFSET)?;
    let burst = read_u64(bytes, RATE_LIMITER_BURST_OFFSET)?;
    let refill = read_u64(bytes, RATE_LIMITER_REFILL_OFFSET)?;
    let rate_limiter = match (rate_present, burst_present) {
        (false, false) if size == 0 && burst == 0 && refill == 0 => None,
        (true, false) if burst == 0 => Some(SerialRateLimiterConfig::new(size, None, refill)),
        (true, true) => Some(SerialRateLimiterConfig::new(size, Some(burst), refill)),
        _ => return Err(SnapshotV2SerialStateDecodeError::InvalidRateLimiter),
    };

    let receive_interrupt_intent_pending = read_bool(bytes, RECEIVE_INTERRUPT_INTENT_OFFSET)?;
    let input_ready_intent_pending = read_bool(bytes, INPUT_READY_INTENT_OFFSET)?;
    let legacy_state = SerialMmioState::new(
        read_u8(bytes, INTERRUPT_ENABLE_OFFSET)?,
        read_u8(bytes, LINE_CONTROL_OFFSET)?,
        read_u8(bytes, MODEM_CONTROL_OFFSET)?,
        read_u8(bytes, SCRATCH_OFFSET)?,
        read_u8(bytes, DIVISOR_LATCH_LOW_OFFSET)?,
        read_u8(bytes, DIVISOR_LATCH_HIGH_OFFSET)?,
    );
    let interrupt_identification = read_u8(bytes, INTERRUPT_IDENTIFICATION_OFFSET)?;
    let line_status = read_u8(bytes, LINE_STATUS_OFFSET)?;
    let modem_status = read_u8(bytes, MODEM_STATUS_OFFSET)?;

    let endpoint_intent = if let Some(selector) = selector {
        let mut owned = String::new();
        reserve
            .reserve_string(&mut owned, selector.len())
            .map_err(SnapshotV2SerialStateDecodeError::Allocation)?;
        owned.push_str(selector);
        SnapshotV2SerialEndpointIntent::ConfiguredOutput { selector: owned }
    } else {
        SnapshotV2SerialEndpointIntent::DefaultProcessStdio
    };
    let mut receive = Vec::new();
    reserve
        .reserve_vec(&mut receive, receive_bytes.len())
        .map_err(SnapshotV2SerialStateDecodeError::Allocation)?;
    receive.extend_from_slice(receive_bytes);
    let device = SerialMmioCaptureState::try_from_parts(SerialMmioCaptureStateParts {
        legacy_state,
        interrupt_identification,
        line_status,
        modem_status,
        receive_bytes: receive,
        receive_interrupt_intent_pending,
        input_ready_intent_pending,
    })
    .map_err(SnapshotV2SerialStateDecodeError::InvalidDeviceState)?;

    SnapshotV2SerialState::try_new(endpoint_intent, rate_limiter, device)
        .map_err(|_| SnapshotV2SerialStateDecodeError::InvalidEndpointIntent)
}

fn read_u8(bytes: &[u8], offset: usize) -> Result<u8, SnapshotV2SerialStateDecodeError> {
    bytes
        .get(offset)
        .copied()
        .ok_or(SnapshotV2SerialStateDecodeError::Truncated)
}

fn read_bool(bytes: &[u8], offset: usize) -> Result<bool, SnapshotV2SerialStateDecodeError> {
    match read_u8(bytes, offset)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(SnapshotV2SerialStateDecodeError::InvalidHeader),
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, SnapshotV2SerialStateDecodeError> {
    read_array(bytes, offset).map(u16::from_le_bytes)
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, SnapshotV2SerialStateDecodeError> {
    read_array(bytes, offset).map(u32::from_le_bytes)
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, SnapshotV2SerialStateDecodeError> {
    read_array(bytes, offset).map(u64::from_le_bytes)
}

fn read_array<const N: usize>(
    bytes: &[u8],
    offset: usize,
) -> Result<[u8; N], SnapshotV2SerialStateDecodeError> {
    bytes
        .get(
            offset
                ..offset
                    .checked_add(N)
                    .ok_or(SnapshotV2SerialStateDecodeError::LengthOverflow)?,
        )
        .ok_or(SnapshotV2SerialStateDecodeError::Truncated)?
        .try_into()
        .map_err(|_| SnapshotV2SerialStateDecodeError::Truncated)
}

fn all_zero(bytes: &[u8], offset: usize, length: usize) -> bool {
    bytes
        .get(offset..offset.saturating_add(length))
        .is_some_and(|reserved| reserved.iter().all(|byte| *byte == 0))
}

fn push_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}
