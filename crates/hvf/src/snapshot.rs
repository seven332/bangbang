//! Native arm64 snapshot policy for Hypervisor.framework-only state.

use std::fmt;

use bangbang_runtime::BackendError;

use crate::vcpu::{
    HvfArm64VcpuBreakpointRegisterState, HvfArm64VcpuExecutionControlRegisterState,
    HvfArm64VcpuPhysicalTimerState, HvfArm64VcpuSmePstate, HvfArm64VcpuVirtualTimerState,
    HvfArm64VcpuWatchpointRegisterState,
};

const TIMER_CONTROL_ENABLE: u64 = 1 << 0;
const TIMER_CONTROL_IMASK: u64 = 1 << 1;
const TIMER_CONTROL_ISTATUS: u64 = 1 << 2;
const TIMER_CONTROL_WRITABLE_BITS: u64 = TIMER_CONTROL_ENABLE | TIMER_CONTROL_IMASK;
const TIMER_CONTROL_CAPTURE_BITS: u64 = TIMER_CONTROL_WRITABLE_BITS | TIMER_CONTROL_ISTATUS;
const CPACR_EL1_ZEN_MASK: u64 = 0b11 << 16;
const CPACR_EL1_SMEN_MASK: u64 = 0b11 << 24;
const DEBUG_CONTROL_ENABLE: u64 = 1;

/// Portable native-HVF timer state normalized around one host-counter sample.
///
/// The virtual timer stores its frozen guest-visible counter rather than the
/// source host's vTimer offset. The physical timer stores the wrapping distance
/// from the capture sample to its full-width comparator rather than the source
/// host's absolute comparator. Derived ISTATUS and relative TVAL observations
/// are deliberately absent.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64SnapshotTimerState {
    virtual_timer_exit_masked: bool,
    cntkctl_el1: u64,
    virtual_count: u64,
    virtual_control: u64,
    virtual_compare_value: u64,
    physical_control: u64,
    physical_compare_delta: u64,
}

impl HvfArm64SnapshotTimerState {
    /// Build already-normalized timer state.
    ///
    /// Only ENABLE and IMASK may be supplied in either control. Raw captures
    /// should instead use [`normalize_arm64_snapshot_timer_state`], which strips
    /// the derived ISTATUS observation after validating the raw bit inventory.
    pub fn try_new(
        virtual_timer_exit_masked: bool,
        cntkctl_el1: u64,
        virtual_count: u64,
        virtual_control: u64,
        virtual_compare_value: u64,
        physical_control: u64,
        physical_compare_delta: u64,
    ) -> Result<Self, HvfArm64SnapshotTimerPolicyError> {
        if virtual_control & !TIMER_CONTROL_WRITABLE_BITS != 0 {
            return Err(HvfArm64SnapshotTimerPolicyError::VirtualTimerControl);
        }
        if physical_control & !TIMER_CONTROL_WRITABLE_BITS != 0 {
            return Err(HvfArm64SnapshotTimerPolicyError::PhysicalTimerControl);
        }

        Ok(Self {
            virtual_timer_exit_masked,
            cntkctl_el1,
            virtual_count,
            virtual_control,
            virtual_compare_value,
            physical_control,
            physical_compare_delta,
        })
    }

    /// Return whether Hypervisor.framework vTimer exits were masked.
    pub const fn virtual_timer_exit_masked(self) -> bool {
        self.virtual_timer_exit_masked
    }

    /// Return the captured raw `CNTKCTL_EL1` value.
    pub const fn cntkctl_el1(self) -> u64 {
        self.cntkctl_el1
    }

    /// Return the frozen guest-visible virtual counter.
    pub const fn virtual_count(self) -> u64 {
        self.virtual_count
    }

    /// Return writable ENABLE/IMASK bits for `CNTV_CTL_EL0`.
    pub const fn virtual_control(self) -> u64 {
        self.virtual_control
    }

    /// Return the full-width `CNTV_CVAL_EL0` comparator.
    pub const fn virtual_compare_value(self) -> u64 {
        self.virtual_compare_value
    }

    /// Return writable ENABLE/IMASK bits for `CNTP_CTL_EL0`.
    pub const fn physical_control(self) -> u64 {
        self.physical_control
    }

