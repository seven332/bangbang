//! HVF arm64 PVTime measurement and gated SMCCC primitives.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bangbang_runtime::BackendError;
use bangbang_runtime::memory::{GuestMemoryAccessError, GuestMemoryAtomicU64};

pub(crate) const ARM_SMCCC_PV_TIME_FEATURES_64: u64 = 0xc500_0020;
pub(crate) const ARM_SMCCC_PV_TIME_ST_64: u64 = 0xc500_0021;
pub(crate) const ARM_SMCCC_PV_TIME_FEATURES_32: u64 = 0x8500_0020;
pub(crate) const ARM_SMCCC_PV_TIME_ST_32: u64 = 0x8500_0021;
pub(crate) const ARM_SMCCC_RET_NOT_SUPPORTED_64: u64 = u64::MAX;

/// Failure while measuring cumulative HVF vCPU execution time for PVTime.
#[derive(Clone, PartialEq, Eq)]
pub enum HvfArm64PvTimeMeasurementError {
    MonotonicTime(BackendError),
    ExecutionTime(BackendError),
    Timebase(BackendError),
    InvalidTimebase,
    NanosecondOverflow,
}

impl fmt::Debug for HvfArm64PvTimeMeasurementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let stage = match self {
            Self::MonotonicTime(_) => "monotonic-time query",
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
            Self::MonotonicTime(source) => write!(f, "monotonic-time query failed: {source}"),
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
            Self::MonotonicTime(source) | Self::ExecutionTime(source) | Self::Timebase(source) => {
                Some(source)
            }
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

pub(crate) fn measure_monotonic_time_ns() -> Result<u64, HvfArm64PvTimeMeasurementError> {
    let ticks =
        crate::ffi::absolute_time().map_err(HvfArm64PvTimeMeasurementError::MonotonicTime)?;
    let timebase = crate::ffi::timebase_info().map_err(HvfArm64PvTimeMeasurementError::Timebase)?;
    mach_ticks_to_nanoseconds(ticks, timebase)
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

/// Hidden control used by signed tests to create deterministic runnable delay.
///
/// The probe supplies no accounting value. While enabled, the already-admitted
/// owner thread waits for `delay`, and the ordinary wall/execution sampler
/// observes that real interval. Production process configuration never creates
/// this probe.
#[derive(Clone)]
pub struct HvfArm64PvTimeContentionProbe {
    enabled: Arc<AtomicBool>,
    delay: Duration,
}

impl HvfArm64PvTimeContentionProbe {
    #[doc(hidden)]
    pub fn new(delay: Duration) -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(true)),
            delay,
        }
    }

    #[doc(hidden)]
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Release);
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    fn delay_if_enabled(&self) {
        if self.is_enabled() && !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
    }
}

impl fmt::Debug for HvfArm64PvTimeContentionProbe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfArm64PvTimeContentionProbe")
            .field("enabled", &self.is_enabled())
            .field("delay", &self.delay)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HvfArm64PvTimeRunSample {
    wall_time_ns: u64,
    execution_time_ns: u64,
}

impl HvfArm64PvTimeRunSample {
    pub(crate) const fn new(wall_time_ns: u64, execution_time_ns: u64) -> Self {
        Self {
            wall_time_ns,
            execution_time_ns,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HvfArm64PvTimeRunDisposition {
    Runnable,
    IdleOrCanceled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfArm64PvTimeAccountingStage {
    Configuration,
    Publication,
    StartWallSample,
    StartExecutionSample,
    EndExecutionSample,
    EndWallSample,
    WallRegression,
    ExecutionRegression,
}

impl fmt::Display for HvfArm64PvTimeAccountingStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let stage = match self {
            Self::Configuration => "configuration",
            Self::Publication => "publication",
            Self::StartWallSample => "start wall sample",
            Self::StartExecutionSample => "start execution sample",
            Self::EndExecutionSample => "end execution sample",
            Self::EndWallSample => "end wall sample",
            Self::WallRegression => "wall-clock regression",
            Self::ExecutionRegression => "execution-counter regression",
        };
        f.write_str(stage)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfArm64PvTimeAccountingError {
    AlreadyConfigured,
    Publication(GuestMemoryAccessError),
    WallRegression,
    ExecutionRegression,
}

impl fmt::Display for HvfArm64PvTimeAccountingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyConfigured => f.write_str("PVTime accounting is already configured"),
            Self::Publication(source) => write!(f, "PVTime publication failed: {source}"),
            Self::WallRegression => f.write_str("PVTime monotonic wall clock regressed"),
            Self::ExecutionRegression => {
                f.write_str("PVTime cumulative execution counter regressed")
            }
        }
    }
}

impl std::error::Error for HvfArm64PvTimeAccountingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Publication(source) => Some(source),
            Self::AlreadyConfigured | Self::WallRegression | Self::ExecutionRegression => None,
        }
    }
}

