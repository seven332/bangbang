use std::fmt;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};

use bangbang_runtime::mmio::MmioDispatcher;

use crate::psci::{PsciCoordinatorRequest, PsciCoordinatorResponse};
use crate::runner::{
    HvfVcpuCoordinatedRunStepOutcome, HvfVcpuPsciCallToken, HvfVcpuRunCompletion, HvfVcpuRunToken,
    HvfVcpuRunner, HvfVcpuRunnerError, cancel_vcpu_run_batch_with,
};
use crate::topology::{HvfVcpuTopology, HvfVcpuTopologyError};
use crate::vcpu::{HvfArm64BootRegisters, HvfArm64SecondaryBootRegisters};
use crate::{HvfHvcExit, HvfVcpuRunStepOutcome};

const STATE_POISONED_MESSAGE: &str = "vCPU run coordinator state lock is poisoned";
const COMPLETION_CHANNEL_CLOSED_MESSAGE: &str = "vCPU run coordinator completion channel is closed";
const RUNNING_PHASE_REQUIRED_MESSAGE: &str = "vCPU run coordinator is not accepting runs";
const PAUSED_PHASE_REQUIRED_MESSAGE: &str = "vCPU run coordinator is not paused";
const ACTIVE_MEMBER_STATE_MESSAGE: &str =
    "vCPU run coordinator member already has an active generation";
const MEMBER_NOT_IDLE_MESSAGE: &str = "vCPU run coordinator member is not idle";
const OFFLINE_CANCELLATION_DEBT_MESSAGE: &str =
    "vCPU run coordinator member has cancellation debt and cannot go offline";
const CONTROL_DURING_TERMINAL_MESSAGE: &str =
    "vCPU run control was superseded by a terminal member result";
const TERMINAL_REPORT_MISSING_MESSAGE: &str =
    "vCPU run coordinator terminal drain has no terminal result";
const UNEXPECTED_DRAIN_RESULT_MESSAGE: &str =
    "vCPU run coordinator returned an unexpected result while draining";
const BATCH_CANCEL_INDEX_MESSAGE: &str =
    "vCPU batch cancellation referenced an unknown topology member";

type BatchCancel = Arc<dyn Fn(&[usize]) -> Result<(), HvfVcpuRunnerError> + Send + Sync + 'static>;

/// Reason for quiescing every currently active vCPU run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfVcpuRunControlReason {
    /// Regain boot-worker control without changing lifecycle state.
    Wakeup,
    /// Quiesce the topology until an explicit resume.
    Pause,
    /// Stop scheduling guest execution.
    Stop,
    /// Stop scheduling and then shut down every owner thread.
    Shutdown,
}

impl HvfVcpuRunControlReason {
    const fn priority(self) -> u8 {
        match self {
            Self::Wakeup => 0,
            Self::Pause => 1,
            Self::Stop => 2,
            Self::Shutdown => 3,
        }
    }

    const fn max(self, other: Self) -> Self {
        if self.priority() >= other.priority() {
            self
        } else {
            other
        }
    }
}

impl fmt::Display for HvfVcpuRunControlReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wakeup => f.write_str("wakeup"),
            Self::Pause => f.write_str("pause"),
            Self::Stop => f.write_str("stop"),
            Self::Shutdown => f.write_str("shutdown"),
        }
    }
}

/// Opaque cross-vCPU work returned by a coordinated PSCI exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfVcpuCoordinatorWork {
    exit: HvfHvcExit,
    function_id: u64,
    runner_token: HvfVcpuPsciCallToken,
    request: PsciCoordinatorRequest,
}

impl HvfVcpuCoordinatorWork {
    pub(crate) const fn into_parts(
        self,
    ) -> (
        HvfHvcExit,
        u64,
        HvfVcpuPsciCallToken,
        PsciCoordinatorRequest,
    ) {
        (self.exit, self.function_id, self.runner_token, self.request)
    }
}

/// Result of one identified bounded run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfVcpuRunMemberOutcome {
    /// Exit handled entirely on the vCPU owner thread.
    Handled(HvfVcpuRunStepOutcome),
    /// Cross-vCPU work that must be completed by the aggregate coordinator.
    Coordinator(HvfVcpuCoordinatorWork),
}

impl From<HvfVcpuCoordinatedRunStepOutcome> for HvfVcpuRunMemberOutcome {
    fn from(outcome: HvfVcpuCoordinatedRunStepOutcome) -> Self {
        match outcome {
            HvfVcpuCoordinatedRunStepOutcome::Handled(outcome) => Self::Handled(outcome),
            HvfVcpuCoordinatedRunStepOutcome::Psci {
                exit,
                function_id,
                token,
                request,
            } => Self::Coordinator(HvfVcpuCoordinatorWork {
                exit,
                function_id,
                runner_token: token,
                request,
            }),
        }
    }
}

/// Completion or failure for one topology member and run generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HvfVcpuRunMemberResult {
    index: usize,
    mpidr: u64,
    generation: u64,
    result: Result<HvfVcpuRunMemberOutcome, HvfVcpuRunnerError>,
}

impl HvfVcpuRunMemberResult {
    const fn new(
        index: usize,
        mpidr: u64,
        generation: u64,
        result: Result<HvfVcpuRunMemberOutcome, HvfVcpuRunnerError>,
    ) -> Self {
        Self {
            index,
            mpidr,
            generation,
            result,
        }
    }

    /// Return the stable topology index.
    pub const fn index(&self) -> usize {
        self.index
    }

    /// Return the verified MPIDR associated with the member.
    pub const fn mpidr(&self) -> u64 {
        self.mpidr
    }

    /// Return the coordinator-issued run generation.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Return the owner-thread result.
    pub const fn result(&self) -> Result<&HvfVcpuRunMemberOutcome, &HvfVcpuRunnerError> {
        self.result.as_ref()
    }

    fn is_canceled(&self) -> bool {
        matches!(
            self.result,
            Ok(HvfVcpuRunMemberOutcome::Handled(
                HvfVcpuRunStepOutcome::Canceled
            ))
        )
    }

    fn terminal_priority(&self) -> Option<u8> {
        match &self.result {
            Err(_) => Some(0),
            Ok(HvfVcpuRunMemberOutcome::Handled(HvfVcpuRunStepOutcome::Unknown { .. })) => Some(1),
            Ok(HvfVcpuRunMemberOutcome::Handled(HvfVcpuRunStepOutcome::GuestReset { .. })) => {
                Some(2)
            }
            Ok(HvfVcpuRunMemberOutcome::Handled(HvfVcpuRunStepOutcome::GuestShutdown {
                ..
            })) => Some(3),
            Ok(_) => None,
        }
    }
}

/// Exact acknowledgements collected for one topology-wide control barrier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HvfVcpuRunBarrierReport {
    reason: HvfVcpuRunControlReason,
    acknowledgements: Vec<HvfVcpuRunMemberResult>,
}

impl HvfVcpuRunBarrierReport {
    /// Return the strongest coalesced control reason.
    pub const fn reason(&self) -> HvfVcpuRunControlReason {
        self.reason
    }

    /// Return one acknowledgement for each run in the canceled snapshot.
    pub fn acknowledgements(&self) -> &[HvfVcpuRunMemberResult] {
        &self.acknowledgements
    }
}

/// Deterministic terminal reduction after every active peer has drained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HvfVcpuRunTerminalReport {
    primary: HvfVcpuRunMemberResult,
    members: Vec<HvfVcpuRunMemberResult>,
}

impl HvfVcpuRunTerminalReport {
    /// Return the stable primary failure or terminal outcome.
    pub const fn primary(&self) -> &HvfVcpuRunMemberResult {
        &self.primary
    }

    /// Return all triggering and peer-drain results in topology order.
    pub fn members(&self) -> &[HvfVcpuRunMemberResult] {
        &self.members
    }
}

/// Next aggregate event observed by the boot-worker coordinator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfVcpuRunEvent {
    /// One non-terminal member completed while peers may remain active.
    Member(HvfVcpuRunMemberResult),
    /// A requested topology-wide control barrier completed.
    Barrier(HvfVcpuRunBarrierReport),
    /// A terminal member result canceled and drained every active peer.
    Terminal(HvfVcpuRunTerminalReport),
}

/// Failure while coordinating concurrent vCPU runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfVcpuRunCoordinatorError {
    /// An online/member index is outside the ordered topology.
    InvalidMember { index: usize, member_count: usize },
    /// The initial online set contains the same member more than once.
    DuplicateOnlineMember { index: usize },
    /// The coordinator cannot perform the operation in its current phase.
    InvalidState(&'static str),
    /// A member exhausted its monotonic run-generation space.
    GenerationExhausted { index: usize, mpidr: u64 },
    /// An owner rejected one bounded-run submission.
    Submission {
        index: usize,
        mpidr: u64,
        source: Box<HvfVcpuRunnerError>,
        acknowledgements: Vec<HvfVcpuRunMemberResult>,
    },
    /// Prefix cancellation also failed after a bounded-run submission failure.
    SubmissionCleanup {
        index: usize,
        mpidr: u64,
        source: Box<HvfVcpuRunnerError>,
        cleanup: Box<HvfVcpuRunnerError>,
        acknowledgements: Vec<HvfVcpuRunMemberResult>,
    },
    /// One batch cancellation failed before quiescence was proven.
    BatchCancel {
        reason: Option<HvfVcpuRunControlReason>,
        source: Box<HvfVcpuRunnerError>,
        acknowledgements: Vec<HvfVcpuRunMemberResult>,
    },
    /// The owner-thread completion channel closed unexpectedly.
    CompletionChannelClosed,
    /// A completion token did not match the member's active generation.
    CompletionIdentity {
        index: usize,
        generation: u64,
        expected: Option<u64>,
    },
    /// A control waiter was superseded by a terminal member result.
    ControlSupersededByTerminal {
        reason: HvfVcpuRunControlReason,
        report: Box<HvfVcpuRunTerminalReport>,
    },
    /// One indexed owner-thread control operation failed.
    MemberOperation {
        operation: &'static str,
        index: usize,
        mpidr: u64,
        source: Box<HvfVcpuRunnerError>,
    },
    /// Owner-thread topology shutdown did not complete cleanly.
    Shutdown(Box<HvfVcpuTopologyError>),
    /// Cleanup also failed after an earlier shutdown-barrier failure.
    ShutdownCleanup {
        primary: Box<HvfVcpuRunCoordinatorError>,
        cleanup: Box<HvfVcpuTopologyError>,
    },
}

