//! PSCI-over-HVC decoding and secondary-vCPU power-state coordination.

use std::fmt;

const PSCI_VERSION: u64 = 0x8400_0000;
const PSCI_CPU_SUSPEND_32: u64 = 0x8400_0001;
const PSCI_CPU_OFF: u64 = 0x8400_0002;
const PSCI_CPU_ON_32: u64 = 0x8400_0003;
const PSCI_AFFINITY_INFO_32: u64 = 0x8400_0004;
const PSCI_MIGRATE_INFO_TYPE: u64 = 0x8400_0006;
const PSCI_SYSTEM_OFF: u64 = 0x8400_0008;
const PSCI_SYSTEM_RESET: u64 = 0x8400_0009;
const PSCI_FEATURES: u64 = 0x8400_000a;
const PSCI_CPU_SUSPEND_64: u64 = 0xc400_0001;
const PSCI_CPU_ON_64: u64 = 0xc400_0003;
const PSCI_AFFINITY_INFO_64: u64 = 0xc400_0004;
const PSCI_VERSION_0_2: u64 = 0x0000_0002;
const PSCI_MIGRATE_INFO_TYPE_TRUSTED_OS_NOT_REQUIRED: u64 = 2;
const PSCI_MPIDR_AFFINITY_MASK: u64 = 0x0000_00ff_00ff_ffff;
const PSCI_MPIDR_32_RESERVED_MASK: u64 = 0xff00_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PsciCall {
    function_id: u64,
    arg0: u64,
    arg1: u64,
    arg2: u64,
}

impl PsciCall {
    pub(crate) const fn new(function_id: u64, arg0: u64) -> Self {
        Self::from_arguments(function_id, [arg0, 0, 0])
    }

    pub(crate) const fn from_arguments(function_id: u64, arguments: [u64; 3]) -> Self {
        let [arg0, arg1, arg2] = arguments;
        Self {
            function_id,
            arg0,
            arg1,
            arg2,
        }
    }
}

pub(crate) const fn call_uses_arg0(function_id: u64) -> bool {
    matches!(function_id, PSCI_FEATURES)
}

