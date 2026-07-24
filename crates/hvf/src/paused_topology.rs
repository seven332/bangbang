use std::fmt;

use bangbang_runtime::machine::MAX_SUPPORTED_VCPUS;

use crate::gic::validate_gic_ppi_pending_intid;

pub(crate) const PSCI_CPU_SUSPEND_32: u64 = 0x8400_0001;
pub(crate) const PSCI_CPU_SUSPEND_64: u64 = 0xc400_0001;

/// Architecturally distinct PSCI CPU_SUSPEND call forms accepted by the
/// coordinated arm64 runner.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HvfArm64CpuSuspendConvention {
    /// The SMCCC 32-bit calling convention.
    Call32,
    /// The SMCCC 64-bit calling convention.
    Call64,
}

impl HvfArm64CpuSuspendConvention {
    /// Return the PSCI function ID placed in X0 by the guest.
    pub const fn function_id(self) -> u64 {
        match self {
            Self::Call32 => PSCI_CPU_SUSPEND_32,
            Self::Call64 => PSCI_CPU_SUSPEND_64,
        }
    }

    /// Interpret one supported PSCI CPU_SUSPEND function ID.
    pub const fn from_function_id(function_id: u64) -> Option<Self> {
        match function_id {
            PSCI_CPU_SUSPEND_32 => Some(Self::Call32),
            PSCI_CPU_SUSPEND_64 => Some(Self::Call64),
            _ => None,
        }
    }
}

impl fmt::Debug for HvfArm64CpuSuspendConvention {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Call32 => f.write_str("Call32"),
            Self::Call64 => f.write_str("Call64"),
        }
    }
}

/// Redacted architectural continuation for one deferred PSCI CPU_SUSPEND.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfArm64StableCpuSuspendState {
    convention: HvfArm64CpuSuspendConvention,
    arguments: [u64; 3],
    return_pc: u64,
}

impl HvfArm64StableCpuSuspendState {
    /// Construct one checked deferred CPU_SUSPEND continuation.
    pub fn new(
        convention: HvfArm64CpuSuspendConvention,
        arguments: [u64; 3],
        return_pc: u64,
    ) -> Result<Self, HvfArm64StablePausedTopologyBuildError> {
        if !return_pc.is_multiple_of(4) {
            return Err(HvfArm64StablePausedTopologyBuildError::MisalignedCpuSuspendReturnPc);
        }
        Ok(Self {
            convention,
            arguments,
            return_pc,
        })
    }

    /// Return the closed PSCI call convention.
    pub const fn convention(&self) -> HvfArm64CpuSuspendConvention {
        self.convention
    }

    /// Return the architectural X1-X3 arguments.
    pub const fn arguments(&self) -> [u64; 3] {
        self.arguments
    }

    /// Return the post-trap guest PC at which execution will resume.
    pub const fn return_pc(&self) -> u64 {
        self.return_pc
    }
}

impl fmt::Debug for HvfArm64StableCpuSuspendState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfArm64StableCpuSuspendState")
            .field("convention", &self.convention)
            .field("architectural_state", &"<redacted>")
            .finish()
    }
}

/// Stable software disposition for one member of a paused arm64 topology.
#[derive(Clone, PartialEq, Eq)]
pub enum HvfArm64StableVcpuDisposition {
    /// The permanent owner exists but PSCI reports the member offline.
    Offline,
    /// The member is online and becomes runnable only after explicit resume.
    Runnable,
    /// The member is online in a deferred PSCI CPU_SUSPEND.
    Suspended(HvfArm64StableCpuSuspendState),
}

impl fmt::Debug for HvfArm64StableVcpuDisposition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Offline => f.write_str("Offline"),
            Self::Runnable => f.write_str("Runnable"),
            Self::Suspended(_) => f.write_str("Suspended(<redacted>)"),
        }
    }
}

/// One topology-ordered member in a stable paused arm64 lifecycle graph.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfArm64StablePausedTopologyMember {
    index: usize,
    mpidr: u64,
    disposition: HvfArm64StableVcpuDisposition,
}

impl HvfArm64StablePausedTopologyMember {
    /// Construct one member. Whole-topology canonicality is checked by
    /// [`HvfArm64StablePausedTopologyState::new`].
    pub const fn new(index: usize, mpidr: u64, disposition: HvfArm64StableVcpuDisposition) -> Self {
        Self {
            index,
            mpidr,
            disposition,
        }
    }

    /// Return the stable topology index.
    pub const fn index(&self) -> usize {
        self.index
    }

    /// Return the canonical MPIDR bound to the member.
    pub const fn mpidr(&self) -> u64 {
        self.mpidr
    }

    /// Return the member's stable lifecycle disposition.
    pub const fn disposition(&self) -> &HvfArm64StableVcpuDisposition {
        &self.disposition
    }
}

