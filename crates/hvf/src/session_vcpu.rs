use std::collections::VecDeque;
use std::fmt;
use std::ops::Deref;
use std::sync::{Arc, Mutex};

use bangbang_runtime::memory::GuestAddress;
use bangbang_runtime::mmio::MmioDispatcher;

use crate::HvfVcpuRunStepOutcome;
use crate::coordinator::{
    HvfVcpuCoordinatorWork, HvfVcpuRunAdmission, HvfVcpuRunControl, HvfVcpuRunControlReason,
    HvfVcpuRunCoordinator, HvfVcpuRunCoordinatorError, HvfVcpuRunEvent, HvfVcpuRunMemberOutcome,
    HvfVcpuRunMemberResult, HvfVcpuRunTerminalReport,
};
use crate::memory::HvfGuestMemoryMappingError;
use crate::psci::{
    PsciCoordinatorRequest, PsciCoordinatorResponse, PsciCpuOnBegin, PsciCpuOnToken, PsciCpuOnWork,
    PsciCpuPowerCoordinator,
};
use crate::runner::{HvfVcpuRunner, HvfVcpuRunnerError};
use crate::vcpu::HvfArm64SecondaryBootRegisters;

/// Failure while translating aggregate vCPU events into boot-session steps.
#[derive(Debug)]
pub enum HvfArm64BootVcpuError {
    /// The mapped guest memory needed for PSCI entry validation is unavailable.
    GuestMemory {
        source: Box<HvfGuestMemoryMappingError>,
    },
    /// One identified owner-thread run failed.
    Member {
        index: usize,
        mpidr: u64,
        generation: u64,
        source: Box<HvfVcpuRunnerError>,
    },
    /// The aggregate coordinator rejected an indexed lifecycle operation.
    Coordinator {
        stage: &'static str,
        index: usize,
        mpidr: u64,
        cleanup_failed: bool,
        source: Box<HvfVcpuRunCoordinatorError>,
    },
    /// The PSCI power transaction rejected an internal transition.
    Power {
        stage: &'static str,
        index: usize,
        mpidr: u64,
    },
}

impl fmt::Display for HvfArm64BootVcpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GuestMemory { source } => {
                write!(f, "boot-session vCPU guest-memory access failed: {source}")
            }
            Self::Member {
                index,
                mpidr,
                generation,
                source,
            } => write!(
                f,
                "boot-session vCPU {index} (MPIDR 0x{mpidr:x}) generation {generation} failed: {source}"
            ),
            Self::Coordinator {
                stage,
                index,
                mpidr,
                cleanup_failed,
                source,
            } => write!(
                f,
                "boot-session vCPU {index} (MPIDR 0x{mpidr:x}) {stage} failed (cleanup_failed={cleanup_failed}): {source}"
            ),
            Self::Power {
                stage,
                index,
                mpidr,
            } => write!(
                f,
                "boot-session vCPU {index} (MPIDR 0x{mpidr:x}) PSCI transaction failed during {stage}"
            ),
        }
    }
}

impl std::error::Error for HvfArm64BootVcpuError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::GuestMemory { source } => Some(source.as_ref()),
            Self::Member { source, .. } => Some(source.as_ref()),
            Self::Coordinator { source, .. } => Some(source.as_ref()),
            Self::Power { .. } => None,
        }
    }
}

impl From<HvfVcpuRunnerError> for HvfArm64BootVcpuError {
    fn from(source: HvfVcpuRunnerError) -> Self {
        Self::Member {
            index: 0,
            mpidr: 0,
            generation: 0,
            source: Box::new(source),
        }
    }
}

#[derive(Debug)]
struct IndexedBootStep {
    index: usize,
    outcome: HvfVcpuRunStepOutcome,
}

/// Boot-session aggregate that owns every vCPU runner and its PSCI power model.
#[derive(Debug)]
pub(crate) struct HvfArm64BootVcpuSession<'vm> {
    coordinator: HvfVcpuRunCoordinator<'vm>,
    power: PsciCpuPowerCoordinator,
    pending_steps: VecDeque<Result<IndexedBootStep, HvfArm64BootVcpuError>>,
    last_step_index: usize,
    last_terminal_report: Option<HvfVcpuRunTerminalReport>,
}