#[derive(Clone)]
pub(crate) struct HvfArm64PvTimeAccountingConfig {
    record_ipa: u64,
    publisher: GuestMemoryAtomicU64,
    initial_stolen_time_ns: u64,
    contention_probe: Option<HvfArm64PvTimeContentionProbe>,
}

impl HvfArm64PvTimeAccountingConfig {
    pub(crate) const fn new(
        record_ipa: u64,
        publisher: GuestMemoryAtomicU64,
        initial_stolen_time_ns: u64,
        contention_probe: Option<HvfArm64PvTimeContentionProbe>,
    ) -> Self {
        Self {
            record_ipa,
            publisher,
            initial_stolen_time_ns,
            contention_probe,
        }
    }
}

impl fmt::Debug for HvfArm64PvTimeAccountingConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfArm64PvTimeAccountingConfig")
            .field("publisher", &self.publisher)
            .field("contention_probe", &self.contention_probe.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub(crate) struct HvfArm64PvTimeAccounting {
    enabled: Option<HvfArm64PvTimeAccountingEnabled>,
}

#[derive(Debug)]
struct HvfArm64PvTimeAccountingEnabled {
    record_ipa: u64,
    publisher: GuestMemoryAtomicU64,
    stolen_time_ns: u64,
    contention_probe: Option<HvfArm64PvTimeContentionProbe>,
}

impl HvfArm64PvTimeAccounting {
    pub(crate) const fn disabled() -> Self {
        Self { enabled: None }
    }

    pub(crate) fn configure(
        &mut self,
        config: HvfArm64PvTimeAccountingConfig,
    ) -> Result<(), HvfArm64PvTimeAccountingError> {
        if self.enabled.is_some() {
            return Err(HvfArm64PvTimeAccountingError::AlreadyConfigured);
        }
        config
            .publisher
            .store_le(config.initial_stolen_time_ns)
            .map_err(HvfArm64PvTimeAccountingError::Publication)?;
        self.enabled = Some(HvfArm64PvTimeAccountingEnabled {
            record_ipa: config.record_ipa,
            publisher: config.publisher,
            stolen_time_ns: config.initial_stolen_time_ns,
            contention_probe: config.contention_probe,
        });
        Ok(())
    }

    pub(crate) const fn policy(&self) -> HvfArm64PvTimeHvcPolicy {
        match self.enabled.as_ref() {
            Some(enabled) => HvfArm64PvTimeHvcPolicy::enabled(enabled.record_ipa),
            None => HvfArm64PvTimeHvcPolicy::disabled(),
        }
    }

    pub(crate) fn publish(&self) -> Result<bool, HvfArm64PvTimeAccountingError> {
        let Some(enabled) = self.enabled.as_ref() else {
            return Ok(false);
        };
        enabled
            .publisher
            .store_le(enabled.stolen_time_ns)
            .map_err(HvfArm64PvTimeAccountingError::Publication)?;
        Ok(true)
    }

    pub(crate) fn delay_for_contention_probe(&self) {
        if let Some(probe) = self
            .enabled
            .as_ref()
            .and_then(|enabled| enabled.contention_probe.as_ref())
        {
            probe.delay_if_enabled();
        }
    }

    pub(crate) fn finish_run(
        &mut self,
        start: HvfArm64PvTimeRunSample,
        end: HvfArm64PvTimeRunSample,
        disposition: HvfArm64PvTimeRunDisposition,
    ) -> Result<(), HvfArm64PvTimeAccountingError> {
        let Some(enabled) = self.enabled.as_mut() else {
            return Ok(());
        };
        let wall_delta = end
            .wall_time_ns
            .checked_sub(start.wall_time_ns)
            .ok_or(HvfArm64PvTimeAccountingError::WallRegression)?;
        let execution_delta = end
            .execution_time_ns
            .checked_sub(start.execution_time_ns)
            .ok_or(HvfArm64PvTimeAccountingError::ExecutionRegression)?;
        if disposition == HvfArm64PvTimeRunDisposition::Runnable {
            let stolen_delta = wall_delta.saturating_sub(execution_delta);
            enabled.stolen_time_ns = enabled.stolen_time_ns.saturating_add(stolen_delta);
        }
        Ok(())
    }

    pub(crate) fn capture(
        &self,
    ) -> Result<Option<HvfArm64PvTimeVcpuCaptureState>, HvfArm64PvTimeAccountingError> {
        let Some(enabled) = self.enabled.as_ref() else {
            return Ok(None);
        };
        enabled
            .publisher
            .store_le(enabled.stolen_time_ns)
            .map_err(HvfArm64PvTimeAccountingError::Publication)?;
        Ok(Some(HvfArm64PvTimeVcpuCaptureState {
            stolen_time_ns: enabled.stolen_time_ns,
        }))
    }
}

/// Capture-ready committed PVTime value for one topology member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64PvTimeVcpuCaptureState {
    stolen_time_ns: u64,
}

