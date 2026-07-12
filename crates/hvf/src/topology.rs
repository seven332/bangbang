use std::fmt;

use bangbang_runtime::BackendError;
use bangbang_runtime::machine::MAX_SUPPORTED_VCPUS;

use crate::runner::{HvfVcpuMpidrAffinityStage, HvfVcpuRunner, HvfVcpuRunnerError};

const MAX_ORDERED_MPIDR: u64 = MAX_SUPPORTED_VCPUS as u64 - 1;

/// Allocation owned by an ordered vCPU topology constructor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfVcpuTopologyAllocation {
    Mpidrs,
    Runners,
}

impl fmt::Display for HvfVcpuTopologyAllocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mpidrs => f.write_str("MPIDR metadata"),
            Self::Runners => f.write_str("vCPU runners"),
        }
    }
}

/// Topology construction stage associated with one vCPU member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfVcpuTopologyCreateStage {
    RunnerStart,
    AffinityCommand,
    AffinityWrite,
    AffinityRead,
    AffinityVerify,
}

impl fmt::Display for HvfVcpuTopologyCreateStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RunnerStart => f.write_str("runner start"),
            Self::AffinityCommand => f.write_str("MPIDR affinity command"),
            Self::AffinityWrite => f.write_str("MPIDR affinity write"),
            Self::AffinityRead => f.write_str("MPIDR affinity readback"),
            Self::AffinityVerify => f.write_str("MPIDR affinity verification"),
        }
    }
}

/// Aggregate control operation attempted across a complete vCPU topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfVcpuTopologyOperation {
    Cancel,
    Shutdown,
}

impl fmt::Display for HvfVcpuTopologyOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancel => f.write_str("cancel"),
            Self::Shutdown => f.write_str("shutdown"),
        }
    }
}

/// One indexed member failure retained after an aggregate control attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HvfVcpuTopologyMemberFailure {
    index: usize,
    mpidr: u64,
    source: Box<HvfVcpuRunnerError>,
}

impl HvfVcpuTopologyMemberFailure {
    /// Return the stable position of the failed runner.
    pub const fn index(&self) -> usize {
        self.index
    }

    /// Return the expected MPIDR affinity of the failed runner.
    pub const fn mpidr(&self) -> u64 {
        self.mpidr
    }

    /// Return the underlying runner failure.
    pub fn source_error(&self) -> &HvfVcpuRunnerError {
        self.source.as_ref()
    }
}

impl fmt::Display for HvfVcpuTopologyMemberFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "vCPU topology member {} (MPIDR 0x{:x}): {}",
            self.index, self.mpidr, self.source
        )
    }
}

impl std::error::Error for HvfVcpuTopologyMemberFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Failure while validating, constructing, or controlling an ordered topology.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfVcpuTopologyError {
    Backend(BackendError),
    InvalidVcpuCount {
        requested: usize,
        max: usize,
    },
    HostCapacityExceeded {
        requested: u8,
        host_max: u32,
    },
    InvalidMpidr {
        index: usize,
        mpidr: u64,
        max: u64,
    },
    DuplicateMpidr {
        first_index: usize,
        second_index: usize,
        mpidr: u64,
    },
    Allocation {
        allocation: HvfVcpuTopologyAllocation,
        requested: usize,
        source: String,
    },
    Construction {
        stage: HvfVcpuTopologyCreateStage,
        index: usize,
        mpidr: u64,
        source: Box<HvfVcpuRunnerError>,
        cleanup_failures: Vec<HvfVcpuTopologyMemberFailure>,
    },
    Control {
        operation: HvfVcpuTopologyOperation,
        failures: Vec<HvfVcpuTopologyMemberFailure>,
    },
}

