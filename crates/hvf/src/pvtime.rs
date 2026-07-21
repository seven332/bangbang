//! HVF arm64 PVTime measurement and gated SMCCC primitives.

use std::fmt;

use bangbang_runtime::BackendError;

pub(crate) const ARM_SMCCC_PV_TIME_FEATURES_64: u64 = 0xc500_0020;
pub(crate) const ARM_SMCCC_PV_TIME_ST_64: u64 = 0xc500_0021;
pub(crate) const ARM_SMCCC_PV_TIME_FEATURES_32: u64 = 0x8500_0020;
pub(crate) const ARM_SMCCC_PV_TIME_ST_32: u64 = 0x8500_0021;
pub(crate) const ARM_SMCCC_RET_NOT_SUPPORTED_64: u64 = u64::MAX;

/// Failure while measuring cumulative HVF vCPU execution time for PVTime.
#[derive(Clone, PartialEq, Eq)]
pub enum HvfArm64PvTimeMeasurementError {
    ExecutionTime(BackendError),
    Timebase(BackendError),
    InvalidTimebase,
    NanosecondOverflow,
}

impl fmt::Debug for HvfArm64PvTimeMeasurementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let stage = match self {
            Self::ExecutionTime(_) => "execution-time query",
            Self::Timebase(_) => "Mach timebase query",
            Self::InvalidTimebase => "Mach timebase validation",
            Self::NanosecondOverflow => "nanosecond conversion",
        };
        f.debug_struct("HvfArm64PvTimeMeasurementError")
            .field("stage", &stage)
            .field("source", &"<redacted>")
            .finish()
    }
}

impl fmt::Display for HvfArm64PvTimeMeasurementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExecutionTime(source) => {
                write!(f, "HVF vCPU execution-time query failed: {source}")
            }
            Self::Timebase(source) => write!(f, "Mach timebase query failed: {source}"),
            Self::InvalidTimebase => f.write_str("Mach timebase numerator or denominator is zero"),
            Self::NanosecondOverflow => {
                f.write_str("HVF vCPU execution time exceeds nanosecond representation")
            }
        }
    }
}

impl std::error::Error for HvfArm64PvTimeMeasurementError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ExecutionTime(source) | Self::Timebase(source) => Some(source),
            Self::InvalidTimebase | Self::NanosecondOverflow => None,
        }
    }
}

/// Report whether the current process exports the required public HVF symbol.
pub fn is_hvf_arm64_pvtime_measurement_available() -> bool {
    crate::ffi::vcpu_exec_time_available()
}

pub(crate) fn measure_vcpu_execution_time_ns(
    vcpu: crate::ffi::HvVcpu,
) -> Result<u64, HvfArm64PvTimeMeasurementError> {
    measure_vcpu_execution_time_ns_with(
        || crate::ffi::get_vcpu_exec_time(vcpu),
        crate::ffi::timebase_info,
    )
}

fn measure_vcpu_execution_time_ns_with(
    read_exec_time: impl FnOnce() -> Result<u64, BackendError>,
    read_timebase: impl FnOnce() -> Result<crate::ffi::MachTimebaseInfo, BackendError>,
) -> Result<u64, HvfArm64PvTimeMeasurementError> {
    let ticks = read_exec_time().map_err(HvfArm64PvTimeMeasurementError::ExecutionTime)?;
    let timebase = read_timebase().map_err(HvfArm64PvTimeMeasurementError::Timebase)?;
    mach_ticks_to_nanoseconds(ticks, timebase)
}

fn mach_ticks_to_nanoseconds(
    ticks: u64,
    timebase: crate::ffi::MachTimebaseInfo,
) -> Result<u64, HvfArm64PvTimeMeasurementError> {
    let numerator = u128::from(timebase.numer());
    let denominator = u128::from(timebase.denom());
    if numerator == 0 || denominator == 0 {
        return Err(HvfArm64PvTimeMeasurementError::InvalidTimebase);
    }
    let nanoseconds = u128::from(ticks)
        .checked_mul(numerator)
        .ok_or(HvfArm64PvTimeMeasurementError::NanosecondOverflow)?
        / denominator;
    u64::try_from(nanoseconds).map_err(|_| HvfArm64PvTimeMeasurementError::NanosecondOverflow)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct HvfArm64PvTimeHvcPolicy {
    record_ipa: Option<u64>,
}

impl HvfArm64PvTimeHvcPolicy {
    pub(crate) const fn disabled() -> Self {
        Self { record_ipa: None }
    }

    #[cfg(test)]
    pub(crate) const fn enabled(record_ipa: u64) -> Self {
        Self {
            record_ipa: Some(record_ipa),
        }
    }

    pub(crate) const fn available(self) -> bool {
        self.record_ipa.is_some()
    }

    const fn record_ipa(self) -> Option<u64> {
        self.record_ipa
    }
}

impl fmt::Debug for HvfArm64PvTimeHvcPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfArm64PvTimeHvcPolicy")
            .field("available", &self.available())
            .finish()
    }
}