    /// Return the wrapping distance from capture time to `CNTP_CVAL_EL0`.
    pub const fn physical_compare_delta(self) -> u64 {
        self.physical_compare_delta
    }
}

impl fmt::Debug for HvfArm64SnapshotTimerState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfArm64SnapshotTimerState")
            .field("timer_state", &"<redacted>")
            .finish()
    }
}

/// Policy rejection while normalizing native-HVF arm64 timers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfArm64SnapshotTimerPolicyError {
    /// The virtual-timer control contains bits outside ENABLE/IMASK/ISTATUS.
    VirtualTimerControl,
    /// The physical-timer control contains bits outside ENABLE/IMASK/ISTATUS.
    PhysicalTimerControl,
}

impl fmt::Display for HvfArm64SnapshotTimerPolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VirtualTimerControl => {
                f.write_str("unsupported arm64 virtual-timer control state")
            }
            Self::PhysicalTimerControl => {
                f.write_str("unsupported arm64 physical-timer control state")
            }
        }
    }
}

impl std::error::Error for HvfArm64SnapshotTimerPolicyError {}

/// Normalize raw arm64 timer captures at one `mach_absolute_time()` sample.
///
/// Arithmetic intentionally wraps at 64 bits, matching the architectural
/// generic counter. `CNTP_TVAL_EL0` and both derived ISTATUS bits are ignored.
pub fn normalize_arm64_snapshot_timer_state(
    physical: HvfArm64VcpuPhysicalTimerState,
    virtual_timer: HvfArm64VcpuVirtualTimerState,
    counter_sample: u64,
) -> Result<HvfArm64SnapshotTimerState, HvfArm64SnapshotTimerPolicyError> {
    let virtual_control = normalize_raw_timer_control(
        virtual_timer.control(),
        HvfArm64SnapshotTimerPolicyError::VirtualTimerControl,
    )?;
    let physical_control = normalize_raw_timer_control(
        physical.cntp_ctl_el0(),
        HvfArm64SnapshotTimerPolicyError::PhysicalTimerControl,
    )?;

    Ok(HvfArm64SnapshotTimerState {
        virtual_timer_exit_masked: virtual_timer.masked(),
        cntkctl_el1: physical.cntkctl_el1(),
        virtual_count: counter_sample.wrapping_sub(virtual_timer.offset()),
        virtual_control,
        virtual_compare_value: virtual_timer.compare_value(),
        physical_control,
        physical_compare_delta: physical.cntp_cval_el0().wrapping_sub(counter_sample),
    })
}

fn normalize_raw_timer_control(
    control: u64,
    rejection: HvfArm64SnapshotTimerPolicyError,
) -> Result<u64, HvfArm64SnapshotTimerPolicyError> {
    if control & !TIMER_CONTROL_CAPTURE_BITS != 0 {
        return Err(rejection);
    }
    Ok(control & TIMER_CONTROL_WRITABLE_BITS)
}

/// One ordered preflight or write operation in normalized timer restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HvfArm64SnapshotTimerRestoreOperation {
    /// Preflight read of `CNTKCTL_EL1`.
    ReadCntkctlEl1,
    /// Preflight read of `CNTP_CTL_EL0`.
    ReadPhysicalControl,
    /// Preflight read of `CNTP_CVAL_EL0`.
    ReadPhysicalCompareValue,
    /// Preflight read of `CNTP_TVAL_EL0`.
    ReadPhysicalTimerValue,
    /// Preflight read of the Hypervisor.framework vTimer mask.
    ReadVirtualTimerExitMask,
    /// Preflight read of the Hypervisor.framework vTimer offset.
    ReadVirtualTimerOffset,
    /// Preflight read of `CNTV_CTL_EL0`.
    ReadVirtualControl,
    /// Preflight read of `CNTV_CVAL_EL0`.
    ReadVirtualCompareValue,
    /// Sampling of the destination host generic-counter domain.
    SampleCounter,
    /// Force the Hypervisor.framework vTimer exit mask on.
    MaskVirtualTimerExits,
    /// Disable `CNTV_CTL_EL0` before changing timer state.
    DisableVirtualTimer,
    /// Disable `CNTP_CTL_EL0` before changing timer state.
    DisablePhysicalTimer,
    /// Write captured `CNTKCTL_EL1`.
    WriteCntkctlEl1,
    /// Write the destination-adjusted `CNTP_CVAL_EL0`.
    WritePhysicalCompareValue,
    /// Write the destination-adjusted Hypervisor.framework vTimer offset.
    WriteVirtualTimerOffset,
    /// Write captured `CNTV_CVAL_EL0`.
    WriteVirtualCompareValue,
    /// Restore writable `CNTP_CTL_EL0` bits.
    RestorePhysicalControl,
    /// Restore writable `CNTV_CTL_EL0` bits.
    RestoreVirtualControl,
    /// Restore the captured Hypervisor.framework vTimer exit mask.
    RestoreVirtualTimerExitMask,
}