impl<'vm> HvfArm64BootVcpuSession<'vm> {
    pub(crate) const fn new(
        coordinator: HvfVcpuRunCoordinator<'vm>,
        power: PsciCpuPowerCoordinator,
    ) -> Self {
        Self {
            coordinator,
            power,
            pending_steps: VecDeque::new(),
            last_step_index: 0,
            last_terminal_report: None,
        }
    }

    pub(crate) fn from_restored_runner(
        runner: HvfVcpuRunner<'vm>,
        mpidr: u64,
        dispatcher: Arc<Mutex<MmioDispatcher>>,
    ) -> Result<Self, HvfVcpuRunCoordinatorError> {
        let coordinator = HvfVcpuRunCoordinator::from_runner(runner, mpidr, dispatcher, true)?;
        let power = PsciCpuPowerCoordinator::new(&[mpidr]).map_err(|_| {
            HvfVcpuRunCoordinatorError::InvalidState(
                "restored vCPU power topology is incompatible with its MPIDR",
            )
        })?;
        Ok(Self::new(coordinator, power))
    }

    pub(crate) fn member_count(&self) -> usize {
        self.coordinator.member_count()
    }

    pub(crate) fn mpidrs(&self) -> &[u64] {
        self.coordinator.mpidrs()
    }

    pub(crate) fn primary_mpidr(&self) -> u64 {
        self.coordinator.primary_mpidr()
    }

    pub(crate) fn shutdown(&mut self) -> Result<(), HvfVcpuRunCoordinatorError> {
        self.coordinator.shutdown()
    }

    pub(crate) fn control(&self) -> HvfVcpuRunControl {
        self.coordinator.control()
    }