impl HvfArm64PvTimeVcpuCaptureState {
    pub const fn stolen_time_ns(self) -> u64 {
        self.stolen_time_ns
    }
}

/// Topology-ordered capture-ready PVTime values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HvfArm64PvTimeCaptureState {
    vcpus: Vec<HvfArm64PvTimeVcpuCaptureState>,
}

impl HvfArm64PvTimeCaptureState {
    pub(crate) const fn new(vcpus: Vec<HvfArm64PvTimeVcpuCaptureState>) -> Self {
        Self { vcpus }
    }

    pub fn vcpus(&self) -> &[HvfArm64PvTimeVcpuCaptureState] {
        &self.vcpus
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
    use bangbang_runtime::memory::{
        GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange,
    };

    use super::{
        ARM_SMCCC_PV_TIME_FEATURES_32, ARM_SMCCC_PV_TIME_FEATURES_64, ARM_SMCCC_PV_TIME_ST_32,
        ARM_SMCCC_PV_TIME_ST_64, ARM_SMCCC_RET_NOT_SUPPORTED_64, HvfArm64PvTimeAccounting,
        HvfArm64PvTimeAccountingConfig, HvfArm64PvTimeAccountingError, HvfArm64PvTimeHvcPolicy,
        HvfArm64PvTimeMeasurementError, HvfArm64PvTimeRunDisposition, HvfArm64PvTimeRunSample,
        dispatch_pvtime_call, mach_ticks_to_nanoseconds, measure_vcpu_execution_time_ns_with,
    };
    use crate::ffi::MachTimebaseInfo;

    fn test_accounting(
        initial_stolen_time_ns: u64,
    ) -> (
        HvfArm64PvTimeAccounting,
        bangbang_runtime::memory::GuestMemoryAtomicU64,
    ) {
        let range = GuestMemoryRange::new(GuestAddress::new(0), 64 * 1024)
            .expect("test range should be valid");
        let layout = GuestMemoryLayout::new(vec![range]).expect("test layout should be valid");
        let memory = GuestMemory::allocate(&layout).expect("test memory should allocate");
        let publisher = memory
            .atomic_u64(GuestAddress::new(8))
            .expect("test publisher should be aligned");
        let retained = publisher.clone();
        let mut accounting = HvfArm64PvTimeAccounting::disabled();
        accounting
            .configure(HvfArm64PvTimeAccountingConfig::new(
                0,
                publisher,
                initial_stolen_time_ns,
                None,
            ))
            .expect("test accounting should configure");
        (accounting, retained)
    }

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

    #[test]
    fn accounting_publishes_old_value_then_commits_saturating_runnable_delta() {
        let (mut accounting, publisher) = test_accounting(5);

        assert_eq!(publisher.load_le(), 5);
        assert_eq!(accounting.publish(), Ok(true));
        accounting
            .finish_run(
                HvfArm64PvTimeRunSample::new(100, 30),
                HvfArm64PvTimeRunSample::new(170, 80),
                HvfArm64PvTimeRunDisposition::Runnable,
            )
            .expect("forward samples should commit");
        assert_eq!(publisher.load_le(), 5);
        let capture = accounting
            .capture()
            .expect("capture publication should succeed")
            .expect("configured accounting should capture");

        assert_eq!(capture.stolen_time_ns(), 25);
        assert_eq!(publisher.load_le(), 25);

        accounting
            .finish_run(
                HvfArm64PvTimeRunSample::new(200, 100),
                HvfArm64PvTimeRunSample::new(210, 130),
                HvfArm64PvTimeRunDisposition::Runnable,
            )
            .expect("execution precision skew should saturate at zero");
        assert_eq!(
            accounting
                .capture()
                .expect("capture should succeed")
                .expect("capture should remain enabled")
                .stolen_time_ns(),
            25
        );
    }

    #[test]
    fn accounting_discards_idle_and_canceled_windows() {
        let (mut accounting, _) = test_accounting(11);

        for disposition in [
            HvfArm64PvTimeRunDisposition::IdleOrCanceled,
            HvfArm64PvTimeRunDisposition::IdleOrCanceled,
        ] {
            accounting
                .finish_run(
                    HvfArm64PvTimeRunSample::new(10, 1),
                    HvfArm64PvTimeRunSample::new(1_010, 2),
                    disposition,
                )
                .expect("discarded forward samples should validate");
        }

        assert_eq!(
            accounting
                .capture()
                .expect("capture should succeed")
                .expect("capture should remain enabled")
                .stolen_time_ns(),
            11
        );
    }

    #[test]
    fn accounting_keeps_smp_vcpu_values_independent_and_topology_ordered() {
        let (mut first, first_publisher) = test_accounting(3);
        let (mut second, second_publisher) = test_accounting(7);
        first
            .finish_run(
                HvfArm64PvTimeRunSample::new(100, 20),
                HvfArm64PvTimeRunSample::new(200, 80),
                HvfArm64PvTimeRunDisposition::Runnable,
            )
            .expect("first vCPU samples should commit");
        second
            .finish_run(
                HvfArm64PvTimeRunSample::new(300, 100),
                HvfArm64PvTimeRunSample::new(500, 140),
                HvfArm64PvTimeRunDisposition::Runnable,
            )
            .expect("second vCPU samples should commit");

        let capture = super::HvfArm64PvTimeCaptureState::new(vec![
            first
                .capture()
                .expect("first capture should publish")
                .expect("first vCPU should be configured"),
            second
                .capture()
                .expect("second capture should publish")
                .expect("second vCPU should be configured"),
        ]);

        assert_eq!(
            capture
                .vcpus()
                .iter()
                .map(|state| state.stolen_time_ns())
                .collect::<Vec<_>>(),
            [43, 167]
        );
        assert_eq!(first_publisher.load_le(), 43);
        assert_eq!(second_publisher.load_le(), 167);
    }

    #[test]
    fn accounting_rejects_clock_and_execution_regressions_without_committing() {
        let (mut accounting, _) = test_accounting(19);

        assert_eq!(
            accounting.finish_run(
                HvfArm64PvTimeRunSample::new(20, 5),
                HvfArm64PvTimeRunSample::new(19, 6),
                HvfArm64PvTimeRunDisposition::Runnable,
            ),
            Err(HvfArm64PvTimeAccountingError::WallRegression)
        );
        assert_eq!(
            accounting.finish_run(
                HvfArm64PvTimeRunSample::new(20, 5),
                HvfArm64PvTimeRunSample::new(21, 4),
                HvfArm64PvTimeRunDisposition::Runnable,
            ),
            Err(HvfArm64PvTimeAccountingError::ExecutionRegression)
        );
        assert_eq!(
            accounting
                .capture()
                .expect("capture should succeed")
                .expect("capture should remain enabled")
                .stolen_time_ns(),
            19
        );
    }

    #[test]
    fn accounting_saturates_and_restore_starts_without_a_downtime_baseline() {
        let (mut source, _) = test_accounting(u64::MAX - 3);
        source
            .finish_run(
                HvfArm64PvTimeRunSample::new(0, 0),
                HvfArm64PvTimeRunSample::new(10, 1),
                HvfArm64PvTimeRunDisposition::Runnable,
            )
            .expect("forward source samples should commit");
        let captured = source
            .capture()
            .expect("source capture should succeed")
            .expect("source should be enabled");
        assert_eq!(captured.stolen_time_ns(), u64::MAX);

        let (mut destination, _) = test_accounting(captured.stolen_time_ns());
        assert_eq!(
            destination
                .capture()
                .expect("destination capture should succeed")
                .expect("destination should be enabled")
                .stolen_time_ns(),
            u64::MAX
        );
        destination
            .finish_run(
                HvfArm64PvTimeRunSample::new(9_000_000, 0),
                HvfArm64PvTimeRunSample::new(9_000_001, 0),
                HvfArm64PvTimeRunDisposition::Runnable,
            )
            .expect("destination should begin a fresh sample window");
        assert_eq!(
            destination
                .capture()
                .expect("destination capture should succeed")
                .expect("destination should be enabled")
                .stolen_time_ns(),
            u64::MAX
        );
    }
}
