//! Compatibility adapter from the test-only evidence protocol to production credentials.

use std::os::fd::RawFd;

use crate::credential as production;
use crate::elevated_probe as probe;

/// Complete value-free postcondition of one process credential transition.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CredentialTransition {
    state: probe::CredentialSelfState,
    prefix: probe::CredentialPrefix,
}

impl CredentialTransition {
    /// Returns the exact final self-state class.
    #[must_use]
    pub const fn state(self) -> probe::CredentialSelfState {
        self.state
    }

    /// Returns the complete ordered prefix.
    #[must_use]
    pub const fn prefix(self) -> probe::CredentialPrefix {
        self.prefix
    }
}

impl std::fmt::Debug for CredentialTransition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CredentialTransition(<redacted>)")
    }
}

/// Opaque initial peer state retained only for later semantic comparison.
pub struct PeerBaseline(crate::macos::credential::PeerBaseline);

impl std::fmt::Debug for PeerBaseline {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PeerBaseline(<redacted>)")
    }
}

/// Performs the exact no-chroot credential transition for one evidence endpoint.
pub fn transition_process(
    mode: probe::ProbeMode,
    target_uid: u32,
    target_gid: u32,
) -> Result<CredentialTransition, probe::CredentialFailureValue> {
    let target = target(mode, target_uid, target_gid).map_err(invalid_failure)?;
    crate::macos::credential::transition_process(target)
        .map(map_transition)
        .map_err(map_failure)
}

/// Re-attests the exact local post-transition identity before runtime work.
pub fn attest_current_process(
    mode: probe::ProbeMode,
    target_uid: u32,
    target_gid: u32,
) -> Result<probe::CredentialSelfState, probe::ProbeErrorCategory> {
    if !mode.continues_runtime() {
        return Err(probe::ProbeErrorCategory::InvalidInput);
    }
    let target = target(mode, target_uid, target_gid)?;
    crate::macos::credential::attest_current_process(target)
        .map(map_state)
        .map_err(map_category)
}

/// Captures the initial connected stream/datagram peer observation.
pub fn observe_initial_peer(
    stream: RawFd,
    datagram: RawFd,
    expected_pid: libc::pid_t,
    socket_creator_pid: libc::pid_t,
    target_uid: u32,
    target_gid: u32,
) -> Result<(probe::PeerObservation, PeerBaseline), probe::ProbeErrorCategory> {
    let target = production::CredentialTarget::new(target_uid, target_gid)
        .map_err(|_| probe::ProbeErrorCategory::InvalidInput)?;
    let (observation, baseline) = crate::macos::credential::observe_initial_peer(
        stream,
        datagram,
        expected_pid,
        socket_creator_pid,
        target,
    )
    .map_err(map_category)?;
    Ok((map_observation(observation)?, PeerBaseline(baseline)))
}

/// Captures one later peer observation relative to the exact initial baseline.
pub fn observe_later_peer(
    stream: RawFd,
    datagram: RawFd,
    expected_pid: libc::pid_t,
    socket_creator_pid: libc::pid_t,
    target_uid: u32,
    target_gid: u32,
    baseline: &PeerBaseline,
) -> Result<probe::PeerObservation, probe::ProbeErrorCategory> {
    let target = production::CredentialTarget::new(target_uid, target_gid)
        .map_err(|_| probe::ProbeErrorCategory::InvalidInput)?;
    crate::macos::credential::observe_later_peer(
        stream,
        datagram,
        expected_pid,
        socket_creator_pid,
        target,
        &baseline.0,
    )
    .map_err(map_category)
    .and_then(map_observation)
}

fn target(
    mode: probe::ProbeMode,
    uid: u32,
    gid: u32,
) -> Result<production::CredentialTarget, probe::ProbeErrorCategory> {
    if (!mode.is_credential_pair() && mode != probe::ProbeMode::CredentialControl)
        || !mode.accepts_target(uid, gid)
    {
        return Err(probe::ProbeErrorCategory::InvalidInput);
    }
    production::CredentialTarget::new(uid, gid).map_err(|_| probe::ProbeErrorCategory::InvalidInput)
}