    pub(crate) fn singular_runner(&self) -> Result<&HvfVcpuRunner<'vm>, HvfVcpuRunnerError> {
        if self.member_count() != 1 {
            return Err(HvfVcpuRunnerError::InvalidState(
                "direct boot-session vCPU steps require exactly one topology member",
            ));
        }
        Ok(self.coordinator.primary_runner())
    }

    pub(crate) const fn last_terminal_report(&self) -> Option<&HvfVcpuRunTerminalReport> {
        self.last_terminal_report.as_ref()
    }

    pub(crate) fn run_step(
        &mut self,
        mut entry_is_valid: impl FnMut(u64) -> bool,
    ) -> Result<HvfVcpuRunStepOutcome, HvfArm64BootVcpuError> {
        if self.pending_steps.is_empty() {
            self.coordinator
                .dispatch_online()
                .map_err(|source| self.coordinator_error("run dispatch", 0, source))?;
            let event = self
                .coordinator
                .receive_event()
                .map_err(|source| self.coordinator_error("event collection", 0, source))?;
            self.enqueue_event(event, &mut entry_is_valid)?;
        }

        let step = self
            .pending_steps
            .pop_front()
            .ok_or_else(|| self.power_error("event translation", 0))??;
        self.last_step_index = step.index;
        Ok(step.outcome)
    }

    pub(crate) fn set_last_step_ppi_pending(
        &self,
        intid: u32,
    ) -> Result<(), HvfArm64BootVcpuError> {
        self.coordinator
            .set_gic_ppi_pending(self.last_step_index, intid)
            .map_err(|source| {
                self.coordinator_error("virtual-timer PPI delivery", self.last_step_index, source)
            })
    }

    fn enqueue_event(
        &mut self,
        event: HvfVcpuRunEvent,
        entry_is_valid: &mut impl FnMut(u64) -> bool,
    ) -> Result<(), HvfArm64BootVcpuError> {
        match event {
            HvfVcpuRunEvent::Member(member) => {
                let step = self.process_member(member, entry_is_valid, true);
                self.pending_steps.push_back(step);
            }
            HvfVcpuRunEvent::Barrier(report) => {
                let mut sentinel_index = 0;
                let cpu_on_admission = barrier_cpu_on_admission(report.reason());
                for member in report.acknowledgements().iter().cloned() {
                    sentinel_index = member.index();
                    if member_is_canceled(&member)
                        || (cpu_on_admission.is_none() && member_has_coordinator_work(&member))
                    {
                        continue;
                    }
                    let step = self.process_member(
                        member,
                        entry_is_valid,
                        cpu_on_admission.unwrap_or(false),
                    );
                    self.pending_steps.push_back(step);
                }
                self.pending_steps.push_back(Ok(IndexedBootStep {
                    index: sentinel_index,
                    outcome: HvfVcpuRunStepOutcome::Canceled,
                }));
            }
            HvfVcpuRunEvent::Terminal(report) => {
                self.last_terminal_report = Some(report.clone());
                let primary = (report.primary().index(), report.primary().generation());
                for member in report.members().iter().cloned() {
                    if (member.index(), member.generation()) == primary
                        || member_is_canceled(&member)
                        || member_is_terminal(&member)
                        || member_has_coordinator_work(&member)
                    {
                        continue;
                    }
                    let step = self.process_member(member, entry_is_valid, false);
                    self.pending_steps.push_back(step);
                }
                let primary = self.process_member(report.primary().clone(), entry_is_valid, false);
                self.pending_steps.push_back(primary);
            }
        }
        Ok(())
    }

    fn process_member(
        &mut self,
        member: HvfVcpuRunMemberResult,
        entry_is_valid: &mut impl FnMut(u64) -> bool,
        cpu_on_admission: bool,
    ) -> Result<IndexedBootStep, HvfArm64BootVcpuError> {
        let index = member.index();
        let mpidr = member.mpidr();
        let generation = member.generation();
        let outcome = match member.result() {
            Ok(HvfVcpuRunMemberOutcome::Handled(outcome)) => *outcome,
            Ok(HvfVcpuRunMemberOutcome::Coordinator(work)) => {
                self.process_coordinator_work(index, *work, entry_is_valid, cpu_on_admission)?
            }
            Err(source) => {
                return Err(HvfArm64BootVcpuError::Member {
                    index,
                    mpidr,
                    generation,
                    source: Box::new(source.clone()),
                });
            }
        };
        Ok(IndexedBootStep { index, outcome })
    }

    fn process_coordinator_work(
        &mut self,
        caller_index: usize,
        work: HvfVcpuCoordinatorWork,
        entry_is_valid: &mut impl FnMut(u64) -> bool,
        cpu_on_admission: bool,
    ) -> Result<HvfVcpuRunStepOutcome, HvfArm64BootVcpuError> {
        let (exit, function_id, _, request) = work.into_parts();
        let response = match request {
            PsciCoordinatorRequest::AffinityInfo(request) => {
                PsciCoordinatorResponse::AffinityInfo(self.power.affinity_info(request))
            }
            PsciCoordinatorRequest::CpuOn(request) => {
                return self.process_cpu_on(
                    caller_index,
                    work,
                    request,
                    entry_is_valid,
                    cpu_on_admission,
                );
            }
        };
        self.complete_caller(
            caller_index,
            work,
            response,
            "AFFINITY_INFO completion",
            false,
        )?;
        Ok(HvfVcpuRunStepOutcome::Hvc {
            exit,
            function_id,
            return_value: response.return_value(),
        })
    }

    fn process_cpu_on(
        &mut self,
        caller_index: usize,
        work: HvfVcpuCoordinatorWork,
        request: crate::psci::PsciCpuOnRequest,
        entry_is_valid: &mut impl FnMut(u64) -> bool,
        cpu_on_admission: bool,
    ) -> Result<HvfVcpuRunStepOutcome, HvfArm64BootVcpuError> {
        let (exit, function_id, _, _) = work.into_parts();
        let begin = self
            .power
            .begin_cpu_on(request, entry_is_valid)
            .map_err(|_| self.power_error("CPU_ON validation", caller_index))?;
        let PsciCpuOnBegin::Pending(cpu_on) = begin else {
            let PsciCpuOnBegin::Complete(response) = begin else {
                return Err(self.power_error("CPU_ON validation", caller_index));
            };
            let response = PsciCoordinatorResponse::CpuOn(response);
            self.complete_caller(caller_index, work, response, "CPU_ON completion", false)?;
            return Ok(HvfVcpuRunStepOutcome::Hvc {
                exit,
                function_id,
                return_value: response.return_value(),
            });
        };

        if !cpu_on_admission {
            return self.complete_failed_cpu_on(caller_index, work, cpu_on);
        }

        let target_index = cpu_on.target_index();
        let registers = HvfArm64SecondaryBootRegisters::new(
            GuestAddress::new(cpu_on.request().entry_point()),
            cpu_on.request().context_id(),
        );
        if self
            .coordinator
            .configure_arm64_secondary_boot_registers(target_index, registers)
            .is_err()
        {
            return self.complete_failed_cpu_on(caller_index, work, cpu_on);
        }

        let admission = match self
            .coordinator
            .activate_and_dispatch_member(target_index)
            .map_err(|source| {
                self.coordinator_error("target-only run admission", target_index, source)
            })? {
            Some(admission) => admission,
            None => return self.complete_failed_cpu_on(caller_index, work, cpu_on),
        };
        self.validate_admission(target_index, admission)?;
        let response = self
            .power
            .finish_target_setup(cpu_on.token(), true)
            .map_err(|_| self.power_error("target setup commit", target_index))?;

        let response = PsciCoordinatorResponse::CpuOn(response);
        if let Err(error) = self.complete_caller(
            caller_index,
            work,
            response,
            "CPU_ON success completion",
            false,
        ) {
            let cleanup_failed = self.cleanup_admitted_cpu_on(cpu_on.token());
            return Err(with_cleanup_evidence(error, cleanup_failed));
        }
        if self.power.commit_caller_completion(cpu_on.token()).is_err() {
            let cleanup_failed = self.cleanup_admitted_cpu_on(cpu_on.token());
            return Err(HvfArm64BootVcpuError::Power {
                stage: if cleanup_failed {
                    "caller commit and admitted-target cleanup"
                } else {
                    "caller commit"
                },
                index: target_index,
                mpidr: self.mpidr(target_index),
            });
        }
        self.power
            .mark_target_entered(cpu_on.token())
            .map_err(|_| self.power_error("target entered transition", target_index))?;

        Ok(HvfVcpuRunStepOutcome::Hvc {
            exit,
            function_id,
            return_value: response.return_value(),
        })
    }

    fn complete_failed_cpu_on(
        &mut self,
        caller_index: usize,
        work: HvfVcpuCoordinatorWork,
        cpu_on: PsciCpuOnWork,
    ) -> Result<HvfVcpuRunStepOutcome, HvfArm64BootVcpuError> {
        let (exit, function_id, _, _) = work.into_parts();
        let target_index = cpu_on.target_index();
        let response = self
            .power
            .finish_target_setup(cpu_on.token(), false)
            .map_err(|_| self.power_error("target setup failure", target_index))?;
        let response = PsciCoordinatorResponse::CpuOn(response);
        if let Err(error) = self.complete_caller(
            caller_index,
            work,
            response,
            "CPU_ON failure completion",
            false,
        ) {
            let cleanup_failed = self
                .power
                .abandon_caller_completion(cpu_on.token())
                .is_err();
            return Err(with_cleanup_evidence(error, cleanup_failed));
        }
        self.power
            .commit_caller_completion(cpu_on.token())
            .map_err(|_| self.power_error("CPU_ON failure commit", target_index))?;
        Ok(HvfVcpuRunStepOutcome::Hvc {
            exit,
            function_id,
            return_value: response.return_value(),
        })
    }

    fn complete_caller(
        &self,
        caller_index: usize,
        work: HvfVcpuCoordinatorWork,
        response: PsciCoordinatorResponse,
        stage: &'static str,
        cleanup_failed: bool,
    ) -> Result<(), HvfArm64BootVcpuError> {
        self.coordinator
            .complete_coordinator_work(caller_index, work, response)
            .map_err(|source| HvfArm64BootVcpuError::Coordinator {
                stage,
                index: caller_index,
                mpidr: self.mpidr(caller_index),
                cleanup_failed,
                source: Box::new(source),
            })
    }

    fn validate_admission(
        &self,
        target_index: usize,
        admission: HvfVcpuRunAdmission,
    ) -> Result<(), HvfArm64BootVcpuError> {
        if admission.index() == target_index && admission.mpidr() == self.mpidr(target_index) {
            let _generation = admission.generation();
            Ok(())
        } else {
            Err(self.power_error("target admission identity", target_index))
        }
    }

    fn cleanup_admitted_cpu_on(&mut self, token: PsciCpuOnToken) -> bool {
        if self.power.abandon_caller_completion(token).is_err() {
            return true;
        }
        self.power.mark_target_entered(token).is_err()
    }

    fn coordinator_error(
        &self,
        stage: &'static str,
        index: usize,
        source: HvfVcpuRunCoordinatorError,
    ) -> HvfArm64BootVcpuError {
        HvfArm64BootVcpuError::Coordinator {
            stage,
            index,
            mpidr: self.mpidr(index),
            cleanup_failed: false,
            source: Box::new(source),
        }
    }

    fn power_error(&self, stage: &'static str, index: usize) -> HvfArm64BootVcpuError {
        HvfArm64BootVcpuError::Power {
            stage,
            index,
            mpidr: self.mpidr(index),
        }
    }

    fn mpidr(&self, index: usize) -> u64 {
        self.coordinator.mpidrs().get(index).copied().unwrap_or(0)
    }
}