impl fmt::Display for HvfVcpuRunCoordinatorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMember {
                index,
                member_count,
            } => write!(
                f,
                "vCPU topology member index {index} is outside 0..{member_count}"
            ),
            Self::DuplicateOnlineMember { index } => {
                write!(
                    f,
                    "vCPU topology member {index} appears twice in the online set"
                )
            }
            Self::InvalidState(message) => f.write_str(message),
            Self::GenerationExhausted { index, mpidr } => write!(
                f,
                "vCPU topology member {index} (MPIDR 0x{mpidr:x}) exhausted run generations"
            ),
            Self::Submission {
                index,
                mpidr,
                source,
                acknowledgements,
            } => write!(
                f,
                "failed to submit vCPU topology member {index} (MPIDR 0x{mpidr:x}); drained {} earlier runs: {source}",
                acknowledgements.len()
            ),
            Self::SubmissionCleanup {
                index,
                mpidr,
                source,
                cleanup,
                acknowledgements,
            } => write!(
                f,
                "failed to submit vCPU topology member {index} (MPIDR 0x{mpidr:x}); prefix cancellation also failed after {} acknowledgements: {source}; cleanup: {cleanup}",
                acknowledgements.len()
            ),
            Self::BatchCancel {
                reason,
                source,
                acknowledgements,
            } => {
                if let Some(reason) = reason {
                    write!(
                        f,
                        "vCPU topology {reason} batch cancellation failed after {} acknowledgements: {source}",
                        acknowledgements.len()
                    )
                } else {
                    write!(
                        f,
                        "vCPU topology terminal batch cancellation failed after {} acknowledgements: {source}",
                        acknowledgements.len()
                    )
                }
            }
            Self::CompletionChannelClosed => f.write_str(COMPLETION_CHANNEL_CLOSED_MESSAGE),
            Self::CompletionIdentity {
                index,
                generation,
                expected,
            } => write!(
                f,
                "vCPU topology member {index} completed generation {generation}, expected {expected:?}"
            ),
            Self::ControlSupersededByTerminal { reason, report } => {
                let primary = report.primary();
                write!(
                    f,
                    "vCPU topology {reason} control was superseded by terminal member {} (MPIDR 0x{:x})",
                    primary.index(),
                    primary.mpidr()
                )
            }
            Self::MemberOperation {
                operation,
                index,
                mpidr,
                source,
            } => write!(
                f,
                "vCPU topology member {index} (MPIDR 0x{mpidr:x}) {operation} failed: {source}"
            ),
            Self::Shutdown(source) => write!(f, "vCPU run coordinator shutdown failed: {source}"),
            Self::ShutdownCleanup { primary, cleanup } => write!(
                f,
                "vCPU run coordinator shutdown cleanup failed after {primary}: {cleanup}"
            ),
        }
    }
}

impl std::error::Error for HvfVcpuRunCoordinatorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Submission { source, .. }
            | Self::SubmissionCleanup { source, .. }
            | Self::BatchCancel { source, .. }
            | Self::MemberOperation { source, .. } => Some(source.as_ref()),
            Self::Shutdown(source) => Some(source.as_ref()),
            Self::ShutdownCleanup { primary, .. } => Some(primary.as_ref()),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct MemberState {
    online: bool,
    active: Option<HvfVcpuRunToken>,
    next_generation: u64,
    cancellation_debt: Option<u64>,
}

#[derive(Debug, Clone)]
struct SubmissionFailure {
    index: usize,
    mpidr: u64,
    source: HvfVcpuRunnerError,
}

#[derive(Debug)]
enum DrainReason {
    Control(HvfVcpuRunControlReason),
    Terminal {
        superseded_control: Option<HvfVcpuRunControlReason>,
    },
    Submission(SubmissionFailure),
}

#[derive(Debug)]
struct DrainState {
    reason: DrainReason,
    remaining: Vec<HvfVcpuRunToken>,
    acknowledgements: Vec<HvfVcpuRunMemberResult>,
    waiters: Vec<mpsc::Sender<Result<HvfVcpuRunBarrierReport, HvfVcpuRunCoordinatorError>>>,
}

#[derive(Debug)]
enum CoordinatorPhase {
    Running,
    Draining(Box<DrainState>),
    Paused,
    Stopped,
    Failed,
    ShutDown,
}

#[derive(Debug)]
struct CoordinatorState {
    phase: CoordinatorPhase,
    members: Vec<MemberState>,
}

struct CoordinatorShared {
    state: Mutex<CoordinatorState>,
    mpidrs: Arc<[u64]>,
    batch_cancel: BatchCancel,
}

impl fmt::Debug for CoordinatorShared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.state.lock() {
            Ok(state) => f
                .debug_struct("CoordinatorShared")
                .field("phase", &state.phase)
                .field("members", &state.members)
                .field("mpidrs", &self.mpidrs)
                .finish_non_exhaustive(),
            Err(_) => f
                .debug_struct("CoordinatorShared")
                .field("state", &STATE_POISONED_MESSAGE)
                .field("mpidrs", &self.mpidrs)
                .finish_non_exhaustive(),
        }
    }
}

impl CoordinatorShared {
    fn lock_state(&self) -> Result<MutexGuard<'_, CoordinatorState>, HvfVcpuRunCoordinatorError> {
        self.state
            .lock()
            .map_err(|_| HvfVcpuRunCoordinatorError::InvalidState(STATE_POISONED_MESSAGE))
    }
}

/// Wait handle completed only after every run in a control snapshot responds.
///
/// For a non-empty snapshot, the thread that owns the corresponding
/// [`HvfVcpuRunCoordinator`] must keep calling
/// [`HvfVcpuRunCoordinator::receive_event`] while a separate control requester
/// waits on this handle.
pub struct HvfVcpuRunBarrierWaiter {
    receiver: mpsc::Receiver<Result<HvfVcpuRunBarrierReport, HvfVcpuRunCoordinatorError>>,
}

impl HvfVcpuRunBarrierWaiter {
    /// Wait for exact per-member barrier acknowledgements.
    ///
    /// Do not call this on the coordinator-owning thread while the snapshot is
    /// active: that thread must continue driving completion collection through
    /// [`HvfVcpuRunCoordinator::receive_event`].
    pub fn wait(self) -> Result<HvfVcpuRunBarrierReport, HvfVcpuRunCoordinatorError> {
        self.receiver
            .recv()
            .map_err(|_| HvfVcpuRunCoordinatorError::CompletionChannelClosed)?
    }

    fn try_wait(
        &self,
    ) -> Result<Result<HvfVcpuRunBarrierReport, HvfVcpuRunCoordinatorError>, mpsc::TryRecvError>
    {
        self.receiver.try_recv()
    }
}

impl fmt::Debug for HvfVcpuRunBarrierWaiter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfVcpuRunBarrierWaiter")
            .finish_non_exhaustive()
    }
}

/// Cloneable topology-wide run-control handle.
#[derive(Clone)]
pub struct HvfVcpuRunControl {
    shared: Arc<CoordinatorShared>,
}

impl HvfVcpuRunControl {
    /// Request an ordinary non-terminal wakeup barrier.
    pub fn request_wakeup(&self) -> Result<HvfVcpuRunBarrierWaiter, HvfVcpuRunCoordinatorError> {
        self.request(HvfVcpuRunControlReason::Wakeup)
    }

    /// Request a topology-wide pause barrier.
    pub fn request_pause(&self) -> Result<HvfVcpuRunBarrierWaiter, HvfVcpuRunCoordinatorError> {
        self.request(HvfVcpuRunControlReason::Pause)
    }

    /// Request a topology-wide stop barrier.
    pub fn request_stop(&self) -> Result<HvfVcpuRunBarrierWaiter, HvfVcpuRunCoordinatorError> {
        self.request(HvfVcpuRunControlReason::Stop)
    }

    fn request_shutdown(&self) -> Result<HvfVcpuRunBarrierWaiter, HvfVcpuRunCoordinatorError> {
        self.request(HvfVcpuRunControlReason::Shutdown)
    }