impl fmt::Display for HvfVcpuTopologyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(source) => write!(f, "{source}"),
            Self::InvalidVcpuCount { requested, max } => write!(
                f,
                "HVF vCPU topology count {requested} is outside 1..={max}"
            ),
            Self::HostCapacityExceeded {
                requested,
                host_max,
            } => write!(
                f,
                "HVF vCPU topology count {requested} exceeds host maximum {host_max}"
            ),
            Self::InvalidMpidr { index, mpidr, max } => write!(
                f,
                "HVF vCPU topology MPIDR 0x{mpidr:x} at index {index} exceeds supported maximum 0x{max:x}"
            ),
            Self::DuplicateMpidr {
                first_index,
                second_index,
                mpidr,
            } => write!(
                f,
                "HVF vCPU topology MPIDR 0x{mpidr:x} is duplicated at indexes {first_index} and {second_index}"
            ),
            Self::Allocation {
                allocation,
                requested,
                source,
            } => write!(
                f,
                "failed to reserve {requested} entries for HVF topology {allocation}: {source}"
            ),
            Self::Construction {
                stage,
                index,
                mpidr,
                source,
                cleanup_failures,
            } => {
                write!(
                    f,
                    "HVF vCPU topology {stage} failed at index {index} (MPIDR 0x{mpidr:x}): {source}"
                )?;
                if !cleanup_failures.is_empty() {
                    write!(
                        f,
                        "; {} runner cleanup operation(s) also failed",
                        cleanup_failures.len()
                    )?;
                }
                Ok(())
            }
            Self::Control {
                operation,
                failures,
            } => write!(
                f,
                "HVF vCPU topology {operation} failed for {} member(s)",
                failures.len()
            ),
        }
    }
}

impl std::error::Error for HvfVcpuTopologyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Backend(source) => Some(source),
            Self::Construction { source, .. } => Some(source.as_ref()),
            Self::Control { failures, .. } => failures
                .first()
                .map(|failure| failure as &(dyn std::error::Error + 'static)),
            Self::InvalidVcpuCount { .. }
            | Self::HostCapacityExceeded { .. }
            | Self::InvalidMpidr { .. }
            | Self::DuplicateMpidr { .. }
            | Self::Allocation { .. } => None,
        }
    }
}

impl From<BackendError> for HvfVcpuTopologyError {
    fn from(source: BackendError) -> Self {
        Self::Backend(source)
    }
}

trait TopologyMember {
    fn configure_mpidr_el1(&self, expected: u64) -> Result<u64, HvfVcpuRunnerError>;
    fn cancel(&self) -> Result<(), HvfVcpuRunnerError>;
    fn shutdown(&self) -> Result<(), HvfVcpuRunnerError>;
}

impl TopologyMember for HvfVcpuRunner<'_> {
    fn configure_mpidr_el1(&self, expected: u64) -> Result<u64, HvfVcpuRunnerError> {
        HvfVcpuRunner::configure_mpidr_el1(self, expected)
    }

    fn cancel(&self) -> Result<(), HvfVcpuRunnerError> {
        HvfVcpuRunner::cancel(self)
    }

    fn shutdown(&self) -> Result<(), HvfVcpuRunnerError> {
        HvfVcpuRunner::shutdown(self)
    }
}

struct CreatedTopology<M> {
    members: Vec<M>,
    mpidrs: Vec<u64>,
}

impl<M> fmt::Debug for CreatedTopology<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreatedTopology")
            .field("member_count", &self.members.len())
            .field("mpidrs", &self.mpidrs)
            .finish()
    }
}

/// Ordered permanent owner-thread vCPUs belonging to one HVF VM/GIC topology.
pub struct HvfVcpuTopology<'vm> {
    runners: Vec<HvfVcpuRunner<'vm>>,
    mpidrs: Vec<u64>,
}

impl<'vm> HvfVcpuTopology<'vm> {
    pub(crate) fn create(vcpu_count: u8) -> Result<Self, HvfVcpuTopologyError> {
        let created = create_ordered_topology_with(
            vcpu_count,
            crate::ffi::get_max_vcpu_count,
            |_, _| Ok(()),
            |_| HvfVcpuRunner::new_unconfigured(),
        )?;
        Ok(Self {
            runners: created.members,
            mpidrs: created.mpidrs,
        })
    }

    /// Return the number of vCPUs in the topology.
    pub fn len(&self) -> usize {
        self.runners.len()
    }

    /// Return whether the topology has no vCPUs.
    pub fn is_empty(&self) -> bool {
        self.runners.is_empty()
    }

    /// Return exact owner-thread-verified MPIDRs in topology order.
    pub fn mpidrs(&self) -> &[u64] {
        &self.mpidrs
    }

    /// Request cancellation from every topology member.
    ///
    /// This prerequisite primitive attempts each current singular cancel path.
    /// It does not define the batch run-epoch behavior used by later concurrent
    /// execution coordination.
    pub fn cancel(&self) -> Result<(), HvfVcpuTopologyError> {
        control_members(
            &self.runners,
            &self.mpidrs,
            HvfVcpuTopologyOperation::Cancel,
        )
    }