fn member_is_canceled(member: &HvfVcpuRunMemberResult) -> bool {
    matches!(
        member.result(),
        Ok(HvfVcpuRunMemberOutcome::Handled(
            HvfVcpuRunStepOutcome::Canceled
        ))
    )
}

const fn barrier_cpu_on_admission(reason: HvfVcpuRunControlReason) -> Option<bool> {
    match reason {
        HvfVcpuRunControlReason::Wakeup => Some(true),
        HvfVcpuRunControlReason::Pause => Some(false),
        HvfVcpuRunControlReason::Stop | HvfVcpuRunControlReason::Shutdown => None,
    }
}

fn member_has_coordinator_work(member: &HvfVcpuRunMemberResult) -> bool {
    matches!(member.result(), Ok(HvfVcpuRunMemberOutcome::Coordinator(_)))
}

fn member_is_terminal(member: &HvfVcpuRunMemberResult) -> bool {
    matches!(
        member.result(),
        Err(_)
            | Ok(HvfVcpuRunMemberOutcome::Handled(
                HvfVcpuRunStepOutcome::Unknown { .. }
                    | HvfVcpuRunStepOutcome::GuestReset { .. }
                    | HvfVcpuRunStepOutcome::GuestShutdown { .. }
            ))
    )
}

fn with_cleanup_evidence(
    error: HvfArm64BootVcpuError,
    cleanup_failed: bool,
) -> HvfArm64BootVcpuError {
    match error {
        HvfArm64BootVcpuError::Coordinator {
            stage,
            index,
            mpidr,
            source,
            ..
        } => HvfArm64BootVcpuError::Coordinator {
            stage,
            index,
            mpidr,
            cleanup_failed,
            source,
        },
        error => error,
    }
}