/// Failure during preflight or ordered application of normalized timer state.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfArm64SnapshotTimerRestoreError {
    operation: HvfArm64SnapshotTimerRestoreOperation,
    completed_writes: usize,
    source: BackendError,
}

impl HvfArm64SnapshotTimerRestoreError {
    /// Return the operation that failed.
    pub const fn operation(&self) -> HvfArm64SnapshotTimerRestoreOperation {
        self.operation
    }

    /// Return the number of ordered writes completed before the failure.
    pub const fn completed_writes(&self) -> usize {
        self.completed_writes
    }

    /// Return the backend failure without embedding timer register values.
    pub const fn backend_source(&self) -> &BackendError {
        &self.source
    }
}

impl fmt::Debug for HvfArm64SnapshotTimerRestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfArm64SnapshotTimerRestoreError")
            .field("operation", &self.operation)
            .field("completed_writes", &self.completed_writes)
            .field("source", &"<redacted>")
            .finish()
    }
}

impl fmt::Display for HvfArm64SnapshotTimerRestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "arm64 snapshot timer restore failed during {:?} after {} completed writes",
            self.operation, self.completed_writes
        )
    }
}

impl std::error::Error for HvfArm64SnapshotTimerRestoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HvfArm64SnapshotTimerRestoreValue {
    Bool(bool),
    U64(u64),
}