    /// Shut down every owner thread in reverse topology order.
    pub fn shutdown(&self) -> Result<(), HvfVcpuTopologyError> {
        shutdown_members(&self.runners, &self.mpidrs)
    }
}

impl fmt::Debug for HvfVcpuTopology<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfVcpuTopology")
            .field("mpidrs", &self.mpidrs)
            .field("runner_count", &self.runners.len())
            .finish_non_exhaustive()
    }
}

impl Drop for HvfVcpuTopology<'_> {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn validate_vcpu_count(vcpu_count: u8) -> Result<usize, HvfVcpuTopologyError> {
    if vcpu_count == 0 || vcpu_count > MAX_SUPPORTED_VCPUS {
        return Err(HvfVcpuTopologyError::InvalidVcpuCount {
            requested: usize::from(vcpu_count),
            max: usize::from(MAX_SUPPORTED_VCPUS),
        });
    }

    Ok(usize::from(vcpu_count))
}

fn validate_host_capacity(vcpu_count: u8, host_max: u32) -> Result<(), HvfVcpuTopologyError> {
    if u32::from(vcpu_count) > host_max {
        return Err(HvfVcpuTopologyError::HostCapacityExceeded {
            requested: vcpu_count,
            host_max,
        });
    }

    Ok(())
}

fn validate_mpidrs(mpidrs: &[u64]) -> Result<(), HvfVcpuTopologyError> {
    if mpidrs.is_empty() || mpidrs.len() > usize::from(MAX_SUPPORTED_VCPUS) {
        return Err(HvfVcpuTopologyError::InvalidVcpuCount {
            requested: mpidrs.len(),
            max: usize::from(MAX_SUPPORTED_VCPUS),
        });
    }

    for (index, mpidr) in mpidrs.iter().copied().enumerate() {
        if mpidr > MAX_ORDERED_MPIDR {
            return Err(HvfVcpuTopologyError::InvalidMpidr {
                index,
                mpidr,
                max: MAX_ORDERED_MPIDR,
            });
        }

        if let Some(first_index) = mpidrs
            .iter()
            .take(index)
            .position(|candidate| *candidate == mpidr)
        {
            return Err(HvfVcpuTopologyError::DuplicateMpidr {
                first_index,
                second_index: index,
                mpidr,
            });
        }
    }

    Ok(())
}

fn create_ordered_topology_with<M, Q, A, F>(
    vcpu_count: u8,
    query_host_max: Q,
    mut before_allocation: A,
    start_member: F,
) -> Result<CreatedTopology<M>, HvfVcpuTopologyError>
where
    M: TopologyMember,
    Q: FnOnce() -> Result<u32, BackendError>,
    A: FnMut(HvfVcpuTopologyAllocation, usize) -> Result<(), String>,
    F: FnMut(usize) -> Result<M, HvfVcpuRunnerError>,
{
    let requested = validate_vcpu_count(vcpu_count)?;
    let host_max = query_host_max().map_err(HvfVcpuTopologyError::Backend)?;
    validate_host_capacity(vcpu_count, host_max)?;

    before_allocation(HvfVcpuTopologyAllocation::Mpidrs, requested).map_err(|source| {
        HvfVcpuTopologyError::Allocation {
            allocation: HvfVcpuTopologyAllocation::Mpidrs,
            requested,
            source,
        }
    })?;
    let mut mpidrs = Vec::new();
    mpidrs
        .try_reserve_exact(requested)
        .map_err(|source| HvfVcpuTopologyError::Allocation {
            allocation: HvfVcpuTopologyAllocation::Mpidrs,
            requested,
            source: source.to_string(),
        })?;
    mpidrs.extend((0..vcpu_count).map(u64::from));
    validate_mpidrs(&mpidrs)?;

    create_topology_from_mpidrs_with(mpidrs, &mut before_allocation, start_member)
}