impl<'vm> Deref for HvfArm64BootVcpuSession<'vm> {
    type Target = HvfVcpuRunner<'vm>;

    fn deref(&self) -> &Self::Target {
        self.coordinator.primary_runner()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, mpsc};

    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioDispatcher;

    use super::{HvfArm64BootVcpuSession, barrier_cpu_on_admission};
    use crate::HvfVcpuRunStepOutcome;
    use crate::coordinator::{HvfVcpuRunControlReason, HvfVcpuRunCoordinator, HvfVcpuRunEvent};
    use crate::psci::{PsciCpuPowerCoordinator, PsciStatus};
    use crate::runner::tests::{
        start_coordinated_psci_run_step_recording_runner,
        start_secondary_configure_recording_runner,
    };
    use crate::vcpu::{HvfArm64SecondaryBootRegisters, HvfRegister};

    const PSCI_CPU_ON_64: u64 = 0xc400_0003;
    const SECONDARY_ENTRY: u64 = 0x8020_0000;
    const SECONDARY_CONTEXT: u64 = 0xfeed_face_cafe_beef;

    type CpuOnSessionFixture = (
        HvfArm64BootVcpuSession<'static>,
        mpsc::Receiver<HvfRegister>,
        mpsc::Receiver<(HvfRegister, u64)>,
        mpsc::Receiver<HvfArm64SecondaryBootRegisters>,
    );

