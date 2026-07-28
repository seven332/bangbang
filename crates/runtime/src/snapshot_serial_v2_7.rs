//! Canonical detached native-v2 2.7 serial state profile.
//!
//! This module defines only inert configuration and guest-visible UART state.
//! Live descriptors, terminal ownership, pipe contents, metrics, wakeups, and
//! grant authority remain destination-local.

use std::collections::TryReserveError;
use std::fmt;

use crate::serial::{
    CaptureReadySerialState, SERIAL_RECEIVE_FIFO_CAPACITY, SerialConfig, SerialMmioCaptureState,
    SerialMmioCaptureStateError, SerialRateLimiterConfig,
};
use crate::snapshot_format::SnapshotFormatVersion;
use crate::snapshot_restore::MAX_SNAPSHOT_RESTORE_RESOURCES;

mod codec;

#[cfg(test)]
mod tests;

/// Exact compatibility context of the singleton serial component.
pub const NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION: SnapshotFormatVersion =
    SnapshotFormatVersion::new(2, 7, 0);

/// Maximum UTF-8 byte length of one inert configured-output selector.
pub const NATIVE_V2_SERIAL_STATE_MAX_SELECTOR_BYTES: usize = 4096;

/// Fixed exact-2.7 serial component header size.
pub const NATIVE_V2_SERIAL_STATE_HEADER_BYTES: usize = 80;

/// Maximum complete exact-2.7 serial component size.
pub const NATIVE_V2_SERIAL_STATE_MAX_BYTES: usize = NATIVE_V2_SERIAL_STATE_HEADER_BYTES
    + NATIVE_V2_SERIAL_STATE_MAX_SELECTOR_BYTES
    + SERIAL_RECEIVE_FIFO_CAPACITY;

const REDACTED: &str = "<redacted>";

trait CaptureReservePolicy {
    fn reserve_string(
        &mut self,
        value: &mut String,
        additional: usize,
    ) -> Result<(), TryReserveError>;
}

struct FallibleCaptureReserve;

impl CaptureReservePolicy for FallibleCaptureReserve {
    fn reserve_string(
        &mut self,
        value: &mut String,
        additional: usize,
    ) -> Result<(), TryReserveError> {
        value.try_reserve_exact(additional)
    }
}

/// Reconstructible destination endpoint intent.
#[derive(Clone, PartialEq, Eq)]
pub enum SnapshotV2SerialEndpointIntent {
    /// Create fresh destination process stdout and any supported stdin.
    DefaultProcessStdio,
    /// Resolve one inert logical selector as a fresh destination output.
    ConfiguredOutput { selector: String },
}

impl SnapshotV2SerialEndpointIntent {
    /// Constructs the destination-local default process endpoint intent.
    pub const fn default_process_stdio() -> Self {
        Self::DefaultProcessStdio
    }

    /// Constructs one bounded configured-output intent.
    pub fn try_configured_output(
        selector: impl Into<String>,
    ) -> Result<Self, SnapshotV2SerialStateBuildError> {
        let intent = Self::ConfiguredOutput {
            selector: selector.into(),
        };
        validate_endpoint_intent(&intent)?;
        Ok(intent)
    }

    /// Returns the configured inert selector, if one is required.
    pub fn configured_selector(&self) -> Option<&str> {
        match self {
            Self::DefaultProcessStdio => None,
            Self::ConfiguredOutput { selector } => Some(selector),
        }
    }

    /// Returns whether destination process stdio must be reconstructed.
    pub const fn is_default_process_stdio(&self) -> bool {
        matches!(self, Self::DefaultProcessStdio)
    }
}

impl fmt::Debug for SnapshotV2SerialEndpointIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DefaultProcessStdio => {
                formatter.write_str("SnapshotV2SerialEndpointIntent::DefaultProcessStdio")
            }
            Self::ConfiguredOutput { .. } => formatter
                .debug_struct("SnapshotV2SerialEndpointIntent::ConfiguredOutput")
                .field("selector", &REDACTED)
                .finish(),
        }
    }
}

/// Complete bounded exact-2.7 serial component value.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2SerialState {
    endpoint_intent: SnapshotV2SerialEndpointIntent,
    rate_limiter: Option<SerialRateLimiterConfig>,
    device: SerialMmioCaptureState,
}

impl SnapshotV2SerialState {
    /// Checks the inert source configuration and complete future restore
    /// resource count without allocating or touching a live capture owner.
    pub fn preflight_capture(
        config: &SerialConfig,
        storage_resource_count: usize,
    ) -> Result<(), SnapshotV2SerialStateCaptureError> {
        let serial_resource_count = match config.serial_out_path() {
            Some(path) => {
                let selector = path
                    .to_str()
                    .ok_or(SnapshotV2SerialStateCaptureError::InvalidEndpointIntent)?;
                validate_configured_selector(selector)
                    .map_err(|_| SnapshotV2SerialStateCaptureError::InvalidEndpointIntent)?;
                1
            }
            None => 0,
        };
        let resource_count = storage_resource_count
            .checked_add(serial_resource_count)
            .ok_or(SnapshotV2SerialStateCaptureError::RestoreResourceCapacity)?;
        if resource_count > MAX_SNAPSHOT_RESTORE_RESOURCES {
            return Err(SnapshotV2SerialStateCaptureError::RestoreResourceCapacity);
        }
        Ok(())
    }