fn create_topology_from_mpidrs_with<M, A, F>(
    mpidrs: Vec<u64>,
    mut before_allocation: A,
    mut start_member: F,
) -> Result<CreatedTopology<M>, HvfVcpuTopologyError>
where
    M: TopologyMember,
    A: FnMut(HvfVcpuTopologyAllocation, usize) -> Result<(), String>,
    F: FnMut(usize) -> Result<M, HvfVcpuRunnerError>,
{
    validate_mpidrs(&mpidrs)?;
    let requested = mpidrs.len();
    before_allocation(HvfVcpuTopologyAllocation::Runners, requested).map_err(|source| {
        HvfVcpuTopologyError::Allocation {
            allocation: HvfVcpuTopologyAllocation::Runners,
            requested,
            source,
        }
    })?;
    let mut members = Vec::new();
    members
        .try_reserve_exact(requested)
        .map_err(|source| HvfVcpuTopologyError::Allocation {
            allocation: HvfVcpuTopologyAllocation::Runners,
            requested,
            source: source.to_string(),
        })?;

    for (index, mpidr) in mpidrs.iter().copied().enumerate() {
        let member = match start_member(index) {
            Ok(member) => member,
            Err(source) => {
                let cleanup_failures = cleanup_members(&members, &mpidrs);
                return Err(HvfVcpuTopologyError::Construction {
                    stage: HvfVcpuTopologyCreateStage::RunnerStart,
                    index,
                    mpidr,
                    source: Box::new(source),
                    cleanup_failures,
                });
            }
        };
        members.push(member);

        let Some(member) = members.last() else {
            return Err(HvfVcpuTopologyError::Construction {
                stage: HvfVcpuTopologyCreateStage::RunnerStart,
                index,
                mpidr,
                source: Box::new(HvfVcpuRunnerError::InvalidState(
                    "created vCPU topology member is missing",
                )),
                cleanup_failures: cleanup_members(&members, &mpidrs),
            });
        };
        let actual = match member.configure_mpidr_el1(mpidr) {
            Ok(actual) => actual,
            Err(source) => {
                let stage = topology_stage_for_runner_error(&source);
                let cleanup_failures = cleanup_members(&members, &mpidrs);
                return Err(HvfVcpuTopologyError::Construction {
                    stage,
                    index,
                    mpidr,
                    source: Box::new(source),
                    cleanup_failures,
                });
            }
        };
        if actual != mpidr {
            let source = HvfVcpuRunnerError::MpidrAffinity {
                stage: HvfVcpuMpidrAffinityStage::Verify,
                expected: mpidr,
                actual: Some(actual),
                source: None,
            };
            let cleanup_failures = cleanup_members(&members, &mpidrs);
            return Err(HvfVcpuTopologyError::Construction {
                stage: HvfVcpuTopologyCreateStage::AffinityVerify,
                index,
                mpidr,
                source: Box::new(source),
                cleanup_failures,
            });
        }
    }

    Ok(CreatedTopology { members, mpidrs })
}

fn topology_stage_for_runner_error(source: &HvfVcpuRunnerError) -> HvfVcpuTopologyCreateStage {
    match source {
        HvfVcpuRunnerError::MpidrAffinity { stage, .. } => match stage {
            HvfVcpuMpidrAffinityStage::Write => HvfVcpuTopologyCreateStage::AffinityWrite,
            HvfVcpuMpidrAffinityStage::Read => HvfVcpuTopologyCreateStage::AffinityRead,
            HvfVcpuMpidrAffinityStage::Verify => HvfVcpuTopologyCreateStage::AffinityVerify,
        },
        _ => HvfVcpuTopologyCreateStage::AffinityCommand,
    }
}

fn cleanup_members<M: TopologyMember>(
    members: &[M],
    mpidrs: &[u64],
) -> Vec<HvfVcpuTopologyMemberFailure> {
    let mut failures = Vec::new();
    for (index, (member, mpidr)) in members.iter().zip(mpidrs).enumerate().rev() {
        if let Err(source) = member.shutdown() {
            failures.push(HvfVcpuTopologyMemberFailure {
                index,
                mpidr: *mpidr,
                source: Box::new(source),
            });
        }
    }
    failures
}