impl fmt::Debug for HvfArm64StablePausedTopologyMember {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfArm64StablePausedTopologyMember")
            .field("index", &self.index)
            .field("mpidr", &"<redacted>")
            .field("disposition", &self.disposition)
            .finish()
    }
}

/// Canonical, redacted software lifecycle state captured at a completed
/// topology-wide pause barrier.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfArm64StablePausedTopologyState {
    virtual_timer_intid: u32,
    members: Vec<HvfArm64StablePausedTopologyMember>,
}

impl HvfArm64StablePausedTopologyState {
    /// Validate and construct one stable paused topology state.
    pub fn new(
        virtual_timer_intid: u32,
        members: Vec<HvfArm64StablePausedTopologyMember>,
    ) -> Result<Self, HvfArm64StablePausedTopologyBuildError> {
        let member_count = members.len();
        let max = usize::from(MAX_SUPPORTED_VCPUS);
        if member_count == 0 || member_count > max {
            return Err(HvfArm64StablePausedTopologyBuildError::InvalidMemberCount {
                member_count,
                max,
            });
        }
        if validate_gic_ppi_pending_intid(virtual_timer_intid).is_err() {
            return Err(HvfArm64StablePausedTopologyBuildError::InvalidVirtualTimerPpi);
        }
        for (position, member) in members.iter().enumerate() {
            if member.index != position {
                return Err(
                    HvfArm64StablePausedTopologyBuildError::NonCanonicalMemberIndex {
                        position,
                        member_index: member.index,
                    },
                );
            }
            if member.mpidr != position as u64 {
                return Err(
                    HvfArm64StablePausedTopologyBuildError::NonCanonicalMemberMpidr {
                        index: position,
                    },
                );
            }
        }
        if matches!(
            members.first().map(|member| &member.disposition),
            Some(HvfArm64StableVcpuDisposition::Offline)
        ) {
            return Err(HvfArm64StablePausedTopologyBuildError::PrimaryOffline);
        }

        Ok(Self {
            virtual_timer_intid,
            members,
        })
    }

    /// Return the validated EL1 virtual-timer PPI.
    pub const fn virtual_timer_intid(&self) -> u32 {
        self.virtual_timer_intid
    }

    /// Return members in canonical topology order.
    pub fn members(&self) -> &[HvfArm64StablePausedTopologyMember] {
        &self.members
    }
}

impl fmt::Debug for HvfArm64StablePausedTopologyState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfArm64StablePausedTopologyState")
            .field("member_count", &self.members.len())
            .field("state", &"<redacted>")
            .finish()
    }
}

/// Rejection while constructing a stable paused topology value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfArm64StablePausedTopologyBuildError {
    /// The member count is empty or exceeds the product topology limit.
    InvalidMemberCount { member_count: usize, max: usize },
    /// The topology vector and explicit member index disagree.
    NonCanonicalMemberIndex {
        position: usize,
        member_index: usize,
    },
    /// The member MPIDR is not the canonical arm64 MPIDR for its index.
    NonCanonicalMemberMpidr { index: usize },
    /// The primary member cannot be represented offline.
    PrimaryOffline,
    /// The virtual-timer interrupt is not a valid PPI.
    InvalidVirtualTimerPpi,
    /// The deferred CPU_SUSPEND return PC is not instruction-aligned.
    MisalignedCpuSuspendReturnPc,
}

impl fmt::Display for HvfArm64StablePausedTopologyBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMemberCount { member_count, max } => write!(
                f,
                "stable paused topology member count {member_count} is outside 1..={max}"
            ),
            Self::NonCanonicalMemberIndex {
                position,
                member_index,
            } => write!(
                f,
                "stable paused topology position {position} contains member index {member_index}"
            ),
            Self::NonCanonicalMemberMpidr { index } => write!(
                f,
                "stable paused topology member {index} has a noncanonical MPIDR"
            ),
            Self::PrimaryOffline => {
                f.write_str("stable paused topology primary member cannot be offline")
            }
            Self::InvalidVirtualTimerPpi => {
                f.write_str("stable paused topology virtual timer interrupt is not a PPI")
            }
            Self::MisalignedCpuSuspendReturnPc => {
                f.write_str("stable CPU_SUSPEND return PC is not instruction-aligned")
            }
        }
    }
}

impl std::error::Error for HvfArm64StablePausedTopologyBuildError {}

#[cfg(test)]
mod tests {
    use bangbang_runtime::machine::MAX_SUPPORTED_VCPUS;

    use super::{
        HvfArm64CpuSuspendConvention, HvfArm64StableCpuSuspendState,
        HvfArm64StablePausedTopologyBuildError, HvfArm64StablePausedTopologyMember,
        HvfArm64StablePausedTopologyState, HvfArm64StableVcpuDisposition,
    };