    /// Converts one selected capture-ready source value into inert exact-2.7
    /// state without retaining any source endpoint ownership.
    pub fn try_from_capture_ready(
        captured: CaptureReadySerialState,
    ) -> Result<Self, SnapshotV2SerialStateCaptureError> {
        Self::try_from_capture_ready_with_policy(captured, &mut FallibleCaptureReserve)
    }

    fn try_from_capture_ready_with_policy<R: CaptureReservePolicy>(
        captured: CaptureReadySerialState,
        reserve: &mut R,
    ) -> Result<Self, SnapshotV2SerialStateCaptureError> {
        let (config, device) = captured.into_parts();
        Self::preflight_capture(&config, 0)?;
        let rate_limiter = config.rate_limiter();
        let endpoint_intent = match config.serial_out_path() {
            Some(path) => {
                let selector = path
                    .to_str()
                    .ok_or(SnapshotV2SerialStateCaptureError::InvalidEndpointIntent)?;
                let mut owned = String::new();
                reserve
                    .reserve_string(&mut owned, selector.len())
                    .map_err(SnapshotV2SerialStateCaptureError::Allocation)?;
                owned.push_str(selector);
                SnapshotV2SerialEndpointIntent::ConfiguredOutput { selector: owned }
            }
            None => SnapshotV2SerialEndpointIntent::DefaultProcessStdio,
        };
        Self::try_new(endpoint_intent, rate_limiter, device)
            .map_err(|_| SnapshotV2SerialStateCaptureError::InvalidEndpointIntent)
    }

    /// Constructs one checked serial value from inert endpoint/configuration
    /// facts and already validated guest-visible UART state.
    pub fn try_new(
        endpoint_intent: SnapshotV2SerialEndpointIntent,
        rate_limiter: Option<SerialRateLimiterConfig>,
        device: SerialMmioCaptureState,
    ) -> Result<Self, SnapshotV2SerialStateBuildError> {
        validate_endpoint_intent(&endpoint_intent)?;
        Ok(Self {
            endpoint_intent,
            rate_limiter,
            device,
        })
    }

    /// Returns the exact compatibility context of this value.
    pub const fn compatibility_version(&self) -> SnapshotFormatVersion {
        NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION
    }

    /// Returns reconstructible destination endpoint intent.
    pub const fn endpoint_intent(&self) -> &SnapshotV2SerialEndpointIntent {
        &self.endpoint_intent
    }

    /// Returns the public serial rate-limiter configuration.
    pub const fn rate_limiter(&self) -> Option<SerialRateLimiterConfig> {
        self.rate_limiter
    }

    /// Returns complete guest-visible UART state.
    pub const fn device(&self) -> &SerialMmioCaptureState {
        &self.device
    }

    /// Consumes this value into its inert parts.
    pub fn into_parts(
        self,
    ) -> (
        SnapshotV2SerialEndpointIntent,
        Option<SerialRateLimiterConfig>,
        SerialMmioCaptureState,
    ) {
        (self.endpoint_intent, self.rate_limiter, self.device)
    }

    /// Encodes the canonical serial payload for an exact outer context.
    pub fn encode(
        &self,
        outer_version: SnapshotFormatVersion,
    ) -> Result<Vec<u8>, SnapshotV2SerialStateEncodeError> {
        codec::encode(outer_version, self)
    }

    /// Decodes and validates one canonical exact-2.7 serial payload.
    pub fn decode(
        outer_version: SnapshotFormatVersion,
        bytes: &[u8],
    ) -> Result<Self, SnapshotV2SerialStateDecodeError> {
        codec::decode(outer_version, bytes)
    }
}

impl fmt::Debug for SnapshotV2SerialState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2SerialState")
            .field("endpoint_intent", &self.endpoint_intent)
            .field("rate_limiter", &self.rate_limiter)
            .field("device", &REDACTED)
            .finish()
    }
}

fn validate_endpoint_intent(
    endpoint_intent: &SnapshotV2SerialEndpointIntent,
) -> Result<(), SnapshotV2SerialStateBuildError> {
    let SnapshotV2SerialEndpointIntent::ConfiguredOutput { selector } = endpoint_intent else {
        return Ok(());
    };
    validate_configured_selector(selector)
}

fn validate_configured_selector(selector: &str) -> Result<(), SnapshotV2SerialStateBuildError> {
    if selector.is_empty()
        || selector.len() > NATIVE_V2_SERIAL_STATE_MAX_SELECTOR_BYTES
        || selector.chars().any(char::is_control)
    {
        return Err(SnapshotV2SerialStateBuildError::InvalidEndpointIntent);
    }
    Ok(())
}