    fn cpu_on_session(fail_secondary_setup: bool) -> CpuOnSessionFixture {
        let (primary, reads, writes) = start_coordinated_psci_run_step_recording_runner(
            PSCI_CPU_ON_64,
            [1, SECONDARY_ENTRY, SECONDARY_CONTEXT],
            0,
            false,
        );
        let (secondary, configured) =
            start_secondary_configure_recording_runner(fail_secondary_setup);
        let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));
        let coordinator = HvfVcpuRunCoordinator::from_test_runners(
            vec![primary, secondary],
            vec![0, 1],
            dispatcher,
            &[0],
        )
        .expect("test coordinator should build");
        let power =
            PsciCpuPowerCoordinator::new(&[0, 1]).expect("test power topology should build");
        (
            HvfArm64BootVcpuSession::new(coordinator, power),
            reads,
            writes,
            configured,
        )
    }

    #[test]
    fn barrier_policy_admits_rejects_or_abandons_cpu_on_by_reason() {
        assert_eq!(
            barrier_cpu_on_admission(HvfVcpuRunControlReason::Wakeup),
            Some(true)
        );
        assert_eq!(
            barrier_cpu_on_admission(HvfVcpuRunControlReason::Pause),
            Some(false)
        );
        assert_eq!(
            barrier_cpu_on_admission(HvfVcpuRunControlReason::Stop),
            None
        );
        assert_eq!(
            barrier_cpu_on_admission(HvfVcpuRunControlReason::Shutdown),
            None
        );
    }

    #[test]
    fn cpu_on_session_configures_and_admits_target_before_success() {
        let (mut session, reads, writes, configured) = cpu_on_session(false);

        let outcome = session
            .run_step(|entry| entry == SECONDARY_ENTRY)
            .expect("CPU_ON should complete through the session adapter");

        assert!(matches!(
            outcome,
            HvfVcpuRunStepOutcome::Hvc {
                function_id: PSCI_CPU_ON_64,
                return_value: 0,
                ..
            }
        ));
        assert_eq!(
            reads.try_iter().collect::<Vec<_>>(),
            vec![
                HvfRegister::X0,
                HvfRegister::X1,
                HvfRegister::X2,
                HvfRegister::X3,
            ]
        );
        assert_eq!(
            configured
                .recv()
                .expect("secondary setup should be observed"),
            HvfArm64SecondaryBootRegisters::new(
                GuestAddress::new(SECONDARY_ENTRY),
                SECONDARY_CONTEXT,
            )
        );
        assert_eq!(
            writes.recv().expect("caller completion should be observed"),
            (HvfRegister::X0, 0)
        );
        let HvfVcpuRunEvent::Member(target) = session
            .coordinator
            .receive_event()
            .expect("admitted target should complete")
        else {
            panic!("expected target member completion");
        };
        assert_eq!(target.index(), 1);
        assert!(matches!(
            target.result(),
            Ok(crate::coordinator::HvfVcpuRunMemberOutcome::Handled(
                HvfVcpuRunStepOutcome::Canceled
            ))
        ));
        session.shutdown().expect("test session should shut down");
    }

    #[test]
    fn cpu_on_session_reports_internal_failure_without_target_admission() {
        let (mut session, _reads, writes, configured) = cpu_on_session(true);

        let outcome = session
            .run_step(|entry| entry == SECONDARY_ENTRY)
            .expect("setup failure should return a PSCI response");

        assert!(matches!(
            outcome,
            HvfVcpuRunStepOutcome::Hvc {
                function_id: PSCI_CPU_ON_64,
                return_value,
                ..
            } if return_value == PsciStatus::InternalFailure.return_value()
        ));
        assert_eq!(
            configured
                .recv()
                .expect("failed secondary setup should be observed"),
            HvfArm64SecondaryBootRegisters::new(
                GuestAddress::new(SECONDARY_ENTRY),
                SECONDARY_CONTEXT,
            )
        );
        assert_eq!(
            writes
                .recv()
                .expect("caller failure response should be observed"),
            (HvfRegister::X0, PsciStatus::InternalFailure.return_value())
        );
        session.shutdown().expect("test session should shut down");
    }
}