pub(crate) const fn dispatch_pvtime_call(
    function_id: u64,
    arg0: u64,
    policy: HvfArm64PvTimeHvcPolicy,
) -> Option<u64> {
    match function_id {
        ARM_SMCCC_PV_TIME_FEATURES_64 => Some(
            if policy.available()
                && matches!(
                    arg0,
                    ARM_SMCCC_PV_TIME_FEATURES_64 | ARM_SMCCC_PV_TIME_ST_64
                )
            {
                0
            } else {
                ARM_SMCCC_RET_NOT_SUPPORTED_64
            },
        ),
        ARM_SMCCC_PV_TIME_ST_64 => Some(match policy.record_ipa() {
            Some(record_ipa) => record_ipa,
            None => ARM_SMCCC_RET_NOT_SUPPORTED_64,
        }),
        ARM_SMCCC_PV_TIME_FEATURES_32 | ARM_SMCCC_PV_TIME_ST_32 => {
            Some(ARM_SMCCC_RET_NOT_SUPPORTED_64)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use bangbang_runtime::BackendError;

    use super::{
        ARM_SMCCC_PV_TIME_FEATURES_32, ARM_SMCCC_PV_TIME_FEATURES_64, ARM_SMCCC_PV_TIME_ST_32,
        ARM_SMCCC_PV_TIME_ST_64, ARM_SMCCC_RET_NOT_SUPPORTED_64, HvfArm64PvTimeHvcPolicy,
        HvfArm64PvTimeMeasurementError, dispatch_pvtime_call, mach_ticks_to_nanoseconds,
        measure_vcpu_execution_time_ns_with,
    };
    use crate::ffi::MachTimebaseInfo;

    #[test]
    fn mach_ticks_convert_with_checked_integer_arithmetic() {
        assert_eq!(
            mach_ticks_to_nanoseconds(15, MachTimebaseInfo::new(2, 3)),
            Ok(10)
        );
        assert_eq!(
            mach_ticks_to_nanoseconds(1, MachTimebaseInfo::new(0, 1)),
            Err(HvfArm64PvTimeMeasurementError::InvalidTimebase)
        );
        assert_eq!(
            mach_ticks_to_nanoseconds(1, MachTimebaseInfo::new(1, 0)),
            Err(HvfArm64PvTimeMeasurementError::InvalidTimebase)
        );
        assert_eq!(
            mach_ticks_to_nanoseconds(u64::MAX, MachTimebaseInfo::new(u32::MAX, 1)),
            Err(HvfArm64PvTimeMeasurementError::NanosecondOverflow)
        );
    }

    #[test]
    fn measurement_preserves_stage_and_redacts_observations() {
        assert_eq!(
            measure_vcpu_execution_time_ns_with(|| Ok(21), || Ok(MachTimebaseInfo::new(2, 3))),
            Ok(14)
        );
        let exec_error = measure_vcpu_execution_time_ns_with(
            || Err(BackendError::InvalidState("injected execution failure")),
            || panic!("timebase must not run after execution-time failure"),
        )
        .expect_err("execution query should fail");
        assert!(matches!(
            exec_error,
            HvfArm64PvTimeMeasurementError::ExecutionTime(_)
        ));
        assert!(!format!("{exec_error:?}").contains("injected execution failure"));

        let timebase_error = measure_vcpu_execution_time_ns_with(
            || Ok(u64::MAX),
            || Err(BackendError::InvalidState("injected timebase failure")),
        )
        .expect_err("timebase query should fail");
        assert!(matches!(
            timebase_error,
            HvfArm64PvTimeMeasurementError::Timebase(_)
        ));
        assert!(!format!("{timebase_error:?}").contains(&u64::MAX.to_string()));
    }

    #[test]
    fn disabled_policy_rejects_discovery_and_direct_calls() {
        let disabled = HvfArm64PvTimeHvcPolicy::disabled();

        assert_eq!(
            dispatch_pvtime_call(
                ARM_SMCCC_PV_TIME_FEATURES_64,
                ARM_SMCCC_PV_TIME_ST_64,
                disabled
            ),
            Some(ARM_SMCCC_RET_NOT_SUPPORTED_64)
        );
        assert_eq!(
            dispatch_pvtime_call(ARM_SMCCC_PV_TIME_ST_64, 0, disabled),
            Some(ARM_SMCCC_RET_NOT_SUPPORTED_64)
        );
    }

    #[test]
    fn enabled_policy_returns_only_its_record_and_rejects_unknown_features() {
        let policy = HvfArm64PvTimeHvcPolicy::enabled(0x801f_e800);

        assert_eq!(
            dispatch_pvtime_call(
                ARM_SMCCC_PV_TIME_FEATURES_64,
                ARM_SMCCC_PV_TIME_FEATURES_64,
                policy
            ),
            Some(0)
        );
        assert_eq!(
            dispatch_pvtime_call(
                ARM_SMCCC_PV_TIME_FEATURES_64,
                ARM_SMCCC_PV_TIME_ST_64,
                policy
            ),
            Some(0)
        );
        assert_eq!(
            dispatch_pvtime_call(ARM_SMCCC_PV_TIME_ST_64, 0, policy),
            Some(0x801f_e800)
        );
        assert_eq!(
            dispatch_pvtime_call(ARM_SMCCC_PV_TIME_FEATURES_64, u64::MAX, policy),
            Some(ARM_SMCCC_RET_NOT_SUPPORTED_64)
        );
        assert!(!format!("{policy:?}").contains("801f"));
    }

    #[test]
    fn thirty_two_bit_aliases_are_rejected_with_signed_64_status() {
        let policy = HvfArm64PvTimeHvcPolicy::enabled(0x1000);

        assert_eq!(
            dispatch_pvtime_call(
                ARM_SMCCC_PV_TIME_FEATURES_32,
                ARM_SMCCC_PV_TIME_ST_32,
                policy
            ),
            Some(u64::MAX)
        );
        assert_eq!(
            dispatch_pvtime_call(ARM_SMCCC_PV_TIME_ST_32, 0, policy),
            Some(u64::MAX)
        );
        assert_eq!(dispatch_pvtime_call(0xdead_beef, 0, policy), None);
    }
}