    fn request(
        &self,
        reason: HvfVcpuRunControlReason,
    ) -> Result<HvfVcpuRunBarrierWaiter, HvfVcpuRunCoordinatorError> {
        let (sender, receiver) = mpsc::channel();
        let waiter = HvfVcpuRunBarrierWaiter { receiver };
        let mut state = self.shared.lock_state()?;

        match &mut state.phase {
            CoordinatorPhase::Draining(drain) => match &mut drain.reason {
                DrainReason::Control(active_reason) => {
                    *active_reason = active_reason.max(reason);
                    drain.waiters.push(sender);
                    return Ok(waiter);
                }
                DrainReason::Terminal { .. } | DrainReason::Submission(_) => {
                    return Err(HvfVcpuRunCoordinatorError::InvalidState(
                        CONTROL_DURING_TERMINAL_MESSAGE,
                    ));
                }
            },
            CoordinatorPhase::Paused => {
                if matches!(
                    reason,
                    HvfVcpuRunControlReason::Wakeup | HvfVcpuRunControlReason::Pause
                ) {
                    let report = HvfVcpuRunBarrierReport {
                        reason,
                        acknowledgements: Vec::new(),
                    };
                    let _ = sender.send(Ok(report));
                    return Ok(waiter);
                }
                state.phase = CoordinatorPhase::Stopped;
                let report = HvfVcpuRunBarrierReport {
                    reason,
                    acknowledgements: Vec::new(),
                };
                let _ = sender.send(Ok(report));
                return Ok(waiter);
            }
            CoordinatorPhase::Stopped => {
                if matches!(
                    reason,
                    HvfVcpuRunControlReason::Stop | HvfVcpuRunControlReason::Shutdown
                ) {
                    let report = HvfVcpuRunBarrierReport {
                        reason,
                        acknowledgements: Vec::new(),
                    };
                    let _ = sender.send(Ok(report));
                    return Ok(waiter);
                }
                return Err(HvfVcpuRunCoordinatorError::InvalidState(
                    RUNNING_PHASE_REQUIRED_MESSAGE,
                ));
            }
            CoordinatorPhase::Running => {}
            CoordinatorPhase::Failed | CoordinatorPhase::ShutDown => {
                return Err(HvfVcpuRunCoordinatorError::InvalidState(
                    RUNNING_PHASE_REQUIRED_MESSAGE,
                ));
            }
        }

        let snapshot = active_tokens(&state);
        if snapshot.is_empty() {
            state.phase = phase_after_control(reason);
            let report = HvfVcpuRunBarrierReport {
                reason,
                acknowledgements: Vec::new(),
            };
            let _ = sender.send(Ok(report));
            return Ok(waiter);
        }

        record_cancellation_debts(&mut state, &snapshot)?;
        let indexes = snapshot
            .iter()
            .map(|token| token.member_index())
            .collect::<Vec<_>>();
        state.phase = CoordinatorPhase::Draining(Box::new(DrainState {
            reason: DrainReason::Control(reason),
            remaining: snapshot,
            acknowledgements: Vec::new(),
            waiters: vec![sender],
        }));

        if let Err(source) = (self.shared.batch_cancel)(&indexes) {
            let error = HvfVcpuRunCoordinatorError::BatchCancel {
                reason: Some(reason),
                source: Box::new(source),
                acknowledgements: Vec::new(),
            };
            if let CoordinatorPhase::Draining(drain) =
                std::mem::replace(&mut state.phase, CoordinatorPhase::Failed)
            {
                let drain = *drain;
                for waiter in drain.waiters {
                    let _ = waiter.send(Err(error.clone()));
                }
            }
            return Err(error);
        }

        Ok(waiter)
    }
}

impl fmt::Debug for HvfVcpuRunControl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfVcpuRunControl")
            .field("shared", &self.shared)
            .finish()
    }
}

fn phase_after_control(reason: HvfVcpuRunControlReason) -> CoordinatorPhase {
    match reason {
        HvfVcpuRunControlReason::Wakeup => CoordinatorPhase::Running,
        HvfVcpuRunControlReason::Pause => CoordinatorPhase::Paused,
        HvfVcpuRunControlReason::Stop | HvfVcpuRunControlReason::Shutdown => {
            CoordinatorPhase::Stopped
        }
    }
}

fn active_tokens(state: &CoordinatorState) -> Vec<HvfVcpuRunToken> {
    state
        .members
        .iter()
        .filter_map(|member| member.active)
        .collect()
}

fn record_cancellation_debts(
    state: &mut CoordinatorState,
    snapshot: &[HvfVcpuRunToken],
) -> Result<(), HvfVcpuRunCoordinatorError> {
    let member_count = state.members.len();
    for token in snapshot {
        let member = state.members.get_mut(token.member_index()).ok_or(
            HvfVcpuRunCoordinatorError::InvalidMember {
                index: token.member_index(),
                member_count,
            },
        )?;
        // HVF promises a pending next-run exit, not a counted queue. Keep the
        // earliest unresolved generation so repeated controls cannot require
        // cancellation acknowledgements the framework never promised.
        member.cancellation_debt.get_or_insert(token.generation());
    }
    Ok(())
}

trait CoordinatorMember: fmt::Debug {
    fn submit(
        &self,
        token: HvfVcpuRunToken,
        dispatcher: Arc<Mutex<MmioDispatcher>>,
        completion_sender: mpsc::Sender<HvfVcpuRunCompletion>,
    ) -> Result<(), HvfVcpuRunnerError>;

    fn configure_primary(&self, registers: HvfArm64BootRegisters)
    -> Result<(), HvfVcpuRunnerError>;

    fn configure_secondary(
        &self,
        registers: HvfArm64SecondaryBootRegisters,
    ) -> Result<(), HvfVcpuRunnerError>;

    fn complete_psci(
        &self,
        token: HvfVcpuPsciCallToken,
        response: PsciCoordinatorResponse,
    ) -> Result<(), HvfVcpuRunnerError>;

    fn set_ppi_pending(&self, intid: u32) -> Result<(), HvfVcpuRunnerError>;

    fn clear_ppi_pending(&self, intid: u32) -> Result<(), HvfVcpuRunnerError>;
}

impl CoordinatorMember for HvfVcpuRunner<'_> {
    fn submit(
        &self,
        token: HvfVcpuRunToken,
        dispatcher: Arc<Mutex<MmioDispatcher>>,
        completion_sender: mpsc::Sender<HvfVcpuRunCompletion>,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.submit_run_once_and_handle_mmio_coordinated(token, dispatcher, completion_sender)
    }

    fn configure_primary(
        &self,
        registers: HvfArm64BootRegisters,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.configure_arm64_boot_registers(registers)
    }

    fn configure_secondary(
        &self,
        registers: HvfArm64SecondaryBootRegisters,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.configure_arm64_secondary_boot_registers(registers)
    }

    fn complete_psci(
        &self,
        token: HvfVcpuPsciCallToken,
        response: PsciCoordinatorResponse,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.complete_psci_call(token, response)
    }

    fn set_ppi_pending(&self, intid: u32) -> Result<(), HvfVcpuRunnerError> {
        self.set_gic_ppi_pending(intid)
    }

    fn clear_ppi_pending(&self, intid: u32) -> Result<(), HvfVcpuRunnerError> {
        self.clear_gic_ppi_pending(intid)
    }
}

struct RunCoordinator<'members, M> {
    members: &'members [M],
    dispatcher: Arc<Mutex<MmioDispatcher>>,
    completion_sender: mpsc::Sender<HvfVcpuRunCompletion>,
    completion_receiver: mpsc::Receiver<HvfVcpuRunCompletion>,
    shared: Arc<CoordinatorShared>,
}

impl<M> fmt::Debug for RunCoordinator<'_, M>
where
    M: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunCoordinator")
            .field("member_count", &self.members.len())
            .field("shared", &self.shared)
            .finish_non_exhaustive()
    }
}