/// Failure while converting one trusted live capture into exact-2.7 state.
pub enum SnapshotV2SerialStateCaptureError {
    /// The committed configured endpoint has no canonical bounded selector.
    InvalidEndpointIntent,
    /// Storage plus a future configured serial sink exceeds the complete limit.
    RestoreResourceCapacity,
    /// The bounded configured selector could not be copied.
    Allocation(TryReserveError),
}

impl fmt::Debug for SnapshotV2SerialStateCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for SnapshotV2SerialStateCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidEndpointIntent => "native-v2 captured serial endpoint intent is invalid",
            Self::RestoreResourceCapacity => {
                "native-v2 captured serial restore resource capacity is exceeded"
            }
            Self::Allocation(_) => "native-v2 captured serial allocation failed",
        })
    }
}

impl std::error::Error for SnapshotV2SerialStateCaptureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Allocation(source) => Some(source),
            Self::InvalidEndpointIntent | Self::RestoreResourceCapacity => None,
        }
    }
}

/// Failure while constructing trusted exact-2.7 serial state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2SerialStateBuildError {
    /// Configured endpoint intent has no canonical bounded selector.
    InvalidEndpointIntent,
}

impl fmt::Display for SnapshotV2SerialStateBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("native-v2 serial endpoint intent is invalid")
    }
}

impl std::error::Error for SnapshotV2SerialStateBuildError {}

/// Failure while encoding trusted exact-2.7 serial state.
pub enum SnapshotV2SerialStateEncodeError {
    /// The supplied outer semantic version is not exact 2.7.
    UnsupportedVersion,
    /// Trusted endpoint intent no longer satisfies the canonical profile.
    InvalidState(SnapshotV2SerialStateBuildError),
    /// Encoded length arithmetic overflowed.
    LengthOverflow,
    /// The encoded payload exceeds the fixed profile limit.
    TooLarge,
    /// The exact output buffer could not be reserved.
    Allocation(TryReserveError),
}

impl fmt::Debug for SnapshotV2SerialStateEncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for SnapshotV2SerialStateEncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedVersion => "native-v2 serial encoding version is unsupported",
            Self::InvalidState(_) => "native-v2 serial state is invalid",
            Self::LengthOverflow => "native-v2 serial state length arithmetic overflowed",
            Self::TooLarge => "native-v2 serial state exceeds its size limit",
            Self::Allocation(_) => "native-v2 serial output allocation failed",
        })
    }
}

impl std::error::Error for SnapshotV2SerialStateEncodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidState(source) => Some(source),
            Self::Allocation(source) => Some(source),
            Self::UnsupportedVersion | Self::LengthOverflow | Self::TooLarge => None,
        }
    }
}

/// Failure while decoding untrusted exact-2.7 serial state.
pub enum SnapshotV2SerialStateDecodeError {
    /// The supplied outer semantic version is not exact 2.7.
    UnsupportedVersion,
    /// Input ends before the fixed header.
    Truncated,
    /// Header magic, profile, tags, flags, booleans, or reserved bytes are invalid.
    InvalidHeader,
    /// Declared length arithmetic overflowed.
    LengthOverflow,
    /// Declared and actual payload lengths disagree.
    LengthMismatch,
    /// A declared bound exceeds the fixed profile limit.
    TooLarge,
    /// Endpoint tag, selector presence, UTF-8, or selector content is invalid.
    InvalidEndpointIntent,
    /// Optional rate-limiter presence fields are noncanonical.
    InvalidRateLimiter,
    /// UART fields fail complete cross-field validation.
    InvalidDeviceState(SerialMmioCaptureStateError),
    /// A bounded selector or RX allocation failed.
    Allocation(TryReserveError),
}

impl fmt::Debug for SnapshotV2SerialStateDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for SnapshotV2SerialStateDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedVersion => "native-v2 serial decoding version is unsupported",
            Self::Truncated => "native-v2 serial state is truncated",
            Self::InvalidHeader => "native-v2 serial state header is invalid",
            Self::LengthOverflow => "native-v2 serial state length arithmetic overflowed",
            Self::LengthMismatch => "native-v2 serial state length is inconsistent",
            Self::TooLarge => "native-v2 serial state exceeds its bounds",
            Self::InvalidEndpointIntent => "native-v2 serial endpoint intent is invalid",
            Self::InvalidRateLimiter => "native-v2 serial rate limiter is invalid",
            Self::InvalidDeviceState(_) => "native-v2 serial UART state is invalid",
            Self::Allocation(_) => "native-v2 serial state allocation failed",
        })
    }
}

impl std::error::Error for SnapshotV2SerialStateDecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidDeviceState(source) => Some(source),
            Self::Allocation(source) => Some(source),
            Self::UnsupportedVersion
            | Self::Truncated
            | Self::InvalidHeader
            | Self::LengthOverflow
            | Self::LengthMismatch
            | Self::TooLarge
            | Self::InvalidEndpointIntent
            | Self::InvalidRateLimiter => None,
        }
    }
}