pub(crate) const fn coordinated_call_argument_count(function_id: u64) -> usize {
    match function_id {
        PSCI_CPU_SUSPEND_32 | PSCI_CPU_SUSPEND_64 | PSCI_CPU_ON_32 | PSCI_CPU_ON_64 => 3,
        PSCI_AFFINITY_INFO_32 | PSCI_AFFINITY_INFO_64 => 2,
        PSCI_FEATURES => 1,
        _ => 0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PsciStatus {
    Success,
    NotSupported,
    InvalidParameters,
    Denied,
    AlreadyOn,
    OnPending,
    InternalFailure,
}

impl PsciStatus {
    pub(crate) const fn return_value(self) -> u64 {
        let signed = match self {
            Self::Success => 0_i32,
            Self::NotSupported => -1,
            Self::InvalidParameters => -2,
            Self::Denied => -3,
            Self::AlreadyOn => -4,
            Self::OnPending => -5,
            Self::InternalFailure => -6,
        };

        (signed as u32) as u64
    }
}

pub(crate) const fn not_supported_result() -> PsciCallResult {
    PsciCallResult::returned(PsciStatus::NotSupported.return_value())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PsciCallAction {
    Return,
    SystemOff,
    SystemReset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PsciCallResult {
    return_value: u64,
    action: PsciCallAction,
}

impl PsciCallResult {
    const fn returned(return_value: u64) -> Self {
        Self {
            return_value,
            action: PsciCallAction::Return,
        }
    }

    const fn terminal(return_value: u64, action: PsciCallAction) -> Self {
        Self {
            return_value,
            action,
        }
    }

    pub(crate) const fn return_value(self) -> u64 {
        self.return_value
    }

    pub(crate) const fn action(self) -> PsciCallAction {
        self.action
    }
}

pub(crate) const fn handle_call(call: PsciCall) -> PsciCallResult {
    match call.function_id {
        PSCI_VERSION => PsciCallResult::returned(PSCI_VERSION_0_2),
        PSCI_MIGRATE_INFO_TYPE => {
            PsciCallResult::returned(PSCI_MIGRATE_INFO_TYPE_TRUSTED_OS_NOT_REQUIRED)
        }
        PSCI_SYSTEM_OFF => PsciCallResult::terminal(
            PsciStatus::Success.return_value(),
            PsciCallAction::SystemOff,
        ),
        PSCI_SYSTEM_RESET => PsciCallResult::terminal(
            PsciStatus::Success.return_value(),
            PsciCallAction::SystemReset,
        ),
        PSCI_FEATURES => {
            let status = if supports_legacy_function(call.arg0) {
                PsciStatus::Success
            } else {
                PsciStatus::NotSupported
            };
            PsciCallResult::returned(status.return_value())
        }
        PSCI_CPU_OFF => not_supported_result(),
        _ => not_supported_result(),
    }
}

const fn supports_legacy_function(function_id: u64) -> bool {
    matches!(
        function_id,
        PSCI_VERSION | PSCI_MIGRATE_INFO_TYPE | PSCI_SYSTEM_OFF | PSCI_SYSTEM_RESET | PSCI_FEATURES
    )
}

const fn supports_coordinated_function(function_id: u64) -> bool {
    supports_legacy_function(function_id)
        || matches!(
            function_id,
            PSCI_CPU_SUSPEND_32
                | PSCI_CPU_SUSPEND_64
                | PSCI_CPU_OFF
                | PSCI_CPU_ON_32
                | PSCI_CPU_ON_64
                | PSCI_AFFINITY_INFO_32
                | PSCI_AFFINITY_INFO_64
        )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PsciCallingConvention {
    Smc32,
    Smc64,
}

impl PsciCallingConvention {
    const fn narrow(self, value: u64) -> u64 {
        match self {
            Self::Smc32 => (value as u32) as u64,
            Self::Smc64 => value,
        }
    }

    const fn target_mpidr(self, value: u64) -> Option<u64> {
        let value = self.narrow(value);
        let has_reserved_bits = match self {
            Self::Smc32 => value & PSCI_MPIDR_32_RESERVED_MASK != 0,
            Self::Smc64 => value & !PSCI_MPIDR_AFFINITY_MASK != 0,
        };
        if has_reserved_bits { None } else { Some(value) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PsciCpuOnRequest {
    target_mpidr: u64,
    entry_point: u64,
    context_id: u64,
}

impl PsciCpuOnRequest {
    #[cfg(test)]
    const fn new(target_mpidr: u64, entry_point: u64, context_id: u64) -> Self {
        Self {
            target_mpidr,
            entry_point,
            context_id,
        }
    }

    #[cfg(test)]
    pub(crate) const fn target_mpidr(self) -> u64 {
        self.target_mpidr
    }

    pub(crate) const fn entry_point(self) -> u64 {
        self.entry_point
    }

    pub(crate) const fn context_id(self) -> u64 {
        self.context_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PsciAffinityInfoRequest {
    target_mpidr: u64,
    lowest_affinity_level: u64,
}

#[cfg(test)]
impl PsciAffinityInfoRequest {
    const fn new(target_mpidr: u64, lowest_affinity_level: u64) -> Self {
        Self {
            target_mpidr,
            lowest_affinity_level,
        }
    }

    pub(crate) const fn target_mpidr(self) -> u64 {
        self.target_mpidr
    }

    pub(crate) const fn lowest_affinity_level(self) -> u64 {
        self.lowest_affinity_level
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PsciCoordinatorRequest {
    CpuSuspend,
    CpuOff,
    CpuOn(PsciCpuOnRequest),
    AffinityInfo(PsciAffinityInfoRequest),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PsciCoordinatorResponse {
    CpuSuspend(PsciCpuSuspendResponse),
    CpuOff(PsciCpuOffResponse),
    CpuOn(PsciCpuOnResponse),
    AffinityInfo(PsciAffinityInfoResponse),
}

impl PsciCoordinatorResponse {
    pub(crate) const fn return_value(self) -> u64 {
        match self {
            Self::CpuSuspend(response) => response.status().return_value(),
            Self::CpuOff(response) => response.status().return_value(),
            Self::CpuOn(response) => response.status().return_value(),
            Self::AffinityInfo(response) => response.return_value(),
        }
    }
}

pub(crate) const fn response_matches_request(
    request: PsciCoordinatorRequest,
    response: PsciCoordinatorResponse,
) -> bool {
    matches!(
        (request, response),
        (
            PsciCoordinatorRequest::CpuSuspend,
            PsciCoordinatorResponse::CpuSuspend(_)
        ) | (
            PsciCoordinatorRequest::CpuOff,
            PsciCoordinatorResponse::CpuOff(_)
        ) | (
            PsciCoordinatorRequest::CpuOn(_),
            PsciCoordinatorResponse::CpuOn(_)
        ) | (
            PsciCoordinatorRequest::AffinityInfo(_),
            PsciCoordinatorResponse::AffinityInfo(_)
        )
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PsciCoordinatedDispatch {
    Immediate(PsciCallResult),
    Coordinate(PsciCoordinatorRequest),
}

pub(crate) const fn handle_coordinated_call(call: PsciCall) -> PsciCoordinatedDispatch {
    if call.function_id == PSCI_FEATURES {
        let status = if supports_coordinated_function(call.arg0) {
            PsciStatus::Success
        } else {
            PsciStatus::NotSupported
        };
        return PsciCoordinatedDispatch::Immediate(PsciCallResult::returned(status.return_value()));
    }

    if call.function_id == PSCI_CPU_OFF {
        return PsciCoordinatedDispatch::Coordinate(PsciCoordinatorRequest::CpuOff);
    }

    let convention = match call.function_id {
        PSCI_CPU_SUSPEND_32 | PSCI_CPU_ON_32 | PSCI_AFFINITY_INFO_32 => {
            Some(PsciCallingConvention::Smc32)
        }
        PSCI_CPU_SUSPEND_64 | PSCI_CPU_ON_64 | PSCI_AFFINITY_INFO_64 => {
            Some(PsciCallingConvention::Smc64)
        }
        _ => None,
    };

    match (call.function_id, convention) {
        (PSCI_CPU_SUSPEND_32 | PSCI_CPU_SUSPEND_64, Some(convention)) => {
            // KVM v5.15 deliberately downgrades every request to retained
            // standby. Consume the ABI-width arguments without retaining or
            // interpreting guest power-state, entry, or context values.
            let _ = [
                convention.narrow(call.arg0),
                convention.narrow(call.arg1),
                convention.narrow(call.arg2),
            ];
            PsciCoordinatedDispatch::Coordinate(PsciCoordinatorRequest::CpuSuspend)
        }
        (PSCI_CPU_ON_32 | PSCI_CPU_ON_64, Some(convention)) => {
            let Some(target_mpidr) = convention.target_mpidr(call.arg0) else {
                return PsciCoordinatedDispatch::Immediate(PsciCallResult::returned(
                    PsciStatus::InvalidParameters.return_value(),
                ));
            };
            PsciCoordinatedDispatch::Coordinate(PsciCoordinatorRequest::CpuOn(PsciCpuOnRequest {
                target_mpidr,
                entry_point: convention.narrow(call.arg1),
                context_id: convention.narrow(call.arg2),
            }))
        }
        (PSCI_AFFINITY_INFO_32 | PSCI_AFFINITY_INFO_64, Some(convention)) => {
            let Some(target_mpidr) = convention.target_mpidr(call.arg0) else {
                return PsciCoordinatedDispatch::Immediate(PsciCallResult::returned(
                    PsciStatus::InvalidParameters.return_value(),
                ));
            };
            PsciCoordinatedDispatch::Coordinate(PsciCoordinatorRequest::AffinityInfo(
                PsciAffinityInfoRequest {
                    target_mpidr,
                    lowest_affinity_level: convention.narrow(call.arg1),
                },
            ))
        }
        _ => PsciCoordinatedDispatch::Immediate(handle_call(call)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PsciCpuPowerState {
    On,
    Off,
    OnPending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PsciCpuSuspendResponse {
    Success,
}

impl PsciCpuSuspendResponse {
    pub(crate) const fn status(self) -> PsciStatus {
        match self {
            Self::Success => PsciStatus::Success,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PsciCpuSuspendToken(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PsciCpuSuspendWork {
    token: PsciCpuSuspendToken,
    caller_index: usize,
}

impl PsciCpuSuspendWork {
    pub(crate) const fn token(self) -> PsciCpuSuspendToken {
        self.token
    }

    pub(crate) const fn caller_index(self) -> usize {
        self.caller_index
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PsciCpuOffResponse {
    Denied,
    InternalFailure,
}

impl PsciCpuOffResponse {
    pub(crate) const fn status(self) -> PsciStatus {
        match self {
            Self::Denied => PsciStatus::Denied,
            Self::InternalFailure => PsciStatus::InternalFailure,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PsciCpuOffToken(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PsciCpuOffWork {
    token: PsciCpuOffToken,
    caller_index: usize,
}

impl PsciCpuOffWork {
    pub(crate) const fn token(self) -> PsciCpuOffToken {
        self.token
    }

    pub(crate) const fn caller_index(self) -> usize {
        self.caller_index
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PsciCpuOffBegin {
    Complete(PsciCpuOffResponse),
    Pending(PsciCpuOffWork),
}

impl PsciCpuPowerState {
    const fn affinity_info_value(self) -> u64 {
        match self {
            Self::On => 0,
            Self::Off => 1,
            Self::OnPending => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PsciCpuOnResponse {
    Success,
    InvalidTarget,
    InvalidAddress,
    AlreadyOn,
    OnPending,
    #[cfg(test)]
    Unsupported,
    InternalFailure,
}

impl PsciCpuOnResponse {
    pub(crate) const fn status(self) -> PsciStatus {
        match self {
            Self::Success => PsciStatus::Success,
            Self::InvalidTarget | Self::InvalidAddress => PsciStatus::InvalidParameters,
            Self::AlreadyOn => PsciStatus::AlreadyOn,
            Self::OnPending => PsciStatus::OnPending,
            #[cfg(test)]
            Self::Unsupported => PsciStatus::NotSupported,
            Self::InternalFailure => PsciStatus::InternalFailure,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PsciCpuOnToken(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PsciCpuOnWork {
    token: PsciCpuOnToken,
    target_index: usize,
    request: PsciCpuOnRequest,
}

impl PsciCpuOnWork {
    pub(crate) const fn token(self) -> PsciCpuOnToken {
        self.token
    }

    pub(crate) const fn target_index(self) -> usize {
        self.target_index
    }

    pub(crate) const fn request(self) -> PsciCpuOnRequest {
        self.request
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PsciCpuOnBegin {
    Complete(PsciCpuOnResponse),
    Pending(PsciCpuOnWork),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PsciAffinityInfoResponse {
    State(PsciCpuPowerState),
    InvalidTarget,
    InvalidLevel,
}

impl PsciAffinityInfoResponse {
    pub(crate) const fn return_value(self) -> u64 {
        match self {
            Self::State(state) => state.affinity_info_value(),
            Self::InvalidTarget | Self::InvalidLevel => {
                PsciStatus::InvalidParameters.return_value()
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PsciCpuPowerError {
    InvalidTopology,
    DuplicateMpidr { mpidr: u64 },
    InvalidMpidr { mpidr: u64 },
    TokenExhausted,
    UnknownTransaction { token: PsciCpuOnToken },
    InvalidTransactionPhase { token: PsciCpuOnToken },
    InvalidCpuIndex { index: usize },
    CpuSuspendUnavailable { index: usize },
    UnknownCpuSuspendTransaction { token: PsciCpuSuspendToken },
    UnknownCpuOffTransaction { token: PsciCpuOffToken },
}

impl fmt::Display for PsciCpuPowerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTopology => f.write_str("PSCI CPU power topology is empty"),
            Self::DuplicateMpidr { mpidr } => {
                write!(f, "PSCI CPU power topology repeats MPIDR 0x{mpidr:x}")
            }
            Self::InvalidMpidr { mpidr } => {
                write!(f, "PSCI CPU power topology has invalid MPIDR 0x{mpidr:x}")
            }
            Self::TokenExhausted => f.write_str("PSCI CPU power transaction tokens are exhausted"),
            Self::UnknownTransaction { token } => {
                write!(f, "PSCI CPU_ON transaction {token:?} is unknown")
            }
            Self::InvalidTransactionPhase { token } => {
                write!(f, "PSCI CPU_ON transaction {token:?} is in the wrong phase")
            }
            Self::InvalidCpuIndex { index } => {
                write!(f, "PSCI CPU power topology has no vCPU index {index}")
            }
            Self::CpuSuspendUnavailable { index } => {
                write!(f, "PSCI CPU_SUSPEND cannot reserve vCPU index {index}")
            }
            Self::UnknownCpuSuspendTransaction { token } => {
                write!(f, "PSCI CPU_SUSPEND transaction {token:?} is unknown")
            }
            Self::UnknownCpuOffTransaction { token } => {
                write!(f, "PSCI CPU_OFF transaction {token:?} is unknown")
            }
        }
    }
}

impl std::error::Error for PsciCpuPowerError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PsciCpuOnPhase {
    AwaitingTargetSetup,
    AwaitingCallerCompletion {
        response: PsciCpuOnResponse,
        target_configured: bool,
    },
    CallerCompleted,
    CallerAbandoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PsciCpuOnTransaction {
    work: PsciCpuOnWork,
    phase: PsciCpuOnPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PsciCpuState {
    mpidr: u64,
    power: PsciCpuPowerState,
    transaction: Option<PsciCpuOnTransaction>,
    cpu_suspend_transaction: Option<PsciCpuSuspendWork>,
    cpu_off_transaction: Option<PsciCpuOffWork>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PsciCpuPowerCoordinator {
    cpus: Vec<PsciCpuState>,
    next_token: u64,
}

impl PsciCpuPowerCoordinator {
    pub(crate) fn new(mpidrs: &[u64]) -> Result<Self, PsciCpuPowerError> {
        if mpidrs.is_empty() {
            return Err(PsciCpuPowerError::InvalidTopology);
        }

        let mut cpus = Vec::new();
        cpus.try_reserve_exact(mpidrs.len())
            .map_err(|_| PsciCpuPowerError::InvalidTopology)?;
        for (index, mpidr) in mpidrs.iter().copied().enumerate() {
            if mpidr & !PSCI_MPIDR_AFFINITY_MASK != 0 {
                return Err(PsciCpuPowerError::InvalidMpidr { mpidr });
            }
            if cpus.iter().any(|cpu: &PsciCpuState| cpu.mpidr == mpidr) {
                return Err(PsciCpuPowerError::DuplicateMpidr { mpidr });
            }
            cpus.push(PsciCpuState {
                mpidr,
                power: if index == 0 {
                    PsciCpuPowerState::On
                } else {
                    PsciCpuPowerState::Off
                },
                transaction: None,
                cpu_suspend_transaction: None,
                cpu_off_transaction: None,
            });
        }

        Ok(Self {
            cpus,
            next_token: 1,
        })
    }

    #[cfg(test)]
    pub(crate) fn power_state(&self, index: usize) -> Option<PsciCpuPowerState> {
        self.cpus.get(index).map(|cpu| cpu.power)
    }

    pub(crate) fn begin_cpu_on(
        &mut self,
        request: PsciCpuOnRequest,
        entry_is_valid: impl FnOnce(u64) -> bool,
    ) -> Result<PsciCpuOnBegin, PsciCpuPowerError> {
        let Some(target_index) = self
            .cpus
            .iter()
            .position(|cpu| cpu.mpidr == request.target_mpidr)
        else {
            return Ok(PsciCpuOnBegin::Complete(PsciCpuOnResponse::InvalidTarget));
        };

        if request.entry_point & 0b11 != 0 || !entry_is_valid(request.entry_point) {
            return Ok(PsciCpuOnBegin::Complete(PsciCpuOnResponse::InvalidAddress));
        }

        let target = self
            .cpus
            .get(target_index)
            .ok_or(PsciCpuPowerError::InvalidTopology)?;
        match target.power {
            PsciCpuPowerState::On => {
                return Ok(PsciCpuOnBegin::Complete(PsciCpuOnResponse::AlreadyOn));
            }
            PsciCpuPowerState::OnPending => {
                return Ok(PsciCpuOnBegin::Complete(PsciCpuOnResponse::OnPending));
            }
            PsciCpuPowerState::Off
                if target.transaction.is_some()
                    || target.cpu_suspend_transaction.is_some()
                    || target.cpu_off_transaction.is_some() =>
            {
                return Ok(PsciCpuOnBegin::Complete(PsciCpuOnResponse::InternalFailure));
            }
            PsciCpuPowerState::Off => {}
        }

        let token = PsciCpuOnToken(self.next_token);
        self.next_token = self
            .next_token
            .checked_add(1)
            .ok_or(PsciCpuPowerError::TokenExhausted)?;
        let work = PsciCpuOnWork {
            token,
            target_index,
            request,
        };
        let target = self
            .cpus
            .get_mut(target_index)
            .ok_or(PsciCpuPowerError::InvalidTopology)?;
        target.power = PsciCpuPowerState::OnPending;
        target.transaction = Some(PsciCpuOnTransaction {
            work,
            phase: PsciCpuOnPhase::AwaitingTargetSetup,
        });

        Ok(PsciCpuOnBegin::Pending(work))
    }

    pub(crate) fn begin_cpu_off(
        &mut self,
        caller_index: usize,
    ) -> Result<PsciCpuOffBegin, PsciCpuPowerError> {
        let caller = self
            .cpus
            .get(caller_index)
            .ok_or(PsciCpuPowerError::InvalidCpuIndex {
                index: caller_index,
            })?;
        if caller.power != PsciCpuPowerState::On
            || caller.transaction.is_some()
            || caller.cpu_suspend_transaction.is_some()
            || caller.cpu_off_transaction.is_some()
        {
            return Ok(PsciCpuOffBegin::Complete(
                PsciCpuOffResponse::InternalFailure,
            ));
        }
        if self
            .cpus
            .iter()
            .filter(|cpu| cpu.power == PsciCpuPowerState::On)
            .count()
            <= 1
        {
            return Ok(PsciCpuOffBegin::Complete(PsciCpuOffResponse::Denied));
        }

        let token = PsciCpuOffToken(self.next_token);
        self.next_token = self
            .next_token
            .checked_add(1)
            .ok_or(PsciCpuPowerError::TokenExhausted)?;
        let work = PsciCpuOffWork {
            token,
            caller_index,
        };
        self.cpus
            .get_mut(caller_index)
            .ok_or(PsciCpuPowerError::InvalidCpuIndex {
                index: caller_index,
            })?
            .cpu_off_transaction = Some(work);
        Ok(PsciCpuOffBegin::Pending(work))
    }

    pub(crate) fn begin_cpu_suspend(
        &mut self,
        caller_index: usize,
    ) -> Result<PsciCpuSuspendWork, PsciCpuPowerError> {
        let caller = self
            .cpus
            .get(caller_index)
            .ok_or(PsciCpuPowerError::InvalidCpuIndex {
                index: caller_index,
            })?;
        if caller.power != PsciCpuPowerState::On
            || caller.transaction.is_some()
            || caller.cpu_suspend_transaction.is_some()
            || caller.cpu_off_transaction.is_some()
        {
            return Err(PsciCpuPowerError::CpuSuspendUnavailable {
                index: caller_index,
            });
        }

        let token = PsciCpuSuspendToken(self.next_token);
        self.next_token = self
            .next_token
            .checked_add(1)
            .ok_or(PsciCpuPowerError::TokenExhausted)?;
        let work = PsciCpuSuspendWork {
            token,
            caller_index,
        };
        self.cpus
            .get_mut(caller_index)
            .ok_or(PsciCpuPowerError::InvalidCpuIndex {
                index: caller_index,
            })?
            .cpu_suspend_transaction = Some(work);
        Ok(work)
    }

    pub(crate) fn validate_cpu_suspend(
        &self,
        token: PsciCpuSuspendToken,
        caller_index: usize,
    ) -> Result<(), PsciCpuPowerError> {
        let work = self.cpu_suspend_transaction(token)?;
        if work.caller_index == caller_index {
            Ok(())
        } else {
            Err(PsciCpuPowerError::UnknownCpuSuspendTransaction { token })
        }
    }

    pub(crate) fn abort_cpu_suspend(
        &mut self,
        token: PsciCpuSuspendToken,
    ) -> Result<(), PsciCpuPowerError> {
        let caller = self.cpu_for_suspend_transaction_mut(token)?;
        caller.cpu_suspend_transaction = None;
        Ok(())
    }

    pub(crate) fn commit_cpu_suspend(
        &mut self,
        token: PsciCpuSuspendToken,
    ) -> Result<(), PsciCpuPowerError> {
        let caller = self.cpu_for_suspend_transaction_mut(token)?;
        caller.cpu_suspend_transaction = None;
        Ok(())
    }

    pub(crate) fn abort_cpu_off(
        &mut self,
        token: PsciCpuOffToken,
    ) -> Result<(), PsciCpuPowerError> {
        let caller = self.cpu_for_off_transaction_mut(token)?;
        caller.cpu_off_transaction = None;
        Ok(())
    }

    pub(crate) fn commit_cpu_off(
        &mut self,
        token: PsciCpuOffToken,
    ) -> Result<(), PsciCpuPowerError> {
        let caller = self.cpu_for_off_transaction_mut(token)?;
        caller.power = PsciCpuPowerState::Off;
        caller.cpu_off_transaction = None;
        Ok(())
    }

    pub(crate) fn finish_target_setup(
        &mut self,
        token: PsciCpuOnToken,
        configured: bool,
    ) -> Result<PsciCpuOnResponse, PsciCpuPowerError> {
        let target = self.target_for_transaction_mut(token)?;
        let Some(mut transaction) = target.transaction else {
            return Err(PsciCpuPowerError::UnknownTransaction { token });
        };
        if transaction.phase != PsciCpuOnPhase::AwaitingTargetSetup {
            return Err(PsciCpuPowerError::InvalidTransactionPhase { token });
        }

        let response = if configured {
            PsciCpuOnResponse::Success
        } else {
            target.power = PsciCpuPowerState::Off;
            PsciCpuOnResponse::InternalFailure
        };
        transaction.phase = PsciCpuOnPhase::AwaitingCallerCompletion {
            response,
            target_configured: configured,
        };
        target.transaction = Some(transaction);
        Ok(response)
    }

    #[cfg(test)]
    pub(crate) fn caller_completion(
        &self,
        token: PsciCpuOnToken,
    ) -> Result<PsciCpuOnResponse, PsciCpuPowerError> {
        let transaction = self.transaction(token)?;
        match transaction.phase {
            PsciCpuOnPhase::AwaitingCallerCompletion { response, .. } => Ok(response),
            PsciCpuOnPhase::AwaitingTargetSetup
            | PsciCpuOnPhase::CallerCompleted
            | PsciCpuOnPhase::CallerAbandoned => {
                Err(PsciCpuPowerError::InvalidTransactionPhase { token })
            }
        }
    }

    pub(crate) fn commit_caller_completion(
        &mut self,
        token: PsciCpuOnToken,
    ) -> Result<(), PsciCpuPowerError> {
        let target = self.target_for_transaction_mut(token)?;
        let Some(mut transaction) = target.transaction else {
            return Err(PsciCpuPowerError::UnknownTransaction { token });
        };
        let PsciCpuOnPhase::AwaitingCallerCompletion {
            target_configured, ..
        } = transaction.phase
        else {
            return Err(PsciCpuPowerError::InvalidTransactionPhase { token });
        };

        if target_configured {
            transaction.phase = PsciCpuOnPhase::CallerCompleted;
            target.transaction = Some(transaction);
        } else {
            target.transaction = None;
        }
        Ok(())
    }

    pub(crate) fn abandon_caller_completion(
        &mut self,
        token: PsciCpuOnToken,
    ) -> Result<(), PsciCpuPowerError> {
        let target = self.target_for_transaction_mut(token)?;
        let Some(mut transaction) = target.transaction else {
            return Err(PsciCpuPowerError::UnknownTransaction { token });
        };
        match transaction.phase {
            PsciCpuOnPhase::AwaitingTargetSetup => {
                target.power = PsciCpuPowerState::Off;
                target.transaction = None;
            }
            PsciCpuOnPhase::AwaitingCallerCompletion {
                target_configured: true,
                ..
            } => {
                transaction.phase = PsciCpuOnPhase::CallerAbandoned;
                target.transaction = Some(transaction);
            }
            PsciCpuOnPhase::AwaitingCallerCompletion {
                target_configured: false,
                ..
            } => {
                target.transaction = None;
            }
            PsciCpuOnPhase::CallerCompleted | PsciCpuOnPhase::CallerAbandoned => {
                return Err(PsciCpuPowerError::InvalidTransactionPhase { token });
            }
        }
        Ok(())
    }

    pub(crate) fn mark_target_entered(
        &mut self,
        token: PsciCpuOnToken,
    ) -> Result<(), PsciCpuPowerError> {
        let target = self.target_for_transaction_mut(token)?;
        let Some(transaction) = target.transaction else {
            return Err(PsciCpuPowerError::UnknownTransaction { token });
        };
        if !matches!(
            transaction.phase,
            PsciCpuOnPhase::CallerCompleted | PsciCpuOnPhase::CallerAbandoned
        ) {
            return Err(PsciCpuPowerError::InvalidTransactionPhase { token });
        }

        target.power = PsciCpuPowerState::On;
        target.transaction = None;
        Ok(())
    }

    pub(crate) fn affinity_info(
        &self,
        request: PsciAffinityInfoRequest,
    ) -> PsciAffinityInfoResponse {
        if request.lowest_affinity_level != 0 {
            return PsciAffinityInfoResponse::InvalidLevel;
        }
        self.cpus
            .iter()
            .find(|cpu| cpu.mpidr == request.target_mpidr)
            .map_or(PsciAffinityInfoResponse::InvalidTarget, |cpu| {
                PsciAffinityInfoResponse::State(cpu.power)
            })
    }

    #[cfg(test)]
    fn transaction(
        &self,
        token: PsciCpuOnToken,
    ) -> Result<PsciCpuOnTransaction, PsciCpuPowerError> {
        self.cpus
            .iter()
            .filter_map(|cpu| cpu.transaction)
            .find(|transaction| transaction.work.token == token)
            .ok_or(PsciCpuPowerError::UnknownTransaction { token })
    }

    fn target_for_transaction_mut(
        &mut self,
        token: PsciCpuOnToken,
    ) -> Result<&mut PsciCpuState, PsciCpuPowerError> {
        self.cpus
            .iter_mut()
            .find(|cpu| {
                cpu.transaction
                    .is_some_and(|transaction| transaction.work.token == token)
            })
            .ok_or(PsciCpuPowerError::UnknownTransaction { token })
    }

    fn cpu_for_off_transaction_mut(
        &mut self,
        token: PsciCpuOffToken,
    ) -> Result<&mut PsciCpuState, PsciCpuPowerError> {
        self.cpus
            .iter_mut()
            .find(|cpu| {
                cpu.cpu_off_transaction
                    .is_some_and(|transaction| transaction.token == token)
            })
            .ok_or(PsciCpuPowerError::UnknownCpuOffTransaction { token })
    }

    fn cpu_suspend_transaction(
        &self,
        token: PsciCpuSuspendToken,
    ) -> Result<PsciCpuSuspendWork, PsciCpuPowerError> {
        self.cpus
            .iter()
            .filter_map(|cpu| cpu.cpu_suspend_transaction)
            .find(|work| work.token == token)
            .ok_or(PsciCpuPowerError::UnknownCpuSuspendTransaction { token })
    }

    fn cpu_for_suspend_transaction_mut(
        &mut self,
        token: PsciCpuSuspendToken,
    ) -> Result<&mut PsciCpuState, PsciCpuPowerError> {
        self.cpus
            .iter_mut()
            .find(|cpu| {
                cpu.cpu_suspend_transaction
                    .is_some_and(|work| work.token == token)
            })
            .ok_or(PsciCpuPowerError::UnknownCpuSuspendTransaction { token })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PSCI_AFFINITY_INFO_32, PSCI_AFFINITY_INFO_64, PSCI_CPU_OFF, PSCI_CPU_ON_32, PSCI_CPU_ON_64,
        PSCI_CPU_SUSPEND_32, PSCI_CPU_SUSPEND_64, PSCI_FEATURES, PSCI_MIGRATE_INFO_TYPE,
        PSCI_MIGRATE_INFO_TYPE_TRUSTED_OS_NOT_REQUIRED, PSCI_SYSTEM_OFF, PSCI_SYSTEM_RESET,
        PSCI_VERSION, PSCI_VERSION_0_2, PsciAffinityInfoRequest, PsciAffinityInfoResponse,
        PsciCall, PsciCallAction, PsciCoordinatedDispatch, PsciCoordinatorRequest, PsciCpuOffBegin,
        PsciCpuOffResponse, PsciCpuOnBegin, PsciCpuOnRequest, PsciCpuOnResponse,
        PsciCpuPowerCoordinator, PsciCpuPowerError, PsciCpuPowerState, PsciCpuSuspendToken,
        PsciStatus, call_uses_arg0, coordinated_call_argument_count, handle_call,
        handle_coordinated_call, not_supported_result,
    };

    fn coordinator() -> PsciCpuPowerCoordinator {
        PsciCpuPowerCoordinator::new(&[0, 1, 0x0000_0002_0000_0003])
            .expect("topology should be valid")
    }

    fn secondary_request() -> PsciCpuOnRequest {
        PsciCpuOnRequest::new(1, 0x8020_0000, 0xfeed_face_cafe_beef)
    }

    fn pending_work(coordinator: &mut PsciCpuPowerCoordinator) -> super::PsciCpuOnWork {
        let PsciCpuOnBegin::Pending(work) = coordinator
            .begin_cpu_on(secondary_request(), |_| true)
            .expect("CPU_ON should be modeled")
        else {
            panic!("secondary should start pending");
        };
        work
    }

    fn bring_secondary_online(coordinator: &mut PsciCpuPowerCoordinator) {
        let work = pending_work(coordinator);
        coordinator
            .finish_target_setup(work.token(), true)
            .expect("secondary setup should finish");
        coordinator
            .commit_caller_completion(work.token())
            .expect("caller completion should commit");
        coordinator
            .mark_target_entered(work.token())
            .expect("secondary should enter");
    }

    #[test]
    fn encodes_all_psci_statuses_as_zero_extended_signed_32_bit_values() {
        for (status, expected) in [
            (PsciStatus::Success, 0x0000_0000),
            (PsciStatus::NotSupported, 0xffff_ffff),
            (PsciStatus::InvalidParameters, 0xffff_fffe),
            (PsciStatus::Denied, 0xffff_fffd),
            (PsciStatus::AlreadyOn, 0xffff_fffc),
            (PsciStatus::OnPending, 0xffff_fffb),
            (PsciStatus::InternalFailure, 0xffff_fffa),
        ] {
            assert_eq!(status.return_value(), expected);
        }
    }

    #[test]
    fn preserves_legacy_psci_version_migration_and_terminal_actions() {
        assert_eq!(
            handle_call(PsciCall::new(PSCI_VERSION, 0)).return_value(),
            PSCI_VERSION_0_2
        );
        assert_eq!(
            handle_call(PsciCall::new(PSCI_MIGRATE_INFO_TYPE, 0)).return_value(),
            PSCI_MIGRATE_INFO_TYPE_TRUSTED_OS_NOT_REQUIRED
        );

        let off = handle_call(PsciCall::new(PSCI_SYSTEM_OFF, 0));
        assert_eq!(off.return_value(), PsciStatus::Success.return_value());
        assert_eq!(off.action(), PsciCallAction::SystemOff);
        let reset = handle_call(PsciCall::new(PSCI_SYSTEM_RESET, 0));
        assert_eq!(reset.return_value(), PsciStatus::Success.return_value());
        assert_eq!(reset.action(), PsciCallAction::SystemReset);
    }

    #[test]
    fn legacy_features_and_cpu_power_calls_stay_unsupported() {
        for function_id in [
            PSCI_CPU_SUSPEND_32,
            PSCI_CPU_SUSPEND_64,
            PSCI_CPU_OFF,
            PSCI_CPU_ON_32,
            PSCI_CPU_ON_64,
        ] {
            assert_eq!(
                handle_call(PsciCall::new(PSCI_FEATURES, function_id)).return_value(),
                PsciStatus::NotSupported.return_value()
            );
            assert_eq!(
                handle_call(PsciCall::new(function_id, 0)).return_value(),
                PsciStatus::NotSupported.return_value()
            );
        }
    }

    #[test]
    fn coordinated_features_advertise_cpu_off_cpu_on_and_affinity_info() {
        for function_id in [
            PSCI_CPU_SUSPEND_32,
            PSCI_CPU_SUSPEND_64,
            PSCI_CPU_OFF,
            PSCI_CPU_ON_32,
            PSCI_CPU_ON_64,
            PSCI_AFFINITY_INFO_32,
            PSCI_AFFINITY_INFO_64,
        ] {
            let PsciCoordinatedDispatch::Immediate(result) =
                handle_coordinated_call(PsciCall::new(PSCI_FEATURES, function_id))
            else {
                panic!("PSCI_FEATURES should complete immediately");
            };
            assert_eq!(result.return_value(), PsciStatus::Success.return_value());
        }
    }

    #[test]
    fn identifies_legacy_and_coordinated_argument_counts() {
        assert!(call_uses_arg0(PSCI_FEATURES));
        assert!(!call_uses_arg0(PSCI_CPU_ON_32));
        assert_eq!(coordinated_call_argument_count(PSCI_VERSION), 0);
        assert_eq!(coordinated_call_argument_count(PSCI_CPU_OFF), 0);
        assert_eq!(coordinated_call_argument_count(PSCI_CPU_SUSPEND_32), 3);
        assert_eq!(coordinated_call_argument_count(PSCI_CPU_SUSPEND_64), 3);
        assert_eq!(coordinated_call_argument_count(PSCI_FEATURES), 1);
        assert_eq!(coordinated_call_argument_count(PSCI_AFFINITY_INFO_64), 2);
        assert_eq!(coordinated_call_argument_count(PSCI_CPU_ON_32), 3);
    }

    #[test]
    fn builds_spec_shaped_not_supported_result() {
        let result = not_supported_result();
        assert_eq!(
            result.return_value(),
            PsciStatus::NotSupported.return_value()
        );
        assert_eq!(result.action(), PsciCallAction::Return);
    }

    #[test]
    fn parses_cpu_on_32_with_argument_truncation() {
        let call = PsciCall::from_arguments(
            PSCI_CPU_ON_32,
            [
                0xabcd_ef00_0000_0001,
                0xffff_ffff_8020_0000,
                0xfeed_face_cafe_beef,
            ],
        );
        let PsciCoordinatedDispatch::Coordinate(PsciCoordinatorRequest::CpuOn(request)) =
            handle_coordinated_call(call)
        else {
            panic!("CPU_ON32 should coordinate");
        };
        assert_eq!(request.target_mpidr(), 1);
        assert_eq!(request.entry_point(), 0x8020_0000);
        assert_eq!(request.context_id(), 0xcafe_beef);
    }

    #[test]
    fn parses_cpu_off_as_zero_argument_coordinator_work() {
        assert_eq!(
            handle_coordinated_call(PsciCall::from_arguments(
                PSCI_CPU_OFF,
                [u64::MAX, 0xfeed_face, 0xcafe_beef],
            )),
            PsciCoordinatedDispatch::Coordinate(PsciCoordinatorRequest::CpuOff)
        );
    }

    #[test]
    fn parses_both_cpu_suspend_widths_and_ignores_all_arguments() {
        for call in [
            PsciCall::from_arguments(
                PSCI_CPU_SUSPEND_32,
                [
                    0xaaaa_bbbb_ffff_0001,
                    0xcccc_dddd_ffff_0002,
                    0xeeee_ffff_ffff_0003,
                ],
            ),
            PsciCall::from_arguments(
                PSCI_CPU_SUSPEND_64,
                [0xaaaa_bbbb_cccc_dddd, u64::MAX, 0xfeed_face_cafe_beef],
            ),
        ] {
            assert_eq!(
                handle_coordinated_call(call),
                PsciCoordinatedDispatch::Coordinate(PsciCoordinatorRequest::CpuSuspend)
            );
        }
    }

    #[test]
    fn parses_cpu_on_64_with_aff3_and_full_width_arguments() {
        let request = PsciCpuOnRequest::new(
            0x0000_00ab_0000_0003,
            0x0000_0001_8020_0000,
            0xfeed_face_cafe_beef,
        );
        let PsciCoordinatedDispatch::Coordinate(PsciCoordinatorRequest::CpuOn(parsed)) =
            handle_coordinated_call(PsciCall::from_arguments(
                PSCI_CPU_ON_64,
                [
                    request.target_mpidr(),
                    request.entry_point(),
                    request.context_id(),
                ],
            ))
        else {
            panic!("CPU_ON64 should coordinate");
        };
        assert_eq!(parsed, request);
    }

    #[test]
    fn rejects_width_specific_reserved_target_bits_without_coordinator_work() {
        for call in [
            PsciCall::from_arguments(PSCI_CPU_ON_32, [0xff00_0001, 0x8000, 0]),
            PsciCall::from_arguments(PSCI_CPU_ON_64, [1 << 40, 0x8000, 0]),
            PsciCall::from_arguments(PSCI_AFFINITY_INFO_64, [1 << 63, 0, 0]),
        ] {
            let PsciCoordinatedDispatch::Immediate(result) = handle_coordinated_call(call) else {
                panic!("reserved target bits should fail immediately");
            };
            assert_eq!(
                result.return_value(),
                PsciStatus::InvalidParameters.return_value()
            );
        }
    }

    #[test]
    fn parses_affinity_info_widths_and_levels() {
        for (function_id, target, level, expected_target, expected_level) in [
            (
                PSCI_AFFINITY_INFO_32,
                0xffff_ffff_0000_0001,
                0xffff_ffff_0000_0002,
                1,
                2,
            ),
            (
                PSCI_AFFINITY_INFO_64,
                0x0000_0002_0000_0003,
                3,
                0x0000_0002_0000_0003,
                3,
            ),
        ] {
            let PsciCoordinatedDispatch::Coordinate(PsciCoordinatorRequest::AffinityInfo(request)) =
                handle_coordinated_call(PsciCall::from_arguments(function_id, [target, level, 0]))
            else {
                panic!("AFFINITY_INFO should coordinate");
            };
            assert_eq!(request.target_mpidr(), expected_target);
            assert_eq!(request.lowest_affinity_level(), expected_level);
        }
    }

    #[test]
    fn rejects_invalid_power_topologies() {
        assert_eq!(
            PsciCpuPowerCoordinator::new(&[]),
            Err(PsciCpuPowerError::InvalidTopology)
        );
        assert_eq!(
            PsciCpuPowerCoordinator::new(&[0, 0]),
            Err(PsciCpuPowerError::DuplicateMpidr { mpidr: 0 })
        );
        assert_eq!(
            PsciCpuPowerCoordinator::new(&[0, 1 << 40]),
            Err(PsciCpuPowerError::InvalidMpidr { mpidr: 1 << 40 })
        );
    }

    #[test]
    fn initializes_primary_on_and_secondaries_off() {
        let coordinator = coordinator();
        assert_eq!(coordinator.power_state(0), Some(PsciCpuPowerState::On));
        assert_eq!(coordinator.power_state(1), Some(PsciCpuPowerState::Off));
        assert_eq!(coordinator.power_state(2), Some(PsciCpuPowerState::Off));
        assert_eq!(coordinator.power_state(3), None);
    }

    #[test]
    fn begin_cpu_on_validates_before_mutating_state() {
        let mut coordinator = coordinator();
        for (request, response) in [
            (
                PsciCpuOnRequest::new(99, 0x8020_0000, 0),
                PsciCpuOnResponse::InvalidTarget,
            ),
            (
                PsciCpuOnRequest::new(1, 0x8020_0002, 0),
                PsciCpuOnResponse::InvalidAddress,
            ),
            (
                PsciCpuOnRequest::new(1, 0x8020_0000, 0),
                PsciCpuOnResponse::InvalidAddress,
            ),
        ] {
            let entry_is_valid = request.entry_point() != 0x8020_0000;
            assert_eq!(
                coordinator
                    .begin_cpu_on(request, |_| entry_is_valid)
                    .expect("validation should not fail the model"),
                PsciCpuOnBegin::Complete(response)
            );
            assert_eq!(coordinator.power_state(1), Some(PsciCpuPowerState::Off));
        }
    }

    #[test]
    fn cpu_on_reports_already_on_and_on_pending() {
        let mut coordinator = coordinator();
        assert_eq!(
            coordinator
                .begin_cpu_on(PsciCpuOnRequest::new(0, 0x8000, 0), |_| true)
                .expect("primary query should be modeled"),
            PsciCpuOnBegin::Complete(PsciCpuOnResponse::AlreadyOn)
        );
        let work = pending_work(&mut coordinator);
        assert_eq!(work.target_index(), 1);
        assert_eq!(work.request(), secondary_request());
        assert_eq!(
            coordinator.power_state(1),
            Some(PsciCpuPowerState::OnPending)
        );
        assert_eq!(
            coordinator
                .begin_cpu_on(secondary_request(), |_| true)
                .expect("repeat should be modeled"),
            PsciCpuOnBegin::Complete(PsciCpuOnResponse::OnPending)
        );
    }

    #[test]
    fn cpu_off_denies_last_on_cpu_without_reserving_state() {
        let mut coordinator = coordinator();
        assert_eq!(
            coordinator.begin_cpu_off(0),
            Ok(PsciCpuOffBegin::Complete(PsciCpuOffResponse::Denied))
        );
        assert_eq!(coordinator.power_state(0), Some(PsciCpuPowerState::On));
        assert_eq!(
            coordinator.begin_cpu_off(1),
            Ok(PsciCpuOffBegin::Complete(
                PsciCpuOffResponse::InternalFailure
            ))
        );
        assert_eq!(
            coordinator.begin_cpu_off(3),
            Err(PsciCpuPowerError::InvalidCpuIndex { index: 3 })
        );
    }

    #[test]
    fn cpu_off_reservation_keeps_affinity_on_until_commit_and_can_abort() {
        let mut coordinator = coordinator();
        bring_secondary_online(&mut coordinator);
        let PsciCpuOffBegin::Pending(work) = coordinator
            .begin_cpu_off(1)
            .expect("online secondary CPU_OFF should be modeled")
        else {
            panic!("online secondary should reserve CPU_OFF");
        };
        assert_eq!(work.caller_index(), 1);
        assert_eq!(coordinator.power_state(1), Some(PsciCpuPowerState::On));
        assert_eq!(
            coordinator.affinity_info(PsciAffinityInfoRequest::new(1, 0)),
            PsciAffinityInfoResponse::State(PsciCpuPowerState::On)
        );
        assert_eq!(
            coordinator.begin_cpu_off(1),
            Ok(PsciCpuOffBegin::Complete(
                PsciCpuOffResponse::InternalFailure
            ))
        );
        coordinator
            .abort_cpu_off(work.token())
            .expect("reserved CPU_OFF should abort");
        assert_eq!(coordinator.power_state(1), Some(PsciCpuPowerState::On));
        assert_eq!(
            coordinator.abort_cpu_off(work.token()),
            Err(PsciCpuPowerError::UnknownCpuOffTransaction {
                token: work.token()
            })
        );
    }

    #[test]
    fn cpu_off_commit_publishes_off_and_later_cpu_on_is_admitted() {
        let mut coordinator = coordinator();
        bring_secondary_online(&mut coordinator);
        let PsciCpuOffBegin::Pending(work) = coordinator
            .begin_cpu_off(1)
            .expect("online secondary CPU_OFF should be modeled")
        else {
            panic!("online secondary should reserve CPU_OFF");
        };
        coordinator
            .commit_cpu_off(work.token())
            .expect("reserved CPU_OFF should commit");
        assert_eq!(coordinator.power_state(1), Some(PsciCpuPowerState::Off));
        assert_eq!(
            coordinator.affinity_info(PsciAffinityInfoRequest::new(1, 0)),
            PsciAffinityInfoResponse::State(PsciCpuPowerState::Off)
        );
        assert!(matches!(
            coordinator
                .begin_cpu_on(secondary_request(), |_| true)
                .expect("offlined secondary should be reusable"),
            PsciCpuOnBegin::Pending(_)
        ));
    }

    #[test]
    fn cpu_off_allows_primary_only_while_secondary_is_on() {
        let mut coordinator = coordinator();
        bring_secondary_online(&mut coordinator);
        let PsciCpuOffBegin::Pending(work) = coordinator
            .begin_cpu_off(0)
            .expect("primary CPU_OFF should be modeled while a peer is on")
        else {
            panic!("primary should reserve CPU_OFF while a peer is on");
        };
        coordinator
            .commit_cpu_off(work.token())
            .expect("primary CPU_OFF should commit");
        assert_eq!(coordinator.power_state(0), Some(PsciCpuPowerState::Off));
        assert_eq!(
            coordinator.begin_cpu_off(1),
            Ok(PsciCpuOffBegin::Complete(PsciCpuOffResponse::Denied))
        );
    }

    #[test]
    fn cpu_suspend_reservation_keeps_affinity_on_and_rejects_conflicts() {
        let mut coordinator = coordinator();
        let work = coordinator
            .begin_cpu_suspend(0)
            .expect("online primary should reserve CPU_SUSPEND");
        assert_eq!(work.caller_index(), 0);
        assert_eq!(coordinator.power_state(0), Some(PsciCpuPowerState::On));
        assert_eq!(
            coordinator.affinity_info(PsciAffinityInfoRequest::new(0, 0)),
            PsciAffinityInfoResponse::State(PsciCpuPowerState::On)
        );
        assert_eq!(
            coordinator.begin_cpu_suspend(0),
            Err(PsciCpuPowerError::CpuSuspendUnavailable { index: 0 })
        );
        assert_eq!(
            coordinator.begin_cpu_off(0),
            Ok(PsciCpuOffBegin::Complete(
                PsciCpuOffResponse::InternalFailure
            ))
        );
        assert_eq!(
            coordinator.validate_cpu_suspend(work.token(), 1),
            Err(PsciCpuPowerError::UnknownCpuSuspendTransaction {
                token: work.token()
            })
        );
        assert_eq!(coordinator.validate_cpu_suspend(work.token(), 0), Ok(()));
    }

    #[test]
    fn cpu_suspend_abort_and_commit_are_exact_and_repeatable() {
        let mut coordinator = coordinator();
        let first = coordinator
            .begin_cpu_suspend(0)
            .expect("first CPU_SUSPEND should reserve");
        coordinator
            .abort_cpu_suspend(first.token())
            .expect("first CPU_SUSPEND should abort");
        assert_eq!(
            coordinator.abort_cpu_suspend(first.token()),
            Err(PsciCpuPowerError::UnknownCpuSuspendTransaction {
                token: first.token()
            })
        );

        let second = coordinator
            .begin_cpu_suspend(0)
            .expect("second CPU_SUSPEND should reserve");
        assert_ne!(first.token(), second.token());
        coordinator
            .commit_cpu_suspend(second.token())
            .expect("second CPU_SUSPEND should commit");
        assert_eq!(coordinator.power_state(0), Some(PsciCpuPowerState::On));
        assert!(coordinator.begin_cpu_suspend(0).is_ok());
    }

    #[test]
    fn cpu_suspend_rejects_off_cpu_and_preserves_cpu_on_already_on() {
        let mut coordinator = coordinator();
        assert_eq!(
            coordinator.begin_cpu_suspend(1),
            Err(PsciCpuPowerError::CpuSuspendUnavailable { index: 1 })
        );
        bring_secondary_online(&mut coordinator);
        let work = coordinator
            .begin_cpu_suspend(1)
            .expect("online secondary should suspend");
        assert_eq!(
            coordinator
                .begin_cpu_on(secondary_request(), |_| true)
                .expect("CPU_ON should still inspect ON affinity"),
            PsciCpuOnBegin::Complete(PsciCpuOnResponse::AlreadyOn)
        );
        coordinator
            .commit_cpu_suspend(work.token())
            .expect("secondary suspend should commit");
    }

    #[test]
    fn cpu_suspend_token_exhaustion_is_mutation_free() {
        let mut coordinator = coordinator();
        coordinator.next_token = u64::MAX;
        assert_eq!(
            coordinator.begin_cpu_suspend(0),
            Err(PsciCpuPowerError::TokenExhausted)
        );
        assert_eq!(
            coordinator.validate_cpu_suspend(PsciCpuSuspendToken(u64::MAX), 0),
            Err(PsciCpuPowerError::UnknownCpuSuspendTransaction {
                token: PsciCpuSuspendToken(u64::MAX)
            })
        );
        assert_eq!(coordinator.power_state(0), Some(PsciCpuPowerState::On));
    }

    #[test]
    fn successful_transaction_requires_caller_commit_before_target_entry() {
        let mut coordinator = coordinator();
        let work = pending_work(&mut coordinator);
        assert_eq!(
            coordinator.finish_target_setup(work.token(), true),
            Ok(PsciCpuOnResponse::Success)
        );
        assert_eq!(
            coordinator.caller_completion(work.token()),
            Ok(PsciCpuOnResponse::Success)
        );
        assert_eq!(
            coordinator.mark_target_entered(work.token()),
            Err(PsciCpuPowerError::InvalidTransactionPhase {
                token: work.token()
            })
        );
        coordinator
            .commit_caller_completion(work.token())
            .expect("caller should commit");
        assert_eq!(
            coordinator.power_state(1),
            Some(PsciCpuPowerState::OnPending)
        );
        coordinator
            .mark_target_entered(work.token())
            .expect("configured target should enter");
        assert_eq!(coordinator.power_state(1), Some(PsciCpuPowerState::On));
    }

    #[test]
    fn target_setup_failure_rolls_back_and_completion_can_be_retried() {
        let mut coordinator = coordinator();
        let work = pending_work(&mut coordinator);
        assert_eq!(
            coordinator.finish_target_setup(work.token(), false),
            Ok(PsciCpuOnResponse::InternalFailure)
        );
        assert_eq!(coordinator.power_state(1), Some(PsciCpuPowerState::Off));
        assert_eq!(
            coordinator.caller_completion(work.token()),
            Ok(PsciCpuOnResponse::InternalFailure)
        );
        assert_eq!(
            coordinator.caller_completion(work.token()),
            Ok(PsciCpuOnResponse::InternalFailure)
        );
        coordinator
            .commit_caller_completion(work.token())
            .expect("failure response should commit");
        assert_eq!(
            coordinator.caller_completion(work.token()),
            Err(PsciCpuPowerError::UnknownTransaction {
                token: work.token()
            })
        );
        assert!(matches!(
            coordinator
                .begin_cpu_on(secondary_request(), |_| true)
                .expect("target should be reusable"),
            PsciCpuOnBegin::Pending(_)
        ));
    }

    #[test]
    fn abandonment_rolls_back_unconfigured_target() {
        let mut coordinator = coordinator();
        let work = pending_work(&mut coordinator);
        coordinator
            .abandon_caller_completion(work.token())
            .expect("unconfigured request should abandon");
        assert_eq!(coordinator.power_state(1), Some(PsciCpuPowerState::Off));
        assert_eq!(
            coordinator.finish_target_setup(work.token(), true),
            Err(PsciCpuPowerError::UnknownTransaction {
                token: work.token()
            })
        );
    }

    #[test]
    fn abandonment_preserves_configured_target_until_entry() {
        let mut coordinator = coordinator();
        let work = pending_work(&mut coordinator);
        coordinator
            .finish_target_setup(work.token(), true)
            .expect("target setup should finish");
        coordinator
            .abandon_caller_completion(work.token())
            .expect("configured request should abandon caller");
        assert_eq!(
            coordinator.power_state(1),
            Some(PsciCpuPowerState::OnPending)
        );
        coordinator
            .mark_target_entered(work.token())
            .expect("abandoned caller should not undo entered target");
        assert_eq!(coordinator.power_state(1), Some(PsciCpuPowerState::On));
    }

    #[test]
    fn stale_and_duplicate_transitions_are_mutation_free() {
        let mut coordinator = coordinator();
        let work = pending_work(&mut coordinator);
        let stale = super::PsciCpuOnToken(99);
        assert_eq!(
            coordinator.finish_target_setup(stale, true),
            Err(PsciCpuPowerError::UnknownTransaction { token: stale })
        );
        assert_eq!(
            coordinator.power_state(1),
            Some(PsciCpuPowerState::OnPending)
        );
        coordinator
            .finish_target_setup(work.token(), true)
            .expect("first setup should finish");
        assert_eq!(
            coordinator.finish_target_setup(work.token(), true),
            Err(PsciCpuPowerError::InvalidTransactionPhase {
                token: work.token()
            })
        );
        assert_eq!(
            coordinator.caller_completion(work.token()),
            Ok(PsciCpuOnResponse::Success)
        );
    }

    #[test]
    fn affinity_info_reports_every_level_zero_state() {
        let mut coordinator = coordinator();
        assert_eq!(
            coordinator.affinity_info(PsciAffinityInfoRequest::new(0, 0)),
            PsciAffinityInfoResponse::State(PsciCpuPowerState::On)
        );
        assert_eq!(
            coordinator.affinity_info(PsciAffinityInfoRequest::new(1, 0)),
            PsciAffinityInfoResponse::State(PsciCpuPowerState::Off)
        );
        let work = pending_work(&mut coordinator);
        assert_eq!(
            coordinator.affinity_info(PsciAffinityInfoRequest::new(1, 0)),
            PsciAffinityInfoResponse::State(PsciCpuPowerState::OnPending)
        );
        coordinator
            .finish_target_setup(work.token(), true)
            .expect("target setup should finish");
        coordinator
            .commit_caller_completion(work.token())
            .expect("caller should complete");
        coordinator
            .mark_target_entered(work.token())
            .expect("target should enter");
        assert_eq!(
            coordinator.affinity_info(PsciAffinityInfoRequest::new(1, 0)),
            PsciAffinityInfoResponse::State(PsciCpuPowerState::On)
        );
    }

    #[test]
    fn affinity_info_rejects_unknown_targets_and_higher_levels() {
        let coordinator = coordinator();
        for (request, expected) in [
            (
                PsciAffinityInfoRequest::new(99, 0),
                PsciAffinityInfoResponse::InvalidTarget,
            ),
            (
                PsciAffinityInfoRequest::new(1, 1),
                PsciAffinityInfoResponse::InvalidLevel,
            ),
        ] {
            let response = coordinator.affinity_info(request);
            assert_eq!(response, expected);
            assert_eq!(
                response.return_value(),
                PsciStatus::InvalidParameters.return_value()
            );
        }
    }

    #[test]
    fn cpu_on_response_keeps_invalid_address_typed_but_psci_0_2_compatible() {
        assert_eq!(
            PsciCpuOnResponse::InvalidAddress.status(),
            PsciStatus::InvalidParameters
        );
        assert_eq!(
            PsciCpuOnResponse::InvalidTarget.status(),
            PsciStatus::InvalidParameters
        );
        assert_eq!(
            PsciCpuOnResponse::Unsupported.status(),
            PsciStatus::NotSupported
        );
    }
}