impl<'members, M> RunCoordinator<'members, M>
where
    M: CoordinatorMember,
{
    fn new(
        members: &'members [M],
        mpidrs: &[u64],
        dispatcher: Arc<Mutex<MmioDispatcher>>,
        online_indexes: &[usize],
        batch_cancel: BatchCancel,
    ) -> Result<Self, HvfVcpuRunCoordinatorError> {
        if members.len() != mpidrs.len() {
            return Err(HvfVcpuRunCoordinatorError::InvalidState(
                "vCPU run coordinator topology metadata length does not match members",
            ));
        }

        let mut member_states = (0..members.len())
            .map(|_| MemberState {
                online: false,
                active: None,
                next_generation: 1,
                cancellation_debt: None,
            })
            .collect::<Vec<_>>();
        for index in online_indexes.iter().copied() {
            let member_count = member_states.len();
            let state =
                member_states
                    .get_mut(index)
                    .ok_or(HvfVcpuRunCoordinatorError::InvalidMember {
                        index,
                        member_count,
                    })?;
            if state.online {
                return Err(HvfVcpuRunCoordinatorError::DuplicateOnlineMember { index });
            }
            state.online = true;
        }

        let (completion_sender, completion_receiver) = mpsc::channel();
        Ok(Self {
            members,
            dispatcher,
            completion_sender,
            completion_receiver,
            shared: Arc::new(CoordinatorShared {
                state: Mutex::new(CoordinatorState {
                    phase: CoordinatorPhase::Running,
                    members: member_states,
                }),
                mpidrs: Arc::from(mpidrs.to_vec()),
                batch_cancel,
            }),
        })
    }

    fn control(&self) -> HvfVcpuRunControl {
        HvfVcpuRunControl {
            shared: Arc::clone(&self.shared),
        }
    }

    fn dispatch_online(&mut self) -> Result<usize, HvfVcpuRunCoordinatorError> {
        let indexes = {
            let state = self.shared.lock_state()?;
            if !matches!(state.phase, CoordinatorPhase::Running) {
                return Err(HvfVcpuRunCoordinatorError::InvalidState(
                    RUNNING_PHASE_REQUIRED_MESSAGE,
                ));
            }
            state
                .members
                .iter()
                .enumerate()
                .filter_map(|(index, member)| {
                    (member.online && member.active.is_none()).then_some(index)
                })
                .collect::<Vec<_>>()
        };

        self.submit_indexes(&indexes)
    }

    fn submit_indexes(&mut self, indexes: &[usize]) -> Result<usize, HvfVcpuRunCoordinatorError> {
        let mut submitted = 0usize;
        let mut failure = None;
        let mut immediate_error = None;

        {
            let mut state = self.shared.lock_state()?;
            if !matches!(state.phase, CoordinatorPhase::Running) {
                return Err(HvfVcpuRunCoordinatorError::InvalidState(
                    RUNNING_PHASE_REQUIRED_MESSAGE,
                ));
            }
            let member_count = state.members.len();

            for index in indexes.iter().copied() {
                let mpidr = *self.shared.mpidrs.get(index).ok_or(
                    HvfVcpuRunCoordinatorError::InvalidMember {
                        index,
                        member_count,
                    },
                )?;
                let member_state = state.members.get_mut(index).ok_or(
                    HvfVcpuRunCoordinatorError::InvalidMember {
                        index,
                        member_count,
                    },
                )?;
                if !member_state.online || member_state.active.is_some() {
                    return Err(HvfVcpuRunCoordinatorError::InvalidState(
                        ACTIVE_MEMBER_STATE_MESSAGE,
                    ));
                }
                let generation = member_state.next_generation;
                let next_generation = generation
                    .checked_add(1)
                    .ok_or(HvfVcpuRunCoordinatorError::GenerationExhausted { index, mpidr })?;
                let token = HvfVcpuRunToken::new(index, generation);
                let member =
                    self.members
                        .get(index)
                        .ok_or(HvfVcpuRunCoordinatorError::InvalidMember {
                            index,
                            member_count,
                        })?;
                match member.submit(
                    token,
                    Arc::clone(&self.dispatcher),
                    self.completion_sender.clone(),
                ) {
                    Ok(()) => {
                        member_state.active = Some(token);
                        member_state.next_generation = next_generation;
                        submitted += 1;
                    }
                    Err(source) => {
                        failure = Some(SubmissionFailure {
                            index,
                            mpidr,
                            source,
                        });
                        break;
                    }
                }
            }

            if let Some(submission) = failure.clone() {
                immediate_error = begin_submission_drain(&self.shared, &mut state, submission)?;
            }
        }

        let Some(submission) = failure else {
            return Ok(submitted);
        };
        if let Some(error) = immediate_error {
            return Err(error);
        }

        loop {
            match self.receive_event() {
                Err(error @ HvfVcpuRunCoordinatorError::Submission { .. }) => return Err(error),
                Err(error) => return Err(error),
                Ok(HvfVcpuRunEvent::Barrier(_) | HvfVcpuRunEvent::Terminal(_)) => {
                    return Err(HvfVcpuRunCoordinatorError::InvalidState(
                        UNEXPECTED_DRAIN_RESULT_MESSAGE,
                    ));
                }
                Ok(HvfVcpuRunEvent::Member(_)) => {}
            }

            let state = self.shared.lock_state()?;
            if matches!(state.phase, CoordinatorPhase::Failed) {
                return Err(HvfVcpuRunCoordinatorError::Submission {
                    index: submission.index,
                    mpidr: submission.mpidr,
                    source: Box::new(submission.source),
                    acknowledgements: Vec::new(),
                });
            }
        }
    }

    fn receive_event(&mut self) -> Result<HvfVcpuRunEvent, HvfVcpuRunCoordinatorError> {
        loop {
            let completion = self
                .completion_receiver
                .recv()
                .map_err(|_| HvfVcpuRunCoordinatorError::CompletionChannelClosed)?;
            if let Some(event) = self.process_completion(completion)? {
                return Ok(event);
            }
        }
    }

    fn process_completion(
        &mut self,
        completion: HvfVcpuRunCompletion,
    ) -> Result<Option<HvfVcpuRunEvent>, HvfVcpuRunCoordinatorError> {
        let (token, result) = completion.into_parts();
        let index = token.member_index();
        let generation = token.generation();
        let member_count = self.members.len();
        let mpidr =
            *self
                .shared
                .mpidrs
                .get(index)
                .ok_or(HvfVcpuRunCoordinatorError::InvalidMember {
                    index,
                    member_count,
                })?;
        let member_result = HvfVcpuRunMemberResult::new(
            index,
            mpidr,
            generation,
            result.map(HvfVcpuRunMemberOutcome::from),
        );
        let mut should_resubmit = false;

        let event =
            {
                let mut state = self.shared.lock_state()?;
                let member = state.members.get_mut(index).ok_or(
                    HvfVcpuRunCoordinatorError::InvalidMember {
                        index,
                        member_count,
                    },
                )?;
                if member.active != Some(token) {
                    return Err(HvfVcpuRunCoordinatorError::CompletionIdentity {
                        index,
                        generation,
                        expected: member.active.map(HvfVcpuRunToken::generation),
                    });
                }
                member.active = None;

                let absorbed_cancellation = if member_result.is_canceled()
                    && member
                        .cancellation_debt
                        .is_some_and(|debt| debt <= generation)
                {
                    member.cancellation_debt = None;
                    true
                } else {
                    false
                };

                match &mut state.phase {
                    CoordinatorPhase::Running => {
                        if member_result.terminal_priority().is_some() {
                            begin_terminal_drain(&self.shared, &mut state, member_result.clone())?
                        } else if absorbed_cancellation {
                            should_resubmit = true;
                            None
                        } else {
                            Some(HvfVcpuRunEvent::Member(member_result.clone()))
                        }
                    }
                    CoordinatorPhase::Draining(drain) => {
                        let Some(position) = drain
                            .remaining
                            .iter()
                            .position(|candidate| *candidate == token)
                        else {
                            return Err(HvfVcpuRunCoordinatorError::CompletionIdentity {
                                index,
                                generation,
                                expected: None,
                            });
                        };
                        let _ = drain.remaining.remove(position);

                        if member_result.terminal_priority().is_some()
                            && matches!(drain.reason, DrainReason::Control(_))
                        {
                            let DrainReason::Control(reason) = drain.reason else {
                                return Err(HvfVcpuRunCoordinatorError::InvalidState(
                                    TERMINAL_REPORT_MISSING_MESSAGE,
                                ));
                            };
                            drain.reason = DrainReason::Terminal {
                                superseded_control: Some(reason),
                            };
                        }

                        drain.acknowledgements.push(member_result.clone());
                        if drain.remaining.is_empty() {
                            finish_drain(&mut state)?
                        } else {
                            None
                        }
                    }
                    CoordinatorPhase::Paused
                    | CoordinatorPhase::Stopped
                    | CoordinatorPhase::Failed
                    | CoordinatorPhase::ShutDown => {
                        return Err(HvfVcpuRunCoordinatorError::CompletionIdentity {
                            index,
                            generation,
                            expected: None,
                        });
                    }
                }
            };

        if should_resubmit {
            let _ = self.submit_indexes(&[index])?;
        }

        Ok(event)
    }

    fn resume(&mut self) -> Result<(), HvfVcpuRunCoordinatorError> {
        let mut state = self.shared.lock_state()?;
        if !matches!(state.phase, CoordinatorPhase::Paused) {
            return Err(HvfVcpuRunCoordinatorError::InvalidState(
                PAUSED_PHASE_REQUIRED_MESSAGE,
            ));
        }
        state.phase = CoordinatorPhase::Running;
        Ok(())
    }

    fn set_online(&mut self, index: usize, online: bool) -> Result<(), HvfVcpuRunCoordinatorError> {
        let mut state = self.shared.lock_state()?;
        if !matches!(
            state.phase,
            CoordinatorPhase::Running | CoordinatorPhase::Paused
        ) {
            return Err(HvfVcpuRunCoordinatorError::InvalidState(
                RUNNING_PHASE_REQUIRED_MESSAGE,
            ));
        }
        let member_count = state.members.len();
        let member =
            state
                .members
                .get_mut(index)
                .ok_or(HvfVcpuRunCoordinatorError::InvalidMember {
                    index,
                    member_count,
                })?;
        if member.active.is_some() {
            return Err(HvfVcpuRunCoordinatorError::InvalidState(
                MEMBER_NOT_IDLE_MESSAGE,
            ));
        }
        if !online && member.cancellation_debt.is_some() {
            return Err(HvfVcpuRunCoordinatorError::InvalidState(
                OFFLINE_CANCELLATION_DEBT_MESSAGE,
            ));
        }
        member.online = online;
        Ok(())
    }

    fn member_operation(
        &self,
        index: usize,
        operation: &'static str,
        apply: impl FnOnce(&M) -> Result<(), HvfVcpuRunnerError>,
    ) -> Result<(), HvfVcpuRunCoordinatorError> {
        let member_count = self.members.len();
        let mpidr =
            *self
                .shared
                .mpidrs
                .get(index)
                .ok_or(HvfVcpuRunCoordinatorError::InvalidMember {
                    index,
                    member_count,
                })?;
        {
            let state = self.shared.lock_state()?;
            if !matches!(
                state.phase,
                CoordinatorPhase::Running | CoordinatorPhase::Paused
            ) {
                return Err(HvfVcpuRunCoordinatorError::InvalidState(
                    RUNNING_PHASE_REQUIRED_MESSAGE,
                ));
            }
            let member =
                state
                    .members
                    .get(index)
                    .ok_or(HvfVcpuRunCoordinatorError::InvalidMember {
                        index,
                        member_count,
                    })?;
            if member.active.is_some() {
                return Err(HvfVcpuRunCoordinatorError::InvalidState(
                    MEMBER_NOT_IDLE_MESSAGE,
                ));
            }
        }
        let member = self
            .members
            .get(index)
            .ok_or(HvfVcpuRunCoordinatorError::InvalidMember {
                index,
                member_count,
            })?;
        apply(member).map_err(|source| HvfVcpuRunCoordinatorError::MemberOperation {
            operation,
            index,
            mpidr,
            source: Box::new(source),
        })
    }
}

fn begin_submission_drain(
    shared: &CoordinatorShared,
    state: &mut CoordinatorState,
    submission: SubmissionFailure,
) -> Result<Option<HvfVcpuRunCoordinatorError>, HvfVcpuRunCoordinatorError> {
    let snapshot = active_tokens(state);
    if snapshot.is_empty() {
        state.phase = CoordinatorPhase::Failed;
        return Ok(Some(HvfVcpuRunCoordinatorError::Submission {
            index: submission.index,
            mpidr: submission.mpidr,
            source: Box::new(submission.source),
            acknowledgements: Vec::new(),
        }));
    }

    record_cancellation_debts(state, &snapshot)?;
    let indexes = snapshot
        .iter()
        .map(|token| token.member_index())
        .collect::<Vec<_>>();
    state.phase = CoordinatorPhase::Draining(Box::new(DrainState {
        reason: DrainReason::Submission(submission.clone()),
        remaining: snapshot,
        acknowledgements: Vec::new(),
        waiters: Vec::new(),
    }));
    if let Err(cleanup) = (shared.batch_cancel)(&indexes) {
        state.phase = CoordinatorPhase::Failed;
        return Ok(Some(HvfVcpuRunCoordinatorError::SubmissionCleanup {
            index: submission.index,
            mpidr: submission.mpidr,
            source: Box::new(submission.source),
            cleanup: Box::new(cleanup),
            acknowledgements: Vec::new(),
        }));
    }
    Ok(None)
}