const fn invalid_failure(category: probe::ProbeErrorCategory) -> probe::CredentialFailureValue {
    probe::CredentialFailureValue::new(
        probe::CredentialStep::InitialIdentity,
        category,
        probe::CredentialPrefix::None,
        probe::CredentialSelfState::new(
            probe::CredentialIdentityClass::Other,
            probe::CredentialGroupClass::Other,
        ),
    )
}

const fn map_transition(
    value: crate::macos::credential::CredentialTransition,
) -> CredentialTransition {
    CredentialTransition {
        state: map_state(value.state()),
        prefix: map_prefix(value.prefix()),
    }
}

const fn map_failure(value: production::CredentialFailureValue) -> probe::CredentialFailureValue {
    probe::CredentialFailureValue::new(
        map_step(value.step()),
        map_category(value.category()),
        map_prefix(value.prefix()),
        map_state(value.state()),
    )
}

fn map_observation(
    value: production::PeerObservation,
) -> Result<probe::PeerObservation, probe::ProbeErrorCategory> {
    if value.is_none() {
        return Ok(probe::PeerObservation::NONE);
    }
    probe::PeerObservation::new(
        map_identity(value.stream_eid()),
        map_identity(value.stream_cred()),
        map_pid(value.stream_pid()),
        map_identity(value.datagram_cred()),
        map_pid(value.datagram_pid()),
        map_token(value.datagram_token()),
    )
    .map_err(|_| probe::ProbeErrorCategory::InvalidInput)
}

const fn map_category(value: production::CredentialErrorCategory) -> probe::ProbeErrorCategory {
    match value {
        production::CredentialErrorCategory::PermissionDenied => {
            probe::ProbeErrorCategory::PermissionDenied
        }
        production::CredentialErrorCategory::InvalidInput => {
            probe::ProbeErrorCategory::InvalidInput
        }
        production::CredentialErrorCategory::Other => probe::ProbeErrorCategory::Other,
    }
}

const fn map_step(value: production::CredentialStep) -> probe::CredentialStep {
    match value {
        production::CredentialStep::InitialIdentity => probe::CredentialStep::InitialIdentity,
        production::CredentialStep::ClearGroups => probe::CredentialStep::ClearGroups,
        production::CredentialStep::ValidateClearedGroups => {
            probe::CredentialStep::ValidateClearedGroups
        }
        production::CredentialStep::SetGid => probe::CredentialStep::SetGid,
        production::CredentialStep::SetUid => probe::CredentialStep::SetUid,
        production::CredentialStep::ValidateFinalIdentity => {
            probe::CredentialStep::ValidateFinalIdentity
        }
        production::CredentialStep::RestoreUid => probe::CredentialStep::RestoreUid,
        production::CredentialStep::RestoreGid => probe::CredentialStep::RestoreGid,
        production::CredentialStep::RestoreGroups => probe::CredentialStep::RestoreGroups,
        production::CredentialStep::PeerObservation => probe::CredentialStep::PeerObservation,
        production::CredentialStep::Protocol => probe::CredentialStep::Protocol,
    }
}

const fn map_prefix(value: production::CredentialPrefix) -> probe::CredentialPrefix {
    match value {
        production::CredentialPrefix::None => probe::CredentialPrefix::None,
        production::CredentialPrefix::Initial => probe::CredentialPrefix::Initial,
        production::CredentialPrefix::GroupsCleared => probe::CredentialPrefix::GroupsCleared,
        production::CredentialPrefix::GidSet => probe::CredentialPrefix::GidSet,
        production::CredentialPrefix::UidSet => probe::CredentialPrefix::UidSet,
        production::CredentialPrefix::FinalIdentity => probe::CredentialPrefix::FinalIdentity,
        production::CredentialPrefix::Irreversible => probe::CredentialPrefix::Irreversible,
        production::CredentialPrefix::RetainedRoot => probe::CredentialPrefix::RetainedRoot,
    }
}