fn control_members<M: TopologyMember>(
    members: &[M],
    mpidrs: &[u64],
    operation: HvfVcpuTopologyOperation,
) -> Result<(), HvfVcpuTopologyError> {
    let mut failures = Vec::new();
    for (index, (member, mpidr)) in members.iter().zip(mpidrs).enumerate() {
        let result = match operation {
            HvfVcpuTopologyOperation::Cancel => member.cancel(),
            HvfVcpuTopologyOperation::Shutdown => member.shutdown(),
        };
        if let Err(source) = result {
            failures.push(HvfVcpuTopologyMemberFailure {
                index,
                mpidr: *mpidr,
                source: Box::new(source),
            });
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(HvfVcpuTopologyError::Control {
            operation,
            failures,
        })
    }
}

fn shutdown_members<M: TopologyMember>(
    members: &[M],
    mpidrs: &[u64],
) -> Result<(), HvfVcpuTopologyError> {
    let failures = cleanup_members(members, mpidrs);
    if failures.is_empty() {
        Ok(())
    } else {
        Err(HvfVcpuTopologyError::Control {
            operation: HvfVcpuTopologyOperation::Shutdown,
            failures,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use bangbang_runtime::BackendError;

    use super::{
        CreatedTopology, HvfVcpuTopologyAllocation, HvfVcpuTopologyCreateStage,
        HvfVcpuTopologyError, HvfVcpuTopologyOperation, TopologyMember,
        create_ordered_topology_with, create_topology_from_mpidrs_with, shutdown_members,
    };
    use crate::runner::{HvfVcpuMpidrAffinityStage, HvfVcpuRunnerError};

    type EventLog = Arc<Mutex<Vec<String>>>;

    #[derive(Clone)]
    enum ConfigureBehavior {
        Echo,
        Actual(u64),
        Error(HvfVcpuRunnerError),
    }

    struct FakeMember {
        index: usize,
        log: EventLog,
        configure: ConfigureBehavior,
        cancel_result: Result<(), HvfVcpuRunnerError>,
        shutdown_results: Mutex<VecDeque<Result<(), HvfVcpuRunnerError>>>,
    }

    impl FakeMember {
        fn echo(index: usize, log: &EventLog) -> Self {
            Self {
                index,
                log: Arc::clone(log),
                configure: ConfigureBehavior::Echo,
                cancel_result: Ok(()),
                shutdown_results: Mutex::new(VecDeque::new()),
            }
        }

        fn with_configure(index: usize, log: &EventLog, configure: ConfigureBehavior) -> Self {
            Self {
                index,
                log: Arc::clone(log),
                configure,
                cancel_result: Ok(()),
                shutdown_results: Mutex::new(VecDeque::new()),
            }
        }

        fn with_control(
            index: usize,
            log: &EventLog,
            cancel_result: Result<(), HvfVcpuRunnerError>,
            shutdown_results: Vec<Result<(), HvfVcpuRunnerError>>,
        ) -> Self {
            Self {
                index,
                log: Arc::clone(log),
                configure: ConfigureBehavior::Echo,
                cancel_result,
                shutdown_results: Mutex::new(shutdown_results.into()),
            }
        }

        fn record(&self, event: impl Into<String>) {
            self.log
                .lock()
                .expect("event log should lock")
                .push(event.into());
        }
    }

    impl TopologyMember for FakeMember {
        fn configure_mpidr_el1(&self, expected: u64) -> Result<u64, HvfVcpuRunnerError> {
            self.record(format!("configure:{}:{expected}", self.index));
            match &self.configure {
                ConfigureBehavior::Echo => Ok(expected),
                ConfigureBehavior::Actual(actual) => Ok(*actual),
                ConfigureBehavior::Error(source) => Err(source.clone()),
            }
        }

        fn cancel(&self) -> Result<(), HvfVcpuRunnerError> {
            self.record(format!("cancel:{}", self.index));
            self.cancel_result.clone()
        }

        fn shutdown(&self) -> Result<(), HvfVcpuRunnerError> {
            self.record(format!("shutdown:{}", self.index));
            self.shutdown_results
                .lock()
                .expect("shutdown results should lock")
                .pop_front()
                .unwrap_or(Ok(()))
        }
    }

    impl Drop for FakeMember {
        fn drop(&mut self) {
            self.record(format!("drop:{}", self.index));
        }
    }

    fn log() -> EventLog {
        Arc::new(Mutex::new(Vec::new()))
    }

    fn events(log: &EventLog) -> Vec<String> {
        log.lock().expect("event log should lock").clone()
    }

    fn injected(message: &'static str) -> HvfVcpuRunnerError {
        HvfVcpuRunnerError::Backend(BackendError::InvalidState(message))
    }

    fn affinity_error(
        stage: HvfVcpuMpidrAffinityStage,
        expected: u64,
        message: &'static str,
    ) -> HvfVcpuRunnerError {
        HvfVcpuRunnerError::MpidrAffinity {
            stage,
            expected,
            actual: None,
            source: Some(BackendError::InvalidState(message)),
        }
    }

    #[test]
    fn invalid_counts_fail_before_query_allocation_or_member_start() {
        for requested in [0, 33] {
            let query_calls = AtomicUsize::new(0);
            let allocation_calls = AtomicUsize::new(0);
            let start_calls = AtomicUsize::new(0);
            let result: Result<CreatedTopology<FakeMember>, _> = create_ordered_topology_with(
                requested,
                || {
                    query_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(64)
                },
                |_, _| {
                    allocation_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
                |_| {
                    start_calls.fetch_add(1, Ordering::SeqCst);
                    Err(injected("unexpected member start after invalid count"))
                },
            );

            assert_eq!(
                result.expect_err("invalid count should fail"),
                HvfVcpuTopologyError::InvalidVcpuCount {
                    requested: usize::from(requested),
                    max: 32,
                }
            );
            assert_eq!(query_calls.load(Ordering::SeqCst), 0);
            assert_eq!(allocation_calls.load(Ordering::SeqCst), 0);
            assert_eq!(start_calls.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn capacity_query_failure_and_host_limit_precede_allocation() {
        let allocation_calls = AtomicUsize::new(0);
        let query_error: Result<CreatedTopology<FakeMember>, _> = create_ordered_topology_with(
            2,
            || {
                Err(BackendError::Hypervisor(
                    "injected capacity failure".to_string(),
                ))
            },
            |_, _| {
                allocation_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            |_| Err(injected("unexpected member start after capacity failure")),
        );
        assert_eq!(
            query_error.expect_err("query should fail"),
            HvfVcpuTopologyError::Backend(BackendError::Hypervisor(
                "injected capacity failure".to_string()
            ))
        );

        let insufficient: Result<CreatedTopology<FakeMember>, _> = create_ordered_topology_with(
            2,
            || Ok(1),
            |_, _| {
                allocation_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            |_| Err(injected("unexpected member start after host limit")),
        );
        assert_eq!(
            insufficient.expect_err("host limit should fail"),
            HvfVcpuTopologyError::HostCapacityExceeded {
                requested: 2,
                host_max: 1,
            }
        );
        assert_eq!(allocation_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn portable_maximum_builds_exact_ordered_mpidrs() {
        let log = log();
        let created = create_ordered_topology_with(
            32,
            || Ok(64),
            |_, _| Ok(()),
            |index| {
                log.lock()
                    .expect("event log should lock")
                    .push(format!("start:{index}"));
                Ok(FakeMember::echo(index, &log))
            },
        )
        .expect("portable maximum should build");

        assert_eq!(
            created.mpidrs,
            (0_u8..32).map(u64::from).collect::<Vec<_>>()
        );
        assert_eq!(created.members.len(), 32);
    }

    #[test]
    fn each_allocation_failure_precedes_member_start() {
        for failed_allocation in [
            HvfVcpuTopologyAllocation::Mpidrs,
            HvfVcpuTopologyAllocation::Runners,
        ] {
            let start_calls = AtomicUsize::new(0);
            let result: Result<CreatedTopology<FakeMember>, _> = create_ordered_topology_with(
                2,
                || Ok(2),
                |allocation, _| {
                    if allocation == failed_allocation {
                        Err("injected allocation failure".to_string())
                    } else {
                        Ok(())
                    }
                },
                |_| {
                    start_calls.fetch_add(1, Ordering::SeqCst);
                    Err(injected("unexpected member start after allocation failure"))
                },
            );

            assert_eq!(
                result.expect_err("allocation should fail"),
                HvfVcpuTopologyError::Allocation {
                    allocation: failed_allocation,
                    requested: 2,
                    source: "injected allocation failure".to_string(),
                }
            );
            assert_eq!(start_calls.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn invalid_and_duplicate_affinities_precede_runner_allocation() {
        for (mpidrs, expected) in [
            (
                Vec::new(),
                HvfVcpuTopologyError::InvalidVcpuCount {
                    requested: 0,
                    max: 32,
                },
            ),
            (
                vec![32],
                HvfVcpuTopologyError::InvalidMpidr {
                    index: 0,
                    mpidr: 32,
                    max: 31,
                },
            ),
            (
                vec![0, 0],
                HvfVcpuTopologyError::DuplicateMpidr {
                    first_index: 0,
                    second_index: 1,
                    mpidr: 0,
                },
            ),
        ] {
            let allocation_calls = AtomicUsize::new(0);
            let start_calls = AtomicUsize::new(0);
            let result: Result<CreatedTopology<FakeMember>, _> = create_topology_from_mpidrs_with(
                mpidrs,
                |_, _| {
                    allocation_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
                |_| {
                    start_calls.fetch_add(1, Ordering::SeqCst);
                    Err(injected("unexpected member start after invalid affinity"))
                },
            );

            assert_eq!(result.expect_err("affinity should fail"), expected);
            assert_eq!(allocation_calls.load(Ordering::SeqCst), 0);
            assert_eq!(start_calls.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn runner_start_failure_cleans_successful_prefix_in_reverse_order() {
        let log = log();
        let result = create_ordered_topology_with(
            3,
            || Ok(3),
            |_, _| Ok(()),
            |index| {
                log.lock()
                    .expect("event log should lock")
                    .push(format!("start:{index}"));
                if index == 2 {
                    Err(injected("injected runner start failure"))
                } else {
                    Ok(FakeMember::echo(index, &log))
                }
            },
        );

        let HvfVcpuTopologyError::Construction {
            stage,
            index,
            mpidr,
            source,
            cleanup_failures,
        } = result.expect_err("runner start should fail")
        else {
            panic!("unexpected topology error");
        };
        assert_eq!(stage, HvfVcpuTopologyCreateStage::RunnerStart);
        assert_eq!((index, mpidr), (2, 2));
        assert_eq!(*source, injected("injected runner start failure"));
        assert!(cleanup_failures.is_empty());
        let events = events(&log);
        let shutdowns = events
            .iter()
            .filter(|event| event.starts_with("shutdown:"))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(shutdowns, ["shutdown:1", "shutdown:0"]);
    }

    #[test]
    fn affinity_failure_cleans_current_member_and_preserves_cleanup_failures() {
        let log = log();
        let source = affinity_error(
            HvfVcpuMpidrAffinityStage::Write,
            1,
            "injected affinity write failure",
        );
        let cleanup = injected("injected cleanup failure");
        let result = create_ordered_topology_with(
            2,
            || Ok(2),
            |_, _| Ok(()),
            |index| {
                if index == 1 {
                    Ok(FakeMember {
                        index,
                        log: Arc::clone(&log),
                        configure: ConfigureBehavior::Error(source.clone()),
                        cancel_result: Ok(()),
                        shutdown_results: Mutex::new(VecDeque::from([Err(cleanup.clone())])),
                    })
                } else {
                    Ok(FakeMember::echo(index, &log))
                }
            },
        );

        let HvfVcpuTopologyError::Construction {
            stage,
            index,
            source: actual_source,
            cleanup_failures,
            ..
        } = result.expect_err("affinity should fail")
        else {
            panic!("unexpected topology error");
        };
        assert_eq!(stage, HvfVcpuTopologyCreateStage::AffinityWrite);
        assert_eq!(index, 1);
        assert_eq!(*actual_source, source);
        assert_eq!(cleanup_failures.len(), 1);
        assert_eq!(cleanup_failures[0].index(), 1);
        assert_eq!(cleanup_failures[0].source_error(), &cleanup);
        let shutdowns = events(&log)
            .into_iter()
            .filter(|event| event.starts_with("shutdown:"))
            .collect::<Vec<_>>();
        assert_eq!(shutdowns, ["shutdown:1", "shutdown:0"]);
    }

    #[test]
    fn affinity_mismatch_is_typed_and_cleans_current_member() {
        let log = log();
        let result = create_ordered_topology_with(
            2,
            || Ok(2),
            |_, _| Ok(()),
            |index| {
                if index == 1 {
                    Ok(FakeMember::with_configure(
                        index,
                        &log,
                        ConfigureBehavior::Actual(9),
                    ))
                } else {
                    Ok(FakeMember::echo(index, &log))
                }
            },
        );

        let HvfVcpuTopologyError::Construction {
            stage,
            index,
            source,
            cleanup_failures,
            ..
        } = result.expect_err("mismatch should fail")
        else {
            panic!("unexpected topology error");
        };
        assert_eq!(stage, HvfVcpuTopologyCreateStage::AffinityVerify);
        assert_eq!(index, 1);
        assert_eq!(
            *source,
            HvfVcpuRunnerError::MpidrAffinity {
                stage: HvfVcpuMpidrAffinityStage::Verify,
                expected: 1,
                actual: Some(9),
                source: None,
            }
        );
        assert!(cleanup_failures.is_empty());
        let shutdowns = events(&log)
            .into_iter()
            .filter(|event| event.starts_with("shutdown:"))
            .collect::<Vec<_>>();
        assert_eq!(shutdowns, ["shutdown:1", "shutdown:0"]);
    }

    #[test]
    fn affinity_read_and_channel_failures_keep_stage_and_cleanup_order() {
        for (source, expected_stage) in [
            (
                affinity_error(
                    HvfVcpuMpidrAffinityStage::Read,
                    1,
                    "injected affinity read failure",
                ),
                HvfVcpuTopologyCreateStage::AffinityRead,
            ),
            (
                HvfVcpuRunnerError::ChannelClosed("injected affinity channel failure"),
                HvfVcpuTopologyCreateStage::AffinityCommand,
            ),
        ] {
            let log = log();
            let result = create_ordered_topology_with(
                2,
                || Ok(2),
                |_, _| Ok(()),
                |index| {
                    if index == 1 {
                        Ok(FakeMember::with_configure(
                            index,
                            &log,
                            ConfigureBehavior::Error(source.clone()),
                        ))
                    } else {
                        Ok(FakeMember::echo(index, &log))
                    }
                },
            );

            let HvfVcpuTopologyError::Construction {
                stage,
                index,
                source: actual_source,
                cleanup_failures,
                ..
            } = result.expect_err("affinity stage should fail")
            else {
                panic!("unexpected topology error");
            };
            assert_eq!(stage, expected_stage);
            assert_eq!(index, 1);
            assert_eq!(*actual_source, source);
            assert!(cleanup_failures.is_empty());
            let shutdowns = events(&log)
                .into_iter()
                .filter(|event| event.starts_with("shutdown:"))
                .collect::<Vec<_>>();
            assert_eq!(shutdowns, ["shutdown:1", "shutdown:0"]);
        }
    }

    #[test]
    fn aggregate_control_attempts_every_member_and_retains_indexes() {
        let log = log();
        let created = create_ordered_topology_with(
            3,
            || Ok(3),
            |_, _| Ok(()),
            |index| {
                let cancel_result = if index == 0 || index == 2 {
                    Err(injected("injected cancel failure"))
                } else {
                    Ok(())
                };
                let shutdown_results = if index == 1 {
                    vec![Err(injected("injected shutdown failure"))]
                } else {
                    Vec::new()
                };
                Ok(FakeMember::with_control(
                    index,
                    &log,
                    cancel_result,
                    shutdown_results,
                ))
            },
        )
        .expect("topology should build");

        let cancel = super::control_members(
            &created.members,
            &created.mpidrs,
            HvfVcpuTopologyOperation::Cancel,
        )
        .expect_err("two cancels should fail");
        let HvfVcpuTopologyError::Control {
            operation,
            failures,
        } = cancel
        else {
            panic!("unexpected cancel error");
        };
        assert_eq!(operation, HvfVcpuTopologyOperation::Cancel);
        assert_eq!(
            failures
                .iter()
                .map(|failure| failure.index())
                .collect::<Vec<_>>(),
            [0, 2]
        );

        let shutdown = shutdown_members(&created.members, &created.mpidrs)
            .expect_err("one shutdown should fail");
        let HvfVcpuTopologyError::Control {
            operation,
            failures,
        } = shutdown
        else {
            panic!("unexpected shutdown error");
        };
        assert_eq!(operation, HvfVcpuTopologyOperation::Shutdown);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].index(), 1);

        let events = events(&log);
        let cancels = events
            .iter()
            .filter(|event| event.starts_with("cancel:"))
            .cloned()
            .collect::<Vec<_>>();
        let shutdowns = events
            .iter()
            .filter(|event| event.starts_with("shutdown:"))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(cancels, ["cancel:0", "cancel:1", "cancel:2"]);
        assert_eq!(shutdowns, ["shutdown:2", "shutdown:1", "shutdown:0"]);
    }
}