fn begin_terminal_drain(
    shared: &CoordinatorShared,
    state: &mut CoordinatorState,
    triggering: HvfVcpuRunMemberResult,
) -> Result<Option<HvfVcpuRunEvent>, HvfVcpuRunCoordinatorError> {
    let snapshot = active_tokens(state);
    if snapshot.is_empty() {
        state.phase = CoordinatorPhase::Stopped;
        return terminal_report(vec![triggering])
            .map(HvfVcpuRunEvent::Terminal)
            .map(Some);
    }

    record_cancellation_debts(state, &snapshot)?;
    let indexes = snapshot
        .iter()
        .map(|token| token.member_index())
        .collect::<Vec<_>>();
    state.phase = CoordinatorPhase::Draining(Box::new(DrainState {
        reason: DrainReason::Terminal {
            superseded_control: None,
        },
        remaining: snapshot,
        acknowledgements: vec![triggering.clone()],
        waiters: Vec::new(),
    }));
    if let Err(source) = (shared.batch_cancel)(&indexes) {
        state.phase = CoordinatorPhase::Failed;
        return Err(HvfVcpuRunCoordinatorError::BatchCancel {
            reason: None,
            source: Box::new(source),
            acknowledgements: vec![triggering],
        });
    }
    Ok(None)
}

fn finish_drain(
    state: &mut CoordinatorState,
) -> Result<Option<HvfVcpuRunEvent>, HvfVcpuRunCoordinatorError> {
    let CoordinatorPhase::Draining(drain) =
        std::mem::replace(&mut state.phase, CoordinatorPhase::Failed)
    else {
        return Err(HvfVcpuRunCoordinatorError::InvalidState(
            UNEXPECTED_DRAIN_RESULT_MESSAGE,
        ));
    };
    let mut drain = *drain;
    drain
        .acknowledgements
        .sort_by_key(|result| (result.index(), result.generation()));

    match drain.reason {
        DrainReason::Control(reason) => {
            let report = HvfVcpuRunBarrierReport {
                reason,
                acknowledgements: drain.acknowledgements,
            };
            state.phase = phase_after_control(reason);
            for waiter in drain.waiters {
                let _ = waiter.send(Ok(report.clone()));
            }
            Ok(Some(HvfVcpuRunEvent::Barrier(report)))
        }
        DrainReason::Terminal { superseded_control } => {
            if !drain.waiters.is_empty() && superseded_control.is_none() {
                return Err(HvfVcpuRunCoordinatorError::InvalidState(
                    TERMINAL_REPORT_MISSING_MESSAGE,
                ));
            }
            let report = terminal_report(drain.acknowledgements)?;
            if let Some(reason) = superseded_control {
                for waiter in drain.waiters {
                    let _ = waiter.send(Err(
                        HvfVcpuRunCoordinatorError::ControlSupersededByTerminal {
                            reason,
                            report: Box::new(report.clone()),
                        },
                    ));
                }
            }
            state.phase = CoordinatorPhase::Stopped;
            Ok(Some(HvfVcpuRunEvent::Terminal(report)))
        }
        DrainReason::Submission(submission) => {
            state.phase = CoordinatorPhase::Failed;
            Err(HvfVcpuRunCoordinatorError::Submission {
                index: submission.index,
                mpidr: submission.mpidr,
                source: Box::new(submission.source),
                acknowledgements: drain.acknowledgements,
            })
        }
    }
}

fn terminal_report(
    mut members: Vec<HvfVcpuRunMemberResult>,
) -> Result<HvfVcpuRunTerminalReport, HvfVcpuRunCoordinatorError> {
    members.sort_by_key(|result| (result.index(), result.generation()));
    let primary = members
        .iter()
        .filter_map(|result| {
            result
                .terminal_priority()
                .map(|priority| ((priority, result.index()), result))
        })
        .min_by_key(|(key, _)| *key)
        .map(|(_, result)| result.clone())
        .ok_or(HvfVcpuRunCoordinatorError::InvalidState(
            TERMINAL_REPORT_MISSING_MESSAGE,
        ))?;
    Ok(HvfVcpuRunTerminalReport { primary, members })
}

/// Concurrent bounded-run coordinator borrowing one ordered HVF topology.
pub struct HvfVcpuRunCoordinator<'topology, 'vm> {
    topology: &'topology HvfVcpuTopology<'vm>,
    inner: RunCoordinator<'topology, HvfVcpuRunner<'vm>>,
    shutdown_complete: bool,
}

impl<'topology, 'vm> HvfVcpuRunCoordinator<'topology, 'vm> {
    pub(crate) fn new(
        topology: &'topology HvfVcpuTopology<'vm>,
        dispatcher: Arc<Mutex<MmioDispatcher>>,
        online_indexes: &[usize],
    ) -> Result<Self, HvfVcpuRunCoordinatorError> {
        let handles = Arc::new(
            topology
                .runners()
                .iter()
                .map(HvfVcpuRunner::run_cancel_handle)
                .collect::<Vec<_>>(),
        );
        let batch_cancel: BatchCancel = Arc::new(move |indexes| {
            let mut selected = Vec::with_capacity(indexes.len());
            for index in indexes.iter().copied() {
                let handle = handles
                    .get(index)
                    .ok_or(HvfVcpuRunnerError::InvalidState(BATCH_CANCEL_INDEX_MESSAGE))?;
                selected.push(handle.clone());
            }
            cancel_vcpu_run_batch_with(&selected, crate::ffi::exit_vcpus)
        });
        let inner = RunCoordinator::new(
            topology.runners(),
            topology.mpidrs(),
            dispatcher,
            online_indexes,
            batch_cancel,
        )?;
        Ok(Self {
            topology,
            inner,
            shutdown_complete: false,
        })
    }

    /// Return a cloneable topology-wide wakeup/pause/stop handle.
    pub fn control(&self) -> HvfVcpuRunControl {
        self.inner.control()
    }

    /// Submit one bounded run to every online idle member before collecting.
    pub fn dispatch_online(&mut self) -> Result<usize, HvfVcpuRunCoordinatorError> {
        self.inner.dispatch_online()
    }

    /// Wait for the next member, barrier, or fully drained terminal event.
    ///
    /// Calling this method drives completion collection for pending control
    /// waiters as well as ordinary run events.
    pub fn receive_event(&mut self) -> Result<HvfVcpuRunEvent, HvfVcpuRunCoordinatorError> {
        self.inner.receive_event()
    }

    /// Resume run submission after a completed pause barrier.
    pub fn resume(&mut self) -> Result<(), HvfVcpuRunCoordinatorError> {
        self.inner.resume()
    }

    /// Change one idle member's software online state.
    ///
    /// A member with an active run or unresolved cancellation debt cannot be
    /// moved offline. This operation does not implement PSCI by itself.
    pub fn set_online(
        &mut self,
        index: usize,
        online: bool,
    ) -> Result<(), HvfVcpuRunCoordinatorError> {
        self.inner.set_online(index, online)
    }

    /// Configure primary-style arm64 entry registers on one idle owner thread.
    pub fn configure_arm64_boot_registers(
        &self,
        index: usize,
        registers: HvfArm64BootRegisters,
    ) -> Result<(), HvfVcpuRunCoordinatorError> {
        self.inner
            .member_operation(index, "boot-register setup", |member| {
                member.configure_primary(registers)
            })
    }

    #[expect(dead_code, reason = "consumed by the multi-vCPU boot-session slice")]
    pub(crate) fn configure_arm64_secondary_boot_registers(
        &self,
        index: usize,
        registers: HvfArm64SecondaryBootRegisters,
    ) -> Result<(), HvfVcpuRunCoordinatorError> {
        self.inner
            .member_operation(index, "secondary boot-register setup", |member| {
                member.configure_secondary(registers)
            })
    }

    #[expect(dead_code, reason = "consumed by the multi-vCPU boot-session slice")]
    pub(crate) fn complete_coordinator_work(
        &self,
        index: usize,
        work: HvfVcpuCoordinatorWork,
        response: PsciCoordinatorResponse,
    ) -> Result<(), HvfVcpuRunCoordinatorError> {
        let (_, _, token, _) = work.into_parts();
        self.inner
            .member_operation(index, "deferred PSCI completion", |member| {
                member.complete_psci(token, response)
            })
    }

    /// Set one PPI pending on the selected vCPU owner thread.
    pub fn set_gic_ppi_pending(
        &self,
        index: usize,
        intid: u32,
    ) -> Result<(), HvfVcpuRunCoordinatorError> {
        self.inner
            .member_operation(index, "GIC PPI set", |member| member.set_ppi_pending(intid))
    }

    /// Clear one PPI pending on the selected vCPU owner thread.
    pub fn clear_gic_ppi_pending(
        &self,
        index: usize,
        intid: u32,
    ) -> Result<(), HvfVcpuRunCoordinatorError> {
        self.inner
            .member_operation(index, "GIC PPI clear", |member| {
                member.clear_ppi_pending(intid)
            })
    }

    /// Quiesce active runs and shut down every owner in reverse topology order.
    pub fn shutdown(&mut self) -> Result<(), HvfVcpuRunCoordinatorError> {
        if self.shutdown_complete {
            return Ok(());
        }

        let barrier_result = match self.control().request_shutdown() {
            Ok(waiter) => self.drain_waiter(&waiter),
            Err(error) => Err(error),
        };
        let shutdown_result = self.topology.shutdown();

        if shutdown_result.is_ok() {
            if let Ok(mut state) = self.inner.shared.lock_state() {
                state.phase = CoordinatorPhase::ShutDown;
            }
            self.shutdown_complete = true;
        } else if let Ok(mut state) = self.inner.shared.lock_state() {
            state.phase = CoordinatorPhase::Failed;
        }

        match (barrier_result, shutdown_result) {
            (Err(primary), Err(cleanup)) => Err(HvfVcpuRunCoordinatorError::ShutdownCleanup {
                primary: Box::new(primary),
                cleanup: Box::new(cleanup),
            }),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(source)) => Err(HvfVcpuRunCoordinatorError::Shutdown(Box::new(source))),
            (Ok(_), Ok(())) => Ok(()),
        }
    }

    fn drain_waiter(
        &mut self,
        waiter: &HvfVcpuRunBarrierWaiter,
    ) -> Result<HvfVcpuRunBarrierReport, HvfVcpuRunCoordinatorError> {
        loop {
            match waiter.try_wait() {
                Ok(result) => return result,
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(HvfVcpuRunCoordinatorError::CompletionChannelClosed);
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }

            match self.inner.receive_event()? {
                HvfVcpuRunEvent::Barrier(_) | HvfVcpuRunEvent::Terminal(_) => {}
                HvfVcpuRunEvent::Member(_) => {
                    return Err(HvfVcpuRunCoordinatorError::InvalidState(
                        UNEXPECTED_DRAIN_RESULT_MESSAGE,
                    ));
                }
            }
        }
    }
}