pub(crate) fn restore_arm64_snapshot_timer_state_with<C: ?Sized>(
    state: &HvfArm64SnapshotTimerState,
    context: &mut C,
    mut preflight: impl FnMut(&mut C, HvfArm64SnapshotTimerRestoreOperation) -> Result<(), BackendError>,
    sample_counter: impl FnOnce(&mut C) -> Result<u64, BackendError>,
    mut write: impl FnMut(
        &mut C,
        HvfArm64SnapshotTimerRestoreOperation,
        HvfArm64SnapshotTimerRestoreValue,
    ) -> Result<(), BackendError>,
) -> Result<(), HvfArm64SnapshotTimerRestoreError> {
    const PREFLIGHT: [HvfArm64SnapshotTimerRestoreOperation; 8] = [
        HvfArm64SnapshotTimerRestoreOperation::ReadCntkctlEl1,
        HvfArm64SnapshotTimerRestoreOperation::ReadPhysicalControl,
        HvfArm64SnapshotTimerRestoreOperation::ReadPhysicalCompareValue,
        HvfArm64SnapshotTimerRestoreOperation::ReadPhysicalTimerValue,
        HvfArm64SnapshotTimerRestoreOperation::ReadVirtualTimerExitMask,
        HvfArm64SnapshotTimerRestoreOperation::ReadVirtualTimerOffset,
        HvfArm64SnapshotTimerRestoreOperation::ReadVirtualControl,
        HvfArm64SnapshotTimerRestoreOperation::ReadVirtualCompareValue,
    ];

    for operation in PREFLIGHT {
        preflight(context, operation).map_err(|source| HvfArm64SnapshotTimerRestoreError {
            operation,
            completed_writes: 0,
            source,
        })?;
    }

    let counter_sample =
        sample_counter(context).map_err(|source| HvfArm64SnapshotTimerRestoreError {
            operation: HvfArm64SnapshotTimerRestoreOperation::SampleCounter,
            completed_writes: 0,
            source,
        })?;
    let adjusted_physical_compare = counter_sample.wrapping_add(state.physical_compare_delta());
    let adjusted_virtual_offset = counter_sample.wrapping_sub(state.virtual_count());
    let writes = [
        (
            HvfArm64SnapshotTimerRestoreOperation::MaskVirtualTimerExits,
            HvfArm64SnapshotTimerRestoreValue::Bool(true),
        ),
        (
            HvfArm64SnapshotTimerRestoreOperation::DisableVirtualTimer,
            HvfArm64SnapshotTimerRestoreValue::U64(0),
        ),
        (
            HvfArm64SnapshotTimerRestoreOperation::DisablePhysicalTimer,
            HvfArm64SnapshotTimerRestoreValue::U64(0),
        ),
        (
            HvfArm64SnapshotTimerRestoreOperation::WriteCntkctlEl1,
            HvfArm64SnapshotTimerRestoreValue::U64(state.cntkctl_el1()),
        ),
        (
            HvfArm64SnapshotTimerRestoreOperation::WritePhysicalCompareValue,
            HvfArm64SnapshotTimerRestoreValue::U64(adjusted_physical_compare),
        ),
        (
            HvfArm64SnapshotTimerRestoreOperation::WriteVirtualTimerOffset,
            HvfArm64SnapshotTimerRestoreValue::U64(adjusted_virtual_offset),
        ),
        (
            HvfArm64SnapshotTimerRestoreOperation::WriteVirtualCompareValue,
            HvfArm64SnapshotTimerRestoreValue::U64(state.virtual_compare_value()),
        ),
        (
            HvfArm64SnapshotTimerRestoreOperation::RestorePhysicalControl,
            HvfArm64SnapshotTimerRestoreValue::U64(state.physical_control()),
        ),
        (
            HvfArm64SnapshotTimerRestoreOperation::RestoreVirtualControl,
            HvfArm64SnapshotTimerRestoreValue::U64(state.virtual_control()),
        ),
        (
            HvfArm64SnapshotTimerRestoreOperation::RestoreVirtualTimerExitMask,
            HvfArm64SnapshotTimerRestoreValue::Bool(state.virtual_timer_exit_masked()),
        ),
    ];

    for (completed_writes, (operation, value)) in writes.into_iter().enumerate() {
        write(context, operation, value).map_err(|source| HvfArm64SnapshotTimerRestoreError {
            operation,
            completed_writes,
            source,
        })?;
    }

    Ok(())
}

/// Native-v1 optional arm64 state that cannot be restored safely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfArm64SnapshotOptionalStateRejection {
    /// `CPACR_EL1.ZEN` permits SVE access without a complete SVE restore model.
    SveAccessEnabled,
    /// `CPACR_EL1.SMEN` permits SME access without a complete SME restore model.
    SmeAccessEnabled,
    /// `PSTATE.SM` indicates active streaming SVE mode.
    StreamingSveModeEnabled,
    /// `PSTATE.ZA` indicates enabled ZA storage.
    ZaStorageEnabled,
    /// At least one implemented hardware breakpoint is enabled.
    BreakpointEnabled,
    /// At least one implemented hardware watchpoint is enabled.
    WatchpointEnabled,
}

impl fmt::Display for HvfArm64SnapshotOptionalStateRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let category = match self {
            Self::SveAccessEnabled => "SVE access",
            Self::SmeAccessEnabled => "SME access",
            Self::StreamingSveModeEnabled => "streaming SVE mode",
            Self::ZaStorageEnabled => "ZA storage",
            Self::BreakpointEnabled => "hardware breakpoint",
            Self::WatchpointEnabled => "hardware watchpoint",
        };
        write!(
            f,
            "native-v1 snapshot restore rejects active {category} state"
        )
    }
}

impl std::error::Error for HvfArm64SnapshotOptionalStateRejection {}