    fn runnable(index: usize) -> HvfArm64StablePausedTopologyMember {
        HvfArm64StablePausedTopologyMember::new(
            index,
            index as u64,
            HvfArm64StableVcpuDisposition::Runnable,
        )
    }

    #[test]
    fn validates_canonical_member_shapes_and_accessors() {
        let suspended = HvfArm64StableCpuSuspendState::new(
            HvfArm64CpuSuspendConvention::Call64,
            [1, 2, 3],
            0x8000,
        )
        .expect("aligned continuation should build");
        let state = HvfArm64StablePausedTopologyState::new(
            27,
            vec![
                runnable(0),
                HvfArm64StablePausedTopologyMember::new(
                    1,
                    1,
                    HvfArm64StableVcpuDisposition::Offline,
                ),
                HvfArm64StablePausedTopologyMember::new(
                    2,
                    2,
                    HvfArm64StableVcpuDisposition::Suspended(suspended.clone()),
                ),
            ],
        )
        .expect("canonical topology should build");

        assert_eq!(state.virtual_timer_intid(), 27);
        assert_eq!(state.members().len(), 3);
        assert_eq!(state.members()[2].index(), 2);
        assert_eq!(state.members()[2].mpidr(), 2);
        assert_eq!(
            suspended.convention().function_id(),
            HvfArm64CpuSuspendConvention::Call64.function_id()
        );
        assert_eq!(suspended.arguments(), [1, 2, 3]);
        assert_eq!(suspended.return_pc(), 0x8000);
    }

    #[test]
    fn rejects_invalid_topology_and_continuation_boundaries() {
        assert!(matches!(
            HvfArm64StablePausedTopologyState::new(27, Vec::new()),
            Err(HvfArm64StablePausedTopologyBuildError::InvalidMemberCount {
                member_count: 0,
                ..
            })
        ));
        assert_eq!(
            HvfArm64StablePausedTopologyState::new(15, vec![runnable(0)]),
            Err(HvfArm64StablePausedTopologyBuildError::InvalidVirtualTimerPpi)
        );
        assert_eq!(
            HvfArm64StablePausedTopologyState::new(
                27,
                vec![HvfArm64StablePausedTopologyMember::new(
                    1,
                    0,
                    HvfArm64StableVcpuDisposition::Runnable
                )]
            ),
            Err(
                HvfArm64StablePausedTopologyBuildError::NonCanonicalMemberIndex {
                    position: 0,
                    member_index: 1
                }
            )
        );
        assert_eq!(
            HvfArm64StablePausedTopologyState::new(
                27,
                vec![HvfArm64StablePausedTopologyMember::new(
                    0,
                    1,
                    HvfArm64StableVcpuDisposition::Runnable
                )]
            ),
            Err(HvfArm64StablePausedTopologyBuildError::NonCanonicalMemberMpidr { index: 0 })
        );
        assert_eq!(
            HvfArm64StablePausedTopologyState::new(
                27,
                vec![HvfArm64StablePausedTopologyMember::new(
                    0,
                    0,
                    HvfArm64StableVcpuDisposition::Offline
                )]
            ),
            Err(HvfArm64StablePausedTopologyBuildError::PrimaryOffline)
        );
        assert_eq!(
            HvfArm64StableCpuSuspendState::new(HvfArm64CpuSuspendConvention::Call32, [0; 3], 3),
            Err(HvfArm64StablePausedTopologyBuildError::MisalignedCpuSuspendReturnPc)
        );
    }

    #[test]
    fn accepts_the_maximum_topology_and_rejects_one_more_member() {
        let max = usize::from(MAX_SUPPORTED_VCPUS);
        let maximum = (0..max).map(runnable).collect();
        let state = HvfArm64StablePausedTopologyState::new(27, maximum)
            .expect("maximum supported topology should build");
        assert_eq!(state.members().len(), max);

        let oversized = (0..=max).map(runnable).collect();
        assert_eq!(
            HvfArm64StablePausedTopologyState::new(27, oversized),
            Err(HvfArm64StablePausedTopologyBuildError::InvalidMemberCount {
                member_count: max + 1,
                max,
            })
        );
    }

    #[test]
    fn debug_output_redacts_architectural_values() {
        let suspended = HvfArm64StableCpuSuspendState::new(
            HvfArm64CpuSuspendConvention::Call32,
            [0x1111, 0x2222, 0x3333],
            0x4444,
        )
        .expect("test continuation should build");
        let state = HvfArm64StablePausedTopologyState::new(
            27,
            vec![HvfArm64StablePausedTopologyMember::new(
                0,
                0,
                HvfArm64StableVcpuDisposition::Suspended(suspended),
            )],
        )
        .expect("test topology should build");

        let formatted = format!("{state:?}");
        assert!(formatted.contains("<redacted>"));
        for raw in ["1111", "2222", "3333", "4444"] {
            assert!(!formatted.contains(raw));
        }
    }
}