impl fmt::Debug for HvfVcpuRunCoordinator<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfVcpuRunCoordinator")
            .field("inner", &self.inner)
            .field("shutdown_complete", &self.shutdown_complete)
            .finish_non_exhaustive()
    }
}

impl Drop for HvfVcpuRunCoordinator<'_, '_> {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex, mpsc};

    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioDispatcher;

    use super::{
        BatchCancel, CoordinatorMember, HvfVcpuRunControlReason, HvfVcpuRunCoordinatorError,
        HvfVcpuRunEvent, RunCoordinator,
    };
    use crate::HvfVcpuRunStepOutcome;
    use crate::exit::{HvfExceptionExit, HvfHvcExit};
    use crate::psci::PsciCoordinatorResponse;
    use crate::runner::{
        HvfVcpuCoordinatedRunStepOutcome, HvfVcpuPsciCallToken, HvfVcpuRunCompletion,
        HvfVcpuRunToken, HvfVcpuRunnerError,
    };
    use crate::vcpu::{HvfArm64BootRegisters, HvfArm64SecondaryBootRegisters};

    const TEST_ERROR_MESSAGE: &str = "injected coordinator test failure";

    type FakeRunResult = Result<HvfVcpuCoordinatedRunStepOutcome, HvfVcpuRunnerError>;

    #[derive(Debug, Clone)]
    struct FakePendingRun {
        token: HvfVcpuRunToken,
        dispatcher: Arc<Mutex<MmioDispatcher>>,
        completion_sender: mpsc::Sender<HvfVcpuRunCompletion>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FakeOperation {
        ConfigurePrimary(HvfArm64BootRegisters),
        ConfigureSecondary(HvfArm64SecondaryBootRegisters),
        CompletePsci,
        SetPpi(u32),
        ClearPpi(u32),
    }

    #[derive(Debug, Default)]
    struct FakeMemberState {
        pending: VecDeque<FakePendingRun>,
        submissions: Vec<HvfVcpuRunToken>,
        submit_results: VecDeque<Result<(), HvfVcpuRunnerError>>,
        operations: Vec<FakeOperation>,
    }

    #[derive(Debug, Clone, Default)]
    struct FakeMember {
        state: Arc<Mutex<FakeMemberState>>,
    }

    impl FakeMember {
        fn queue_submit_error(&self) {
            self.state
                .lock()
                .expect("fake member state should lock")
                .submit_results
                .push_back(Err(HvfVcpuRunnerError::InvalidState(TEST_ERROR_MESSAGE)));
        }

        fn pending_tokens(&self) -> Vec<HvfVcpuRunToken> {
            self.state
                .lock()
                .expect("fake member state should lock")
                .pending
                .iter()
                .map(|pending| pending.token)
                .collect()
        }

        fn submissions(&self) -> Vec<HvfVcpuRunToken> {
            self.state
                .lock()
                .expect("fake member state should lock")
                .submissions
                .clone()
        }

        fn operations(&self) -> Vec<FakeOperation> {
            self.state
                .lock()
                .expect("fake member state should lock")
                .operations
                .clone()
        }

        fn dispatcher(&self, generation: u64) -> Arc<Mutex<MmioDispatcher>> {
            Arc::clone(
                &self
                    .state
                    .lock()
                    .expect("fake member state should lock")
                    .pending
                    .iter()
                    .find(|pending| pending.token.generation() == generation)
                    .expect("requested fake run should be pending")
                    .dispatcher,
            )
        }

        fn completion(&self, generation: u64, result: FakeRunResult) -> HvfVcpuRunCompletion {
            let mut state = self.state.lock().expect("fake member state should lock");
            let position = state
                .pending
                .iter()
                .position(|pending| pending.token.generation() == generation)
                .expect("requested fake run should be pending");
            let pending = state
                .pending
                .remove(position)
                .expect("located fake run should be removable");
            HvfVcpuRunCompletion::new(pending.token, result)
        }

        fn publish(&self, generation: u64, result: FakeRunResult) {
            let mut state = self.state.lock().expect("fake member state should lock");
            let position = state
                .pending
                .iter()
                .position(|pending| pending.token.generation() == generation)
                .expect("requested fake run should be pending");
            let pending = state
                .pending
                .remove(position)
                .expect("located fake run should be removable");
            pending
                .completion_sender
                .send(HvfVcpuRunCompletion::new(pending.token, result))
                .expect("coordinator completion receiver should remain open");
        }
    }

    impl CoordinatorMember for FakeMember {
        fn submit(
            &self,
            token: HvfVcpuRunToken,
            dispatcher: Arc<Mutex<MmioDispatcher>>,
            completion_sender: mpsc::Sender<HvfVcpuRunCompletion>,
        ) -> Result<(), HvfVcpuRunnerError> {
            let mut state = self.state.lock().expect("fake member state should lock");
            if let Some(result) = state.submit_results.pop_front() {
                result?;
            }
            state.submissions.push(token);
            state.pending.push_back(FakePendingRun {
                token,
                dispatcher,
                completion_sender,
            });
            Ok(())
        }

        fn configure_primary(
            &self,
            registers: HvfArm64BootRegisters,
        ) -> Result<(), HvfVcpuRunnerError> {
            self.state
                .lock()
                .expect("fake member state should lock")
                .operations
                .push(FakeOperation::ConfigurePrimary(registers));
            Ok(())
        }

        fn configure_secondary(
            &self,
            registers: HvfArm64SecondaryBootRegisters,
        ) -> Result<(), HvfVcpuRunnerError> {
            self.state
                .lock()
                .expect("fake member state should lock")
                .operations
                .push(FakeOperation::ConfigureSecondary(registers));
            Ok(())
        }

        fn complete_psci(
            &self,
            _token: HvfVcpuPsciCallToken,
            _response: PsciCoordinatorResponse,
        ) -> Result<(), HvfVcpuRunnerError> {
            self.state
                .lock()
                .expect("fake member state should lock")
                .operations
                .push(FakeOperation::CompletePsci);
            Ok(())
        }

        fn set_ppi_pending(&self, intid: u32) -> Result<(), HvfVcpuRunnerError> {
            self.state
                .lock()
                .expect("fake member state should lock")
                .operations
                .push(FakeOperation::SetPpi(intid));
            Ok(())
        }

        fn clear_ppi_pending(&self, intid: u32) -> Result<(), HvfVcpuRunnerError> {
            self.state
                .lock()
                .expect("fake member state should lock")
                .operations
                .push(FakeOperation::ClearPpi(intid));
            Ok(())
        }
    }

    #[derive(Debug, Clone, Default)]
    struct BatchHarness {
        calls: Arc<Mutex<Vec<Vec<usize>>>>,
        failures: Arc<Mutex<VecDeque<HvfVcpuRunnerError>>>,
    }

    impl BatchHarness {
        fn callback(&self) -> BatchCancel {
            let calls = Arc::clone(&self.calls);
            let failures = Arc::clone(&self.failures);
            Arc::new(move |indexes| {
                calls
                    .lock()
                    .expect("batch call log should lock")
                    .push(indexes.to_vec());
                if let Some(error) = failures
                    .lock()
                    .expect("batch failure queue should lock")
                    .pop_front()
                {
                    return Err(error);
                }
                Ok(())
            })
        }

        fn fail_next(&self) {
            self.failures
                .lock()
                .expect("batch failure queue should lock")
                .push_back(HvfVcpuRunnerError::InvalidState(TEST_ERROR_MESSAGE));
        }

        fn calls(&self) -> Vec<Vec<usize>> {
            self.calls
                .lock()
                .expect("batch call log should lock")
                .clone()
        }
    }

    fn fake_members(count: usize) -> Vec<FakeMember> {
        (0..count).map(|_| FakeMember::default()).collect()
    }

    fn coordinator<'members>(
        members: &'members [FakeMember],
        online_indexes: &[usize],
        batch_cancel: BatchCancel,
    ) -> Result<RunCoordinator<'members, FakeMember>, HvfVcpuRunCoordinatorError> {
        let mpidrs = (0..members.len())
            .map(|index| 0x8000_0000_u64 + index as u64)
            .collect::<Vec<_>>();
        RunCoordinator::new(
            members,
            &mpidrs,
            Arc::new(Mutex::new(MmioDispatcher::new())),
            online_indexes,
            batch_cancel,
        )
    }

    fn handled(outcome: HvfVcpuRunStepOutcome) -> FakeRunResult {
        Ok(HvfVcpuCoordinatedRunStepOutcome::Handled(outcome))
    }

    fn canceled() -> FakeRunResult {
        handled(HvfVcpuRunStepOutcome::Canceled)
    }

    fn progressed() -> FakeRunResult {
        handled(HvfVcpuRunStepOutcome::VtimerActivated)
    }

    fn unknown(reason: u32) -> FakeRunResult {
        handled(HvfVcpuRunStepOutcome::Unknown { reason })
    }

    fn test_hvc_exit() -> HvfHvcExit {
        HvfExceptionExit {
            syndrome: 0x16_u64 << 26,
            virtual_address: 0,
            physical_address: 0,
        }
        .decode_hvc()
        .expect("test HVC exit should decode")
    }

    fn guest_shutdown() -> FakeRunResult {
        handled(HvfVcpuRunStepOutcome::GuestShutdown {
            exit: test_hvc_exit(),
            function_id: 0x8400_0008,
            return_value: 0,
        })
    }

    fn guest_reset() -> FakeRunResult {
        handled(HvfVcpuRunStepOutcome::GuestReset {
            exit: test_hvc_exit(),
            function_id: 0x8400_0009,
            return_value: 0,
        })
    }

    #[test]
    fn rejects_invalid_and_duplicate_online_members() {
        let members = fake_members(2);
        let batch = BatchHarness::default();

        assert!(matches!(
            coordinator(&members, &[2], batch.callback()),
            Err(HvfVcpuRunCoordinatorError::InvalidMember {
                index: 2,
                member_count: 2
            })
        ));
        assert!(matches!(
            coordinator(&members, &[1, 1], batch.callback()),
            Err(HvfVcpuRunCoordinatorError::DuplicateOnlineMember { index: 1 })
        ));
    }

    #[test]
    fn dispatches_all_online_members_before_collecting_out_of_order() {
        let members = fake_members(3);
        let batch = BatchHarness::default();
        let mut coordinator =
            coordinator(&members, &[0, 2], batch.callback()).expect("coordinator should build");

        assert_eq!(coordinator.dispatch_online(), Ok(2));
        assert_eq!(
            members[0].pending_tokens(),
            vec![HvfVcpuRunToken::new(0, 1)]
        );
        assert!(members[1].pending_tokens().is_empty());
        assert_eq!(
            members[2].pending_tokens(),
            vec![HvfVcpuRunToken::new(2, 1)]
        );
        assert!(Arc::ptr_eq(
            &members[0].dispatcher(1),
            &members[2].dispatcher(1)
        ));

        members[2].publish(1, progressed());
        let event = coordinator
            .receive_event()
            .expect("member two completion should arrive");
        let HvfVcpuRunEvent::Member(result) = event else {
            panic!("expected one non-terminal member result");
        };
        assert_eq!(result.index(), 2);
        assert_eq!(result.generation(), 1);
        assert_eq!(members[0].pending_tokens().len(), 1);

        members[0].publish(1, progressed());
        let event = coordinator
            .receive_event()
            .expect("member zero completion should arrive");
        let HvfVcpuRunEvent::Member(result) = event else {
            panic!("expected one non-terminal member result");
        };
        assert_eq!(result.index(), 0);
    }

    #[test]
    fn rejects_stale_duplicate_and_unknown_completion_identity() {
        let members = fake_members(1);
        let batch = BatchHarness::default();
        let mut coordinator =
            coordinator(&members, &[0], batch.callback()).expect("coordinator should build");
        coordinator
            .dispatch_online()
            .expect("member should dispatch");

        let stale = HvfVcpuRunCompletion::new(HvfVcpuRunToken::new(0, 99), progressed());
        assert!(matches!(
            coordinator.process_completion(stale),
            Err(HvfVcpuRunCoordinatorError::CompletionIdentity {
                index: 0,
                generation: 99,
                expected: Some(1)
            })
        ));

        let completion = members[0].completion(1, progressed());
        assert!(matches!(
            coordinator.process_completion(completion.clone()),
            Ok(Some(HvfVcpuRunEvent::Member(_)))
        ));
        assert!(matches!(
            coordinator.process_completion(completion),
            Err(HvfVcpuRunCoordinatorError::CompletionIdentity {
                index: 0,
                generation: 1,
                expected: None
            })
        ));

        let unknown = HvfVcpuRunCompletion::new(HvfVcpuRunToken::new(7, 1), progressed());
        assert!(matches!(
            coordinator.process_completion(unknown),
            Err(HvfVcpuRunCoordinatorError::InvalidMember {
                index: 7,
                member_count: 1
            })
        ));
    }

    #[test]
    fn pause_barrier_waits_for_exact_snapshot_and_resume_is_gated() {
        let members = fake_members(2);
        let batch = BatchHarness::default();
        let mut coordinator =
            coordinator(&members, &[0, 1], batch.callback()).expect("coordinator should build");
        coordinator
            .dispatch_online()
            .expect("both members should dispatch");

        let waiter = coordinator
            .control()
            .request_pause()
            .expect("pause should start");
        assert_eq!(batch.calls(), vec![vec![0, 1]]);
        assert!(matches!(waiter.try_wait(), Err(mpsc::TryRecvError::Empty)));

        assert_eq!(
            coordinator
                .process_completion(members[1].completion(1, canceled()))
                .expect("first acknowledgement should process"),
            None
        );
        assert!(matches!(waiter.try_wait(), Err(mpsc::TryRecvError::Empty)));
        assert!(matches!(
            coordinator.member_operation(1, "PPI set", |member| member.set_ppi_pending(27)),
            Err(HvfVcpuRunCoordinatorError::InvalidState(
                super::RUNNING_PHASE_REQUIRED_MESSAGE
            ))
        ));
        let event = coordinator
            .process_completion(members[0].completion(1, canceled()))
            .expect("last acknowledgement should process")
            .expect("last acknowledgement should complete barrier");
        let HvfVcpuRunEvent::Barrier(report) = event else {
            panic!("expected completed pause barrier");
        };
        assert_eq!(report.reason(), HvfVcpuRunControlReason::Pause);
        assert_eq!(
            report
                .acknowledgements()
                .iter()
                .map(|result| result.index())
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert_eq!(waiter.wait(), Ok(report));
        assert!(coordinator.dispatch_online().is_err());
        assert_eq!(coordinator.resume(), Ok(()));
        assert_eq!(coordinator.dispatch_online(), Ok(2));
    }

    #[test]
    fn coalesces_control_reason_without_repeating_batch_cancel() {
        let members = fake_members(2);
        let batch = BatchHarness::default();
        let mut coordinator =
            coordinator(&members, &[0, 1], batch.callback()).expect("coordinator should build");
        coordinator
            .dispatch_online()
            .expect("both members should dispatch");
        let control = coordinator.control();

        let wakeup = control.request_wakeup().expect("wakeup should start");
        let pause = control.request_pause().expect("pause should coalesce");
        let stop = control.request_stop().expect("stop should coalesce");
        let shutdown = control
            .request_shutdown()
            .expect("shutdown should coalesce");
        assert_eq!(batch.calls(), vec![vec![0, 1]]);

        assert_eq!(
            coordinator
                .process_completion(members[0].completion(1, canceled()))
                .expect("first acknowledgement should process"),
            None
        );
        let event = coordinator
            .process_completion(members[1].completion(1, canceled()))
            .expect("last acknowledgement should process")
            .expect("last acknowledgement should complete barrier");
        let HvfVcpuRunEvent::Barrier(report) = event else {
            panic!("expected completed control barrier");
        };
        assert_eq!(report.reason(), HvfVcpuRunControlReason::Shutdown);
        for waiter in [wakeup, pause, stop, shutdown] {
            assert_eq!(
                waiter
                    .wait()
                    .expect("coalesced control waiter should complete")
                    .reason(),
                HvfVcpuRunControlReason::Shutdown
            );
        }
        assert!(control.request_wakeup().is_err());
    }

    #[test]
    fn empty_control_snapshot_completes_without_batch_cancel() {
        let members = fake_members(1);
        let batch = BatchHarness::default();
        let mut coordinator =
            coordinator(&members, &[0], batch.callback()).expect("coordinator should build");

        let report = coordinator
            .control()
            .request_pause()
            .expect("idle pause should start")
            .wait()
            .expect("idle pause should finish immediately");
        assert_eq!(report.reason(), HvfVcpuRunControlReason::Pause);
        assert!(report.acknowledgements().is_empty());
        assert!(batch.calls().is_empty());
        assert_eq!(coordinator.resume(), Ok(()));
    }

    #[test]
    fn stale_cancel_debt_is_absorbed_and_resubmitted_after_normal_race() {
        let members = fake_members(1);
        let batch = BatchHarness::default();
        let mut coordinator =
            coordinator(&members, &[0], batch.callback()).expect("coordinator should build");
        coordinator
            .dispatch_online()
            .expect("member should dispatch");

        let waiter = coordinator
            .control()
            .request_wakeup()
            .expect("wakeup should start");
        let event = coordinator
            .process_completion(members[0].completion(1, progressed()))
            .expect("normal race completion should process")
            .expect("normal race completion should close barrier");
        assert!(matches!(event, HvfVcpuRunEvent::Barrier(_)));
        waiter.wait().expect("wakeup barrier should complete");
        assert!(matches!(
            coordinator.set_online(0, false),
            Err(HvfVcpuRunCoordinatorError::InvalidState(
                super::OFFLINE_CANCELLATION_DEBT_MESSAGE
            ))
        ));

        assert_eq!(coordinator.dispatch_online(), Ok(1));
        assert_eq!(
            coordinator
                .process_completion(members[0].completion(2, canceled()))
                .expect("stale cancellation should be absorbed"),
            None
        );
        assert_eq!(
            members[0].pending_tokens(),
            vec![HvfVcpuRunToken::new(0, 3)]
        );
        assert!(matches!(
            coordinator.set_online(0, false),
            Err(HvfVcpuRunCoordinatorError::InvalidState(
                super::MEMBER_NOT_IDLE_MESSAGE
            ))
        ));

        let event = coordinator
            .process_completion(members[0].completion(3, progressed()))
            .expect("replacement run should complete")
            .expect("replacement run should publish progress");
        assert!(matches!(event, HvfVcpuRunEvent::Member(_)));
        assert_eq!(coordinator.set_online(0, false), Ok(()));
        assert_eq!(batch.calls(), vec![vec![0]]);
    }

    #[test]
    fn repeated_control_while_debt_is_pending_does_not_create_phantom_debt() {
        let members = fake_members(1);
        let batch = BatchHarness::default();
        let mut coordinator =
            coordinator(&members, &[0], batch.callback()).expect("coordinator should build");
        coordinator
            .dispatch_online()
            .expect("first generation should dispatch");

        let first_waiter = coordinator
            .control()
            .request_wakeup()
            .expect("first wakeup should start");
        assert!(matches!(
            coordinator
                .process_completion(members[0].completion(1, progressed()))
                .expect("normal race completion should process"),
            Some(HvfVcpuRunEvent::Barrier(_))
        ));
        first_waiter
            .wait()
            .expect("first wakeup barrier should complete");

        assert_eq!(coordinator.dispatch_online(), Ok(1));
        let second_waiter = coordinator
            .control()
            .request_wakeup()
            .expect("second wakeup should start while debt remains");
        assert!(matches!(
            coordinator
                .process_completion(members[0].completion(2, canceled()))
                .expect("one canceled completion should settle coalesced exit state"),
            Some(HvfVcpuRunEvent::Barrier(_))
        ));
        second_waiter
            .wait()
            .expect("second wakeup barrier should complete");

        assert_eq!(coordinator.dispatch_online(), Ok(1));
        assert!(matches!(
            coordinator
                .process_completion(members[0].completion(3, progressed()))
                .expect("next ordinary completion should publish"),
            Some(HvfVcpuRunEvent::Member(_))
        ));
        assert_eq!(coordinator.set_online(0, false), Ok(()));
        assert_eq!(batch.calls(), vec![vec![0], vec![0]]);
    }

    #[test]
    fn terminal_during_control_waits_for_peer_drain_before_superseding_waiter() {
        let members = fake_members(2);
        let batch = BatchHarness::default();
        let mut coordinator =
            coordinator(&members, &[0, 1], batch.callback()).expect("coordinator should build");
        coordinator
            .dispatch_online()
            .expect("both members should dispatch");
        let waiter = coordinator
            .control()
            .request_pause()
            .expect("pause should start");

        assert_eq!(
            coordinator
                .process_completion(members[1].completion(1, unknown(9)))
                .expect("terminal result should start terminal drain"),
            None
        );
        assert!(matches!(waiter.try_wait(), Err(mpsc::TryRecvError::Empty)));

        let event = coordinator
            .process_completion(members[0].completion(1, canceled()))
            .expect("peer cancellation should complete drain")
            .expect("peer cancellation should publish terminal report");
        let HvfVcpuRunEvent::Terminal(report) = event else {
            panic!("expected terminal report");
        };
        assert_eq!(report.primary().index(), 1);
        let error = waiter
            .wait()
            .expect_err("pause waiter should retain the terminal report");
        let HvfVcpuRunCoordinatorError::ControlSupersededByTerminal {
            reason,
            report: waiter_report,
        } = error
        else {
            panic!("expected terminal-superseded pause error");
        };
        assert_eq!(reason, HvfVcpuRunControlReason::Pause);
        assert_eq!(*waiter_report, report);
        assert_eq!(batch.calls(), vec![vec![0, 1]]);
    }

    #[test]
    fn terminal_report_uses_stable_failure_precedence_and_topology_order() {
        let members = fake_members(4);
        let batch = BatchHarness::default();
        let mut coordinator = coordinator(&members, &[0, 1, 2, 3], batch.callback())
            .expect("coordinator should build");
        coordinator
            .dispatch_online()
            .expect("all members should dispatch");

        assert_eq!(
            coordinator
                .process_completion(members[3].completion(1, guest_shutdown()))
                .expect("first terminal should start peer drain"),
            None
        );
        assert_eq!(batch.calls(), vec![vec![0, 1, 2]]);
        assert_eq!(
            coordinator
                .process_completion(members[2].completion(1, guest_reset()))
                .expect("reset should join terminal evidence"),
            None
        );
        assert_eq!(
            coordinator
                .process_completion(members[1].completion(1, unknown(11)))
                .expect("unknown should join terminal evidence"),
            None
        );
        let event = coordinator
            .process_completion(
                members[0].completion(1, Err(HvfVcpuRunnerError::InvalidState(TEST_ERROR_MESSAGE))),
            )
            .expect("runner failure should complete terminal drain")
            .expect("runner failure should publish terminal report");
        let HvfVcpuRunEvent::Terminal(report) = event else {
            panic!("expected terminal report");
        };
        assert_eq!(report.primary().index(), 0);
        assert!(report.primary().result().is_err());
        assert_eq!(
            report
                .members()
                .iter()
                .map(|result| result.index())
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
    }

    #[test]
    fn partial_submission_failure_batch_cancels_and_drains_submitted_prefix() {
        let members = fake_members(3);
        members[1].queue_submit_error();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let callback_calls = Arc::clone(&calls);
        let first = members[0].clone();
        let batch_cancel: BatchCancel = Arc::new(move |indexes| {
            callback_calls
                .lock()
                .expect("batch call log should lock")
                .push(indexes.to_vec());
            first.publish(1, canceled());
            Ok(())
        });
        let mut coordinator =
            coordinator(&members, &[0, 1, 2], batch_cancel).expect("coordinator should build");

        let error = coordinator
            .dispatch_online()
            .expect_err("second submission should fail");
        let HvfVcpuRunCoordinatorError::Submission {
            index,
            mpidr,
            acknowledgements,
            ..
        } = error
        else {
            panic!("expected indexed submission failure");
        };
        assert_eq!(index, 1);
        assert_eq!(mpidr, 0x8000_0001);
        assert_eq!(acknowledgements.len(), 1);
        assert_eq!(acknowledgements[0].index(), 0);
        assert_eq!(
            calls.lock().expect("batch call log should lock").as_slice(),
            &[vec![0]]
        );
        assert!(members[2].submissions().is_empty());
    }

    #[test]
    fn partial_submission_cleanup_failure_preserves_both_errors() {
        let members = fake_members(2);
        members[1].queue_submit_error();
        let batch = BatchHarness::default();
        batch.fail_next();
        let mut coordinator =
            coordinator(&members, &[0, 1], batch.callback()).expect("coordinator should build");

        let error = coordinator
            .dispatch_online()
            .expect_err("submission and prefix cancellation should fail");
        let HvfVcpuRunCoordinatorError::SubmissionCleanup {
            index,
            mpidr,
            source,
            cleanup,
            acknowledgements,
        } = error
        else {
            panic!("expected combined submission cleanup failure");
        };
        assert_eq!(index, 1);
        assert_eq!(mpidr, 0x8000_0001);
        assert_eq!(
            source.as_ref(),
            &HvfVcpuRunnerError::InvalidState(TEST_ERROR_MESSAGE)
        );
        assert_eq!(
            cleanup.as_ref(),
            &HvfVcpuRunnerError::InvalidState(TEST_ERROR_MESSAGE)
        );
        assert!(acknowledgements.is_empty());
        assert_eq!(batch.calls(), vec![vec![0]]);
    }

    #[test]
    fn batch_cancel_failure_never_reports_a_control_barrier() {
        let members = fake_members(2);
        let batch = BatchHarness::default();
        batch.fail_next();
        let mut coordinator =
            coordinator(&members, &[0, 1], batch.callback()).expect("coordinator should build");
        coordinator
            .dispatch_online()
            .expect("both members should dispatch");

        assert!(matches!(
            coordinator.control().request_pause(),
            Err(HvfVcpuRunCoordinatorError::BatchCancel {
                reason: Some(HvfVcpuRunControlReason::Pause),
                ..
            })
        ));
        assert_eq!(batch.calls(), vec![vec![0, 1]]);
        assert!(coordinator.dispatch_online().is_err());
    }

    #[test]
    fn offline_members_are_not_dispatched_or_batch_canceled() {
        let members = fake_members(3);
        let batch = BatchHarness::default();
        let mut coordinator =
            coordinator(&members, &[0, 1, 2], batch.callback()).expect("coordinator should build");
        coordinator
            .set_online(1, false)
            .expect("idle member should go offline");

        assert_eq!(coordinator.dispatch_online(), Ok(2));
        assert!(members[1].submissions().is_empty());
        let waiter = coordinator
            .control()
            .request_stop()
            .expect("stop should start");
        assert_eq!(batch.calls(), vec![vec![0, 2]]);
        assert_eq!(
            coordinator
                .process_completion(members[2].completion(1, canceled()))
                .expect("member two should acknowledge"),
            None
        );
        assert!(matches!(
            coordinator
                .process_completion(members[0].completion(1, canceled()))
                .expect("member zero should acknowledge"),
            Some(HvfVcpuRunEvent::Barrier(_))
        ));
        assert_eq!(
            waiter
                .wait()
                .expect("stop should finish")
                .acknowledgements()
                .len(),
            2
        );
    }

    #[test]
    fn indexed_operations_route_only_to_the_selected_idle_member() {
        let members = fake_members(3);
        let batch = BatchHarness::default();
        let mut coordinator =
            coordinator(&members, &[0], batch.callback()).expect("coordinator should build");
        let primary = HvfArm64BootRegisters {
            kernel_entry: GuestAddress::new(0x1000),
            fdt_address: GuestAddress::new(0x2000),
        };
        let secondary = HvfArm64SecondaryBootRegisters::new(GuestAddress::new(0x3000), 0x44);

        coordinator
            .member_operation(2, "primary", |member| member.configure_primary(primary))
            .expect("primary setup should route");
        coordinator
            .member_operation(2, "secondary", |member| {
                member.configure_secondary(secondary)
            })
            .expect("secondary setup should route");
        coordinator
            .member_operation(2, "PPI set", |member| member.set_ppi_pending(27))
            .expect("PPI set should route");
        coordinator
            .member_operation(2, "PPI clear", |member| member.clear_ppi_pending(27))
            .expect("PPI clear should route");
        assert!(members[0].operations().is_empty());
        assert!(members[1].operations().is_empty());
        assert_eq!(
            members[2].operations(),
            vec![
                FakeOperation::ConfigurePrimary(primary),
                FakeOperation::ConfigureSecondary(secondary),
                FakeOperation::SetPpi(27),
                FakeOperation::ClearPpi(27),
            ]
        );

        coordinator
            .dispatch_online()
            .expect("member zero should dispatch");
        assert!(matches!(
            coordinator.member_operation(0, "PPI set", |member| member.set_ppi_pending(30)),
            Err(HvfVcpuRunCoordinatorError::InvalidState(
                super::MEMBER_NOT_IDLE_MESSAGE
            ))
        ));
        assert!(members[0].operations().is_empty());
    }
}