/// Fail closed when native-v1 cannot faithfully restore optional arm64 state.
///
/// Rejections are deterministic and category-only: no register value, guest
/// address, comparator slot, or feature identifier is retained in the error.
/// Callers must supply each capture when its optional family is implemented;
/// `None` means compatibility validation already proved that family absent,
/// not that capture was skipped.
pub fn validate_native_v1_arm64_snapshot_optional_state(
    execution_controls: HvfArm64VcpuExecutionControlRegisterState,
    sme_pstate: Option<HvfArm64VcpuSmePstate>,
    breakpoints: Option<&HvfArm64VcpuBreakpointRegisterState>,
    watchpoints: Option<&HvfArm64VcpuWatchpointRegisterState>,
) -> Result<(), HvfArm64SnapshotOptionalStateRejection> {
    if execution_controls.cpacr_el1() & CPACR_EL1_ZEN_MASK != 0 {
        return Err(HvfArm64SnapshotOptionalStateRejection::SveAccessEnabled);
    }
    if execution_controls.cpacr_el1() & CPACR_EL1_SMEN_MASK != 0 {
        return Err(HvfArm64SnapshotOptionalStateRejection::SmeAccessEnabled);
    }
    if let Some(pstate) = sme_pstate {
        if pstate.streaming_sve_mode_enabled() {
            return Err(HvfArm64SnapshotOptionalStateRejection::StreamingSveModeEnabled);
        }
        if pstate.za_storage_enabled() {
            return Err(HvfArm64SnapshotOptionalStateRejection::ZaStorageEnabled);
        }
    }
    if breakpoints.is_some_and(|state| {
        state
            .breakpoint_control_registers()
            .iter()
            .any(|control| control & DEBUG_CONTROL_ENABLE != 0)
    }) {
        return Err(HvfArm64SnapshotOptionalStateRejection::BreakpointEnabled);
    }
    if watchpoints.is_some_and(|state| {
        state
            .watchpoint_control_registers()
            .iter()
            .any(|control| control & DEBUG_CONTROL_ENABLE != 0)
    }) {
        return Err(HvfArm64SnapshotOptionalStateRejection::WatchpointEnabled);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::*;

    fn physical(control: u64, compare: u64, timer_value: u64) -> HvfArm64VcpuPhysicalTimerState {
        HvfArm64VcpuPhysicalTimerState::new(0x1234, control, compare, timer_value)
    }

    fn virtual_timer(
        masked: bool,
        offset: u64,
        control: u64,
        compare: u64,
    ) -> HvfArm64VcpuVirtualTimerState {
        HvfArm64VcpuVirtualTimerState::new(masked, offset, control, compare)
    }

    #[test]
    fn normalization_freezes_both_timer_domains_and_strips_derived_state() {
        let state = normalize_arm64_snapshot_timer_state(
            physical(0b111, 1_250, 0xfeed_face),
            virtual_timer(true, 600, 0b101, 900),
            1_000,
        )
        .unwrap();

        assert!(state.virtual_timer_exit_masked());
        assert_eq!(state.cntkctl_el1(), 0x1234);
        assert_eq!(state.virtual_count(), 400);
        assert_eq!(state.virtual_control(), TIMER_CONTROL_ENABLE);
        assert_eq!(state.virtual_compare_value(), 900);
        assert_eq!(state.physical_control(), TIMER_CONTROL_WRITABLE_BITS);
        assert_eq!(state.physical_compare_delta(), 250);
    }

    #[test]
    fn normalization_uses_wrapping_counter_arithmetic() {
        let state = normalize_arm64_snapshot_timer_state(
            physical(0, 2, 0),
            virtual_timer(false, u64::MAX, 0, 0),
            1,
        )
        .unwrap();

        assert_eq!(state.virtual_count(), 2);
        assert_eq!(state.physical_compare_delta(), 1);
    }

    #[test]
    fn normalization_preserves_expired_comparator_and_ignores_tval() {
        let first = normalize_arm64_snapshot_timer_state(
            physical(0, 900, 0),
            virtual_timer(false, 600, 0, 700),
            1_000,
        )
        .unwrap();
        let second = normalize_arm64_snapshot_timer_state(
            physical(0, 900, u64::MAX),
            virtual_timer(false, 600, 0, 700),
            1_000,
        )
        .unwrap();

        assert_eq!(first, second);
        assert_eq!(first.physical_compare_delta(), 900u64.wrapping_sub(1_000));
        assert_eq!(first.physical_control(), 0);
        assert_eq!(first.virtual_control(), 0);
    }

    #[test]
    fn normalization_rejects_unknown_virtual_control_bits_first() {
        assert_eq!(
            normalize_arm64_snapshot_timer_state(
                physical(1 << 9, 0, 0),
                virtual_timer(false, 0, 1 << 8, 0),
                0,
            ),
            Err(HvfArm64SnapshotTimerPolicyError::VirtualTimerControl)
        );
    }

    #[test]
    fn normalization_rejects_unknown_physical_control_bits() {
        assert_eq!(
            normalize_arm64_snapshot_timer_state(
                physical(1 << 9, 0, 0),
                virtual_timer(false, 0, 0, 0),
                0,
            ),
            Err(HvfArm64SnapshotTimerPolicyError::PhysicalTimerControl)
        );
    }

    #[test]
    fn normalized_constructor_rejects_derived_or_unknown_control_bits() {
        let args = (false, 0, 0, 0, 0, 0, 0);
        assert_eq!(
            HvfArm64SnapshotTimerState::try_new(args.0, args.1, args.2, 0b100, args.4, 0, args.6),
            Err(HvfArm64SnapshotTimerPolicyError::VirtualTimerControl)
        );
        assert_eq!(
            HvfArm64SnapshotTimerState::try_new(args.0, args.1, args.2, 0, args.4, 0b100, args.6),
            Err(HvfArm64SnapshotTimerPolicyError::PhysicalTimerControl)
        );
    }

    #[derive(Default)]
    struct RestoreContext {
        calls: Vec<(HvfArm64SnapshotTimerRestoreOperation, Option<u64>)>,
        fail: Option<HvfArm64SnapshotTimerRestoreOperation>,
        sample: u64,
    }

    fn restore_state() -> HvfArm64SnapshotTimerState {
        HvfArm64SnapshotTimerState::try_new(true, 7, 800, 1, 1_100, 2, 250).unwrap()
    }

    fn restore(context: &mut RestoreContext) -> Result<(), HvfArm64SnapshotTimerRestoreError> {
        restore_arm64_snapshot_timer_state_with(
            &restore_state(),
            context,
            |context, operation| {
                context.calls.push((operation, None));
                if context.fail == Some(operation) {
                    Err(BackendError::Hypervisor("sensitive-preflight".into()))
                } else {
                    Ok(())
                }
            },
            |context| {
                let operation = HvfArm64SnapshotTimerRestoreOperation::SampleCounter;
                context.calls.push((operation, None));
                if context.fail == Some(operation) {
                    Err(BackendError::Hypervisor("sensitive-sample".into()))
                } else {
                    Ok(context.sample)
                }
            },
            |context, operation, value| {
                let value = match value {
                    HvfArm64SnapshotTimerRestoreValue::Bool(value) => u64::from(value),
                    HvfArm64SnapshotTimerRestoreValue::U64(value) => value,
                };
                context.calls.push((operation, Some(value)));
                if context.fail == Some(operation) {
                    Err(BackendError::Hypervisor("sensitive-write".into()))
                } else {
                    Ok(())
                }
            },
        )
    }

    #[test]
    fn restore_preflights_every_field_then_writes_in_safe_order() {
        let mut context = RestoreContext {
            sample: 1_000,
            ..RestoreContext::default()
        };

        restore(&mut context).unwrap();

        assert_eq!(context.calls.len(), 19);
        assert_eq!(
            context.calls.get(8),
            Some(&(HvfArm64SnapshotTimerRestoreOperation::SampleCounter, None))
        );
        assert_eq!(
            context.calls.get(9),
            Some(&(
                HvfArm64SnapshotTimerRestoreOperation::MaskVirtualTimerExits,
                Some(1)
            ))
        );
        assert_eq!(
            context.calls.get(13),
            Some(&(
                HvfArm64SnapshotTimerRestoreOperation::WritePhysicalCompareValue,
                Some(1_250)
            ))
        );
        assert_eq!(
            context.calls.get(14),
            Some(&(
                HvfArm64SnapshotTimerRestoreOperation::WriteVirtualTimerOffset,
                Some(200)
            ))
        );
        assert_eq!(
            context.calls.last(),
            Some(&(
                HvfArm64SnapshotTimerRestoreOperation::RestoreVirtualTimerExitMask,
                Some(1)
            ))
        );
    }

    #[test]
    fn restore_uses_wrapping_counter_arithmetic() {
        let state = HvfArm64SnapshotTimerState::try_new(false, 0, 2, 0, 0, 0, u64::MAX).unwrap();
        let mut context = RestoreContext {
            sample: 1,
            ..RestoreContext::default()
        };

        restore_arm64_snapshot_timer_state_with(
            &state,
            &mut context,
            |_context, _operation| Ok(()),
            |context| Ok(context.sample),
            |context, operation, value| {
                let value = match value {
                    HvfArm64SnapshotTimerRestoreValue::Bool(value) => u64::from(value),
                    HvfArm64SnapshotTimerRestoreValue::U64(value) => value,
                };
                context.calls.push((operation, Some(value)));
                Ok(())
            },
        )
        .unwrap();

        assert!(context.calls.contains(&(
            HvfArm64SnapshotTimerRestoreOperation::WritePhysicalCompareValue,
            Some(0),
        )));
        assert!(context.calls.contains(&(
            HvfArm64SnapshotTimerRestoreOperation::WriteVirtualTimerOffset,
            Some(u64::MAX),
        )));
    }

    #[test]
    fn every_preflight_failure_reports_zero_writes() {
        let operations = [
            HvfArm64SnapshotTimerRestoreOperation::ReadCntkctlEl1,
            HvfArm64SnapshotTimerRestoreOperation::ReadPhysicalControl,
            HvfArm64SnapshotTimerRestoreOperation::ReadPhysicalCompareValue,
            HvfArm64SnapshotTimerRestoreOperation::ReadPhysicalTimerValue,
            HvfArm64SnapshotTimerRestoreOperation::ReadVirtualTimerExitMask,
            HvfArm64SnapshotTimerRestoreOperation::ReadVirtualTimerOffset,
            HvfArm64SnapshotTimerRestoreOperation::ReadVirtualControl,
            HvfArm64SnapshotTimerRestoreOperation::ReadVirtualCompareValue,
            HvfArm64SnapshotTimerRestoreOperation::SampleCounter,
        ];

        for operation in operations {
            let mut context = RestoreContext {
                fail: Some(operation),
                ..RestoreContext::default()
            };
            let error = restore(&mut context).unwrap_err();
            assert_eq!(error.operation(), operation);
            assert_eq!(error.completed_writes(), 0);
        }
    }

    #[test]
    fn every_write_failure_reports_completed_prefix() {
        let operations = [
            HvfArm64SnapshotTimerRestoreOperation::MaskVirtualTimerExits,
            HvfArm64SnapshotTimerRestoreOperation::DisableVirtualTimer,
            HvfArm64SnapshotTimerRestoreOperation::DisablePhysicalTimer,
            HvfArm64SnapshotTimerRestoreOperation::WriteCntkctlEl1,
            HvfArm64SnapshotTimerRestoreOperation::WritePhysicalCompareValue,
            HvfArm64SnapshotTimerRestoreOperation::WriteVirtualTimerOffset,
            HvfArm64SnapshotTimerRestoreOperation::WriteVirtualCompareValue,
            HvfArm64SnapshotTimerRestoreOperation::RestorePhysicalControl,
            HvfArm64SnapshotTimerRestoreOperation::RestoreVirtualControl,
            HvfArm64SnapshotTimerRestoreOperation::RestoreVirtualTimerExitMask,
        ];

        for (completed, operation) in operations.into_iter().enumerate() {
            let mut context = RestoreContext {
                fail: Some(operation),
                ..RestoreContext::default()
            };
            let error = restore(&mut context).unwrap_err();
            assert_eq!(error.operation(), operation);
            assert_eq!(error.completed_writes(), completed);
        }
    }

    #[test]
    fn restore_error_formatting_redacts_backend_details() {
        let mut context = RestoreContext {
            fail: Some(HvfArm64SnapshotTimerRestoreOperation::DisablePhysicalTimer),
            ..RestoreContext::default()
        };
        let error = restore(&mut context).unwrap_err();

        assert!(!format!("{error:?}").contains("sensitive"));
        assert!(!error.to_string().contains("sensitive"));
        assert!(error.source().is_some());
    }

    fn execution(cpacr_el1: u64) -> HvfArm64VcpuExecutionControlRegisterState {
        HvfArm64VcpuExecutionControlRegisterState::new(0, cpacr_el1)
    }

    fn breakpoints(controls: [u64; 16], count: u8) -> HvfArm64VcpuBreakpointRegisterState {
        HvfArm64VcpuBreakpointRegisterState::new(count, [0; 16], controls)
    }

    fn watchpoints(controls: [u64; 16], count: u8) -> HvfArm64VcpuWatchpointRegisterState {
        HvfArm64VcpuWatchpointRegisterState::new(count, [0; 16], controls)
    }

    #[test]
    fn optional_state_classifier_accepts_inactive_or_absent_state() {
        let breakpoints = breakpoints([0; 16], 16);
        let watchpoints = watchpoints([0; 16], 16);
        assert_eq!(
            validate_native_v1_arm64_snapshot_optional_state(
                execution(0),
                Some(HvfArm64VcpuSmePstate::new(false, false)),
                Some(&breakpoints),
                Some(&watchpoints),
            ),
            Ok(())
        );
        assert_eq!(
            validate_native_v1_arm64_snapshot_optional_state(execution(0), None, None, None),
            Ok(())
        );
    }

    #[test]
    fn optional_state_classifier_rejects_each_active_category() {
        let mut enabled = [0; 16];
        if let Some(control) = enabled.get_mut(3) {
            *control = 1;
        }
        let breakpoints = breakpoints(enabled, 4);
        let watchpoints = watchpoints(enabled, 4);
        let cases = [
            (
                validate_native_v1_arm64_snapshot_optional_state(
                    execution(CPACR_EL1_ZEN_MASK),
                    None,
                    None,
                    None,
                ),
                HvfArm64SnapshotOptionalStateRejection::SveAccessEnabled,
            ),
            (
                validate_native_v1_arm64_snapshot_optional_state(
                    execution(CPACR_EL1_SMEN_MASK),
                    None,
                    None,
                    None,
                ),
                HvfArm64SnapshotOptionalStateRejection::SmeAccessEnabled,
            ),
            (
                validate_native_v1_arm64_snapshot_optional_state(
                    execution(0),
                    Some(HvfArm64VcpuSmePstate::new(true, false)),
                    None,
                    None,
                ),
                HvfArm64SnapshotOptionalStateRejection::StreamingSveModeEnabled,
            ),
            (
                validate_native_v1_arm64_snapshot_optional_state(
                    execution(0),
                    Some(HvfArm64VcpuSmePstate::new(false, true)),
                    None,
                    None,
                ),
                HvfArm64SnapshotOptionalStateRejection::ZaStorageEnabled,
            ),
            (
                validate_native_v1_arm64_snapshot_optional_state(
                    execution(0),
                    None,
                    Some(&breakpoints),
                    None,
                ),
                HvfArm64SnapshotOptionalStateRejection::BreakpointEnabled,
            ),
            (
                validate_native_v1_arm64_snapshot_optional_state(
                    execution(0),
                    None,
                    None,
                    Some(&watchpoints),
                ),
                HvfArm64SnapshotOptionalStateRejection::WatchpointEnabled,
            ),
        ];

        for (actual, expected) in cases {
            assert_eq!(actual, Err(expected));
        }
    }

    #[test]
    fn optional_state_classifier_uses_fail_closed_precedence_and_redaction() {
        let mut controls = [0; 16];
        if let Some(control) = controls.first_mut() {
            *control = 1;
        }
        let breakpoints = breakpoints(controls, 1);
        let error = validate_native_v1_arm64_snapshot_optional_state(
            execution(CPACR_EL1_ZEN_MASK | CPACR_EL1_SMEN_MASK),
            Some(HvfArm64VcpuSmePstate::new(true, true)),
            Some(&breakpoints),
            None,
        )
        .unwrap_err();

        assert_eq!(
            error,
            HvfArm64SnapshotOptionalStateRejection::SveAccessEnabled
        );
        assert!(!format!("{error:?}").contains("0x"));
        assert!(!error.to_string().contains('0'));
    }

    #[test]
    fn timer_state_debug_is_redacted() {
        let state = HvfArm64SnapshotTimerState::try_new(
            true,
            0xfeed_face,
            0xdead_beef,
            1,
            0xcafe_babe,
            2,
            0x1234_5678,
        )
        .unwrap();
        let debug = format!("{state:?}");

        assert!(!debug.contains("feed"));
        assert!(!debug.contains("dead"));
        assert!(!debug.contains("cafe"));
        assert!(!debug.contains("1234"));
    }
}