const fn map_identity(
    value: production::CredentialIdentityClass,
) -> probe::CredentialIdentityClass {
    match value {
        production::CredentialIdentityClass::NotObserved => {
            probe::CredentialIdentityClass::NotObserved
        }
        production::CredentialIdentityClass::InitialRoot => {
            probe::CredentialIdentityClass::InitialRoot
        }
        production::CredentialIdentityClass::Target => probe::CredentialIdentityClass::Target,
        production::CredentialIdentityClass::InitialAndTarget => {
            probe::CredentialIdentityClass::InitialAndTarget
        }
        production::CredentialIdentityClass::Other => probe::CredentialIdentityClass::Other,
        production::CredentialIdentityClass::Unsupported => {
            probe::CredentialIdentityClass::Unsupported
        }
    }
}

const fn map_groups(value: production::CredentialGroupClass) -> probe::CredentialGroupClass {
    match value {
        production::CredentialGroupClass::NotObserved => probe::CredentialGroupClass::NotObserved,
        production::CredentialGroupClass::Initial => probe::CredentialGroupClass::Initial,
        production::CredentialGroupClass::EffectiveOnly => {
            probe::CredentialGroupClass::EffectiveOnly
        }
        production::CredentialGroupClass::Other => probe::CredentialGroupClass::Other,
    }
}

const fn map_state(value: production::CredentialSelfState) -> probe::CredentialSelfState {
    probe::CredentialSelfState::new(map_identity(value.identity()), map_groups(value.groups()))
}

const fn map_pid(value: production::PeerPidClass) -> probe::PeerPidClass {
    match value {
        production::PeerPidClass::NotObserved => probe::PeerPidClass::NotObserved,
        production::PeerPidClass::Exact => probe::PeerPidClass::Exact,
        production::PeerPidClass::SocketCreator => probe::PeerPidClass::SocketCreator,
        production::PeerPidClass::Mismatch => probe::PeerPidClass::Mismatch,
        production::PeerPidClass::Unsupported => probe::PeerPidClass::Unsupported,
    }
}

const fn map_token(value: production::PeerTokenClass) -> probe::PeerTokenClass {
    match value {
        production::PeerTokenClass::NotObserved => probe::PeerTokenClass::NotObserved,
        production::PeerTokenClass::Baseline => probe::PeerTokenClass::Baseline,
        production::PeerTokenClass::Unchanged => probe::PeerTokenClass::Unchanged,
        production::PeerTokenClass::Changed => probe::PeerTokenClass::Changed,
        production::PeerTokenClass::Unsupported => probe::PeerTokenClass::Unsupported,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_adapter_accepts_only_exact_credential_modes() {
        assert_eq!(
            target(probe::ProbeMode::CredentialDrop, 501, 20)
                .expect("mapped target")
                .mode(),
            production::CredentialMode::Transition
        );
        assert_eq!(
            target(probe::ProbeMode::CredentialRetainRoot, 0, 0)
                .expect("retained root")
                .mode(),
            production::CredentialMode::RetainedRoot
        );
        assert!(target(probe::ProbeMode::Drop, 501, 20).is_err());
        assert!(target(probe::ProbeMode::CredentialDrop, 0, 0).is_err());
        assert!(target(probe::ProbeMode::CredentialControl, 0, 1).is_err());
    }

    #[test]
    fn adapter_containers_remain_redacted() {
        let transition = CredentialTransition {
            state: probe::CredentialSelfState::new(
                probe::CredentialIdentityClass::Target,
                probe::CredentialGroupClass::EffectiveOnly,
            ),
            prefix: probe::CredentialPrefix::Irreversible,
        };
        assert_eq!(
            format!("{transition:?}"),
            "CredentialTransition(<redacted>)"
        );
    }
}
