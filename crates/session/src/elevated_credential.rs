//! Darwin credential-transition and peer-observation primitives for the test-only probe.

use std::io;
use std::mem::MaybeUninit;
use std::os::fd::RawFd;

use crate::elevated_probe::{
    CredentialFailureValue, CredentialGroupClass, CredentialIdentityClass, CredentialPrefix,
    CredentialSelfState, CredentialStep, PeerObservation, PeerPidClass, PeerTokenClass,
    ProbeErrorCategory, ProbeMode,
};

const MAX_SUPPLEMENTARY_GROUPS: usize = 1_024;
const XUCRED_VERSION: libc::c_uint = 0;
const AUDIT_TOKEN_WORDS: usize = 8;

/// Complete value-free postcondition of one process credential transition.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CredentialTransition {
    state: CredentialSelfState,
    prefix: CredentialPrefix,
}

impl CredentialTransition {
    /// Returns the exact final self-state class.
    #[must_use]
    pub const fn state(self) -> CredentialSelfState {
        self.state
    }

    /// Returns the complete ordered prefix.
    #[must_use]
    pub const fn prefix(self) -> CredentialPrefix {
        self.prefix
    }
}

impl std::fmt::Debug for CredentialTransition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CredentialTransition(<redacted>)")
    }
}

/// Opaque initial peer state retained only for later semantic comparison.
pub struct PeerBaseline {
    datagram_token: TokenBaseline,
}

impl std::fmt::Debug for PeerBaseline {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PeerBaseline(<redacted>)")
    }
}

enum TokenBaseline {
    Unsupported,
    Value([u32; AUDIT_TOKEN_WORDS]),
}

/// Performs the exact no-chroot credential transition for one evidence endpoint.
pub fn transition_process(
    mode: ProbeMode,
    target_uid: u32,
    target_gid: u32,
) -> Result<CredentialTransition, CredentialFailureValue> {
    transition_with(&mut DarwinCredentialOps, mode, target_uid, target_gid)
}

/// Re-attests the exact local post-transition identity before runtime work.
pub fn attest_current_process(
    mode: ProbeMode,
    target_uid: u32,
    target_gid: u32,
) -> Result<CredentialSelfState, ProbeErrorCategory> {
    if !mode.continues_runtime() || !mode.accepts_target(target_uid, target_gid) {
        return Err(ProbeErrorCategory::InvalidInput);
    }
    let mut ops = DarwinCredentialOps;
    let identities = ops.ids();
    let groups = ops.groups().map_err(ProbeErrorCategory::from_io_kind)?;
    if mode.retains_root() {
        if identities != (0, 0, 0, 0) {
            return Err(ProbeErrorCategory::PermissionDenied);
        }
        return Ok(CredentialSelfState::new(
            CredentialIdentityClass::InitialAndTarget,
            CredentialGroupClass::Initial,
        ));
    }
    if identities != (target_uid, target_uid, target_gid, target_gid)
        || groups.as_slice() != [target_gid]
    {
        return Err(ProbeErrorCategory::PermissionDenied);
    }
    Ok(CredentialSelfState::new(
        CredentialIdentityClass::Target,
        CredentialGroupClass::EffectiveOnly,
    ))
}

/// Captures the initial connected stream/datagram peer observation.
pub fn observe_initial_peer(
    stream: RawFd,
    datagram: RawFd,
    expected_pid: libc::pid_t,
    socket_creator_pid: libc::pid_t,
    target_uid: u32,
    target_gid: u32,
) -> Result<(PeerObservation, PeerBaseline), ProbeErrorCategory> {
    validate_socket_type(stream, libc::SOCK_STREAM)?;
    validate_socket_type(datagram, libc::SOCK_DGRAM)?;
    let stream_eid = stream_eid(stream, target_uid, target_gid)?;
    let stream_cred = local_peercred(stream, false, target_uid, target_gid)?;
    let stream_pid = exact_peer_pid(stream, expected_pid, None)?;
    let datagram_cred = local_peercred(datagram, true, target_uid, target_gid)?;
    let datagram_pid = exact_peer_pid(datagram, expected_pid, Some(socket_creator_pid))?;
    let (datagram_token, baseline) = initial_token(datagram)?;
    let observation = PeerObservation::new(
        stream_eid,
        stream_cred,
        stream_pid,
        datagram_cred,
        datagram_pid,
        datagram_token,
    )
    .map_err(|_| ProbeErrorCategory::InvalidInput)?;
    Ok((
        observation,
        PeerBaseline {
            datagram_token: baseline,
        },
    ))
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
) -> Result<PeerObservation, ProbeErrorCategory> {
    validate_socket_type(stream, libc::SOCK_STREAM)?;
    validate_socket_type(datagram, libc::SOCK_DGRAM)?;
    PeerObservation::new(
        stream_eid(stream, target_uid, target_gid)?,
        local_peercred(stream, false, target_uid, target_gid)?,
        exact_peer_pid(stream, expected_pid, None)?,
        local_peercred(datagram, true, target_uid, target_gid)?,
        exact_peer_pid(datagram, expected_pid, Some(socket_creator_pid))?,
        later_token(datagram, &baseline.datagram_token)?,
    )
    .map_err(|_| ProbeErrorCategory::InvalidInput)
}

trait CredentialOps {
    fn ids(&mut self) -> (u32, u32, u32, u32);
    fn groups(&mut self) -> Result<Vec<u32>, io::ErrorKind>;
    fn clear_groups(&mut self) -> Result<(), io::ErrorKind>;
    fn set_gid(&mut self, gid: u32) -> Result<(), io::ErrorKind>;
    fn set_uid(&mut self, uid: u32) -> Result<(), io::ErrorKind>;
    fn restore_groups(&mut self) -> Result<(), io::ErrorKind>;
}

struct DarwinCredentialOps;

impl CredentialOps for DarwinCredentialOps {
    fn ids(&mut self) -> (u32, u32, u32, u32) {
        // SAFETY: Credential getters have no pointer or ownership contract.
        unsafe {
            (
                libc::getuid(),
                libc::geteuid(),
                libc::getgid(),
                libc::getegid(),
            )
        }
    }

    fn groups(&mut self) -> Result<Vec<u32>, io::ErrorKind> {
        // SAFETY: A zero-length query does not dereference the null pointer.
        let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
        let count = usize::try_from(count).map_err(|_| io::Error::last_os_error().kind())?;
        if count > MAX_SUPPLEMENTARY_GROUPS {
            return Err(io::ErrorKind::InvalidData);
        }
        let mut groups = vec![0; count];
        let pointer = if groups.is_empty() {
            std::ptr::null_mut()
        } else {
            groups.as_mut_ptr()
        };
        let capacity =
            libc::c_int::try_from(groups.len()).map_err(|_| io::ErrorKind::InvalidData)?;
        // SAFETY: `pointer` is null for zero capacity or points to `capacity`
        // writable gid slots for the duration of the synchronous call.
        let actual = unsafe { libc::getgroups(capacity, pointer) };
        let actual = usize::try_from(actual).map_err(|_| io::Error::last_os_error().kind())?;
        if actual != groups.len() {
            return Err(io::ErrorKind::InvalidData);
        }
        Ok(groups)
    }

    fn clear_groups(&mut self) -> Result<(), io::ErrorKind> {
        // SAFETY: A zero-length group update does not dereference the null pointer.
        cvt(unsafe { libc::setgroups(0, std::ptr::null()) })
    }

    fn set_gid(&mut self, gid: u32) -> Result<(), io::ErrorKind> {
        // SAFETY: The numeric gid is passed by value and no state is borrowed.
        cvt(unsafe { libc::setgid(gid) })
    }

    fn set_uid(&mut self, uid: u32) -> Result<(), io::ErrorKind> {
        // SAFETY: The numeric uid is passed by value and no state is borrowed.
        cvt(unsafe { libc::setuid(uid) })
    }

    fn restore_groups(&mut self) -> Result<(), io::ErrorKind> {
        let root: libc::gid_t = 0;
        // SAFETY: `root` is one live gid value for the synchronous call.
        cvt(unsafe { libc::setgroups(1, &raw const root) })
    }
}

fn transition_with<O: CredentialOps>(
    ops: &mut O,
    mode: ProbeMode,
    target_uid: u32,
    target_gid: u32,
) -> Result<CredentialTransition, CredentialFailureValue> {
    let retains_root = if mode == ProbeMode::CredentialControl {
        target_uid == 0 && target_gid == 0
    } else if mode.is_credential_pair() {
        mode.retains_root()
    } else {
        return Err(CredentialFailureValue::new(
            CredentialStep::InitialIdentity,
            ProbeErrorCategory::InvalidInput,
            CredentialPrefix::None,
            CredentialSelfState::new(CredentialIdentityClass::Other, CredentialGroupClass::Other),
        ));
    };
    if !mode.accepts_target(target_uid, target_gid) {
        return Err(CredentialFailureValue::new(
            CredentialStep::InitialIdentity,
            ProbeErrorCategory::InvalidInput,
            CredentialPrefix::None,
            CredentialSelfState::new(CredentialIdentityClass::Other, CredentialGroupClass::Other),
        ));
    }

    let initial_groups = ops.groups().map_err(|kind| {
        failure(
            ops,
            CredentialStep::InitialIdentity,
            kind,
            CredentialPrefix::None,
            target_uid,
            target_gid,
            &[],
        )
    })?;
    if ops.ids() != (0, 0, 0, 0) {
        return Err(failure_category(
            ops,
            CredentialStep::InitialIdentity,
            ProbeErrorCategory::PermissionDenied,
            CredentialPrefix::None,
            target_uid,
            target_gid,
            &initial_groups,
        ));
    }

    if retains_root {
        let final_groups = ops.groups().map_err(|kind| {
            failure(
                ops,
                CredentialStep::ValidateFinalIdentity,
                kind,
                CredentialPrefix::Initial,
                target_uid,
                target_gid,
                &initial_groups,
            )
        })?;
        if ops.ids() != (0, 0, 0, 0) || final_groups != initial_groups {
            return Err(failure_category(
                ops,
                CredentialStep::ValidateFinalIdentity,
                ProbeErrorCategory::InvalidInput,
                CredentialPrefix::Initial,
                target_uid,
                target_gid,
                &initial_groups,
            ));
        }
        return Ok(CredentialTransition {
            state: CredentialSelfState::new(
                CredentialIdentityClass::InitialAndTarget,
                CredentialGroupClass::Initial,
            ),
            prefix: CredentialPrefix::RetainedRoot,
        });
    }

    ops.clear_groups().map_err(|kind| {
        failure(
            ops,
            CredentialStep::ClearGroups,
            kind,
            CredentialPrefix::Initial,
            target_uid,
            target_gid,
            &initial_groups,
        )
    })?;
    let groups = ops.groups().map_err(|kind| {
        failure(
            ops,
            CredentialStep::ValidateClearedGroups,
            kind,
            CredentialPrefix::Initial,
            target_uid,
            target_gid,
            &initial_groups,
        )
    })?;
    if groups.as_slice() != [0] {
        return Err(failure_category(
            ops,
            CredentialStep::ValidateClearedGroups,
            ProbeErrorCategory::InvalidInput,
            CredentialPrefix::Initial,
            target_uid,
            target_gid,
            &initial_groups,
        ));
    }

    ops.set_gid(target_gid).map_err(|kind| {
        failure(
            ops,
            CredentialStep::SetGid,
            kind,
            CredentialPrefix::GroupsCleared,
            target_uid,
            target_gid,
            &initial_groups,
        )
    })?;
    ops.set_uid(target_uid).map_err(|kind| {
        failure(
            ops,
            CredentialStep::SetUid,
            kind,
            CredentialPrefix::GidSet,
            target_uid,
            target_gid,
            &initial_groups,
        )
    })?;

    validate_target(ops, target_uid, target_gid).map_err(|kind| {
        failure(
            ops,
            CredentialStep::ValidateFinalIdentity,
            kind,
            CredentialPrefix::UidSet,
            target_uid,
            target_gid,
            &initial_groups,
        )
    })?;

    expect_permission_denied(ops.set_uid(0)).map_err(|category| {
        failure_category(
            ops,
            CredentialStep::RestoreUid,
            category,
            CredentialPrefix::FinalIdentity,
            target_uid,
            target_gid,
            &initial_groups,
        )
    })?;
    expect_permission_denied(ops.set_gid(0)).map_err(|category| {
        failure_category(
            ops,
            CredentialStep::RestoreGid,
            category,
            CredentialPrefix::FinalIdentity,
            target_uid,
            target_gid,
            &initial_groups,
        )
    })?;
    expect_permission_denied(ops.restore_groups()).map_err(|category| {
        failure_category(
            ops,
            CredentialStep::RestoreGroups,
            category,
            CredentialPrefix::FinalIdentity,
            target_uid,
            target_gid,
            &initial_groups,
        )
    })?;
    validate_target(ops, target_uid, target_gid).map_err(|kind| {
        failure(
            ops,
            CredentialStep::ValidateFinalIdentity,
            kind,
            CredentialPrefix::FinalIdentity,
            target_uid,
            target_gid,
            &initial_groups,
        )
    })?;

    Ok(CredentialTransition {
        state: CredentialSelfState::new(
            CredentialIdentityClass::Target,
            CredentialGroupClass::EffectiveOnly,
        ),
        prefix: CredentialPrefix::Irreversible,
    })
}

fn validate_target<O: CredentialOps>(
    ops: &mut O,
    target_uid: u32,
    target_gid: u32,
) -> Result<(), io::ErrorKind> {
    if ops.ids() != (target_uid, target_uid, target_gid, target_gid) {
        return Err(io::ErrorKind::InvalidData);
    }
    if ops.groups()?.as_slice() != [target_gid] {
        return Err(io::ErrorKind::InvalidData);
    }
    Ok(())
}

fn expect_permission_denied(result: Result<(), io::ErrorKind>) -> Result<(), ProbeErrorCategory> {
    match result {
        Err(io::ErrorKind::PermissionDenied) => Ok(()),
        Ok(()) => Err(ProbeErrorCategory::Other),
        Err(kind) => Err(ProbeErrorCategory::from_io_kind(kind)),
    }
}

fn failure<O: CredentialOps>(
    ops: &mut O,
    step: CredentialStep,
    kind: io::ErrorKind,
    prefix: CredentialPrefix,
    target_uid: u32,
    target_gid: u32,
    initial_groups: &[u32],
) -> CredentialFailureValue {
    failure_category(
        ops,
        step,
        ProbeErrorCategory::from_io_kind(kind),
        prefix,
        target_uid,
        target_gid,
        initial_groups,
    )
}

fn failure_category<O: CredentialOps>(
    ops: &mut O,
    step: CredentialStep,
    category: ProbeErrorCategory,
    prefix: CredentialPrefix,
    target_uid: u32,
    target_gid: u32,
    initial_groups: &[u32],
) -> CredentialFailureValue {
    CredentialFailureValue::new(
        step,
        category,
        prefix,
        classify_self(ops, target_uid, target_gid, initial_groups),
    )
}

fn classify_self<O: CredentialOps>(
    ops: &mut O,
    target_uid: u32,
    target_gid: u32,
    initial_groups: &[u32],
) -> CredentialSelfState {
    let (uid, euid, gid, egid) = ops.ids();
    let identity = if (uid, euid, gid, egid) == (0, 0, 0, 0) {
        if target_uid == 0 && target_gid == 0 {
            CredentialIdentityClass::InitialAndTarget
        } else {
            CredentialIdentityClass::InitialRoot
        }
    } else if (uid, euid, gid, egid) == (target_uid, target_uid, target_gid, target_gid) {
        CredentialIdentityClass::Target
    } else {
        CredentialIdentityClass::Other
    };
    let groups = match ops.groups() {
        Ok(groups) if groups == initial_groups => CredentialGroupClass::Initial,
        Ok(groups) if groups.as_slice() == [gid] => CredentialGroupClass::EffectiveOnly,
        Ok(_) | Err(_) => CredentialGroupClass::Other,
    };
    CredentialSelfState::new(identity, groups)
}

fn cvt(result: libc::c_int) -> Result<(), io::ErrorKind> {
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error().kind())
    }
}

fn validate_socket_type(fd: RawFd, expected: libc::c_int) -> Result<(), ProbeErrorCategory> {
    let mut actual = 0;
    let mut length = libc::socklen_t::try_from(std::mem::size_of::<libc::c_int>())
        .map_err(|_| ProbeErrorCategory::InvalidInput)?;
    // SAFETY: `actual` and `length` are writable for the synchronous option query.
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&raw mut actual).cast(),
            &raw mut length,
        )
    } != 0
    {
        return Err(ProbeErrorCategory::from_io_kind(
            io::Error::last_os_error().kind(),
        ));
    }
    if usize::try_from(length).ok() != Some(std::mem::size_of::<libc::c_int>())
        || actual != expected
    {
        return Err(ProbeErrorCategory::InvalidInput);
    }
    Ok(())
}

fn stream_eid(
    fd: RawFd,
    target_uid: u32,
    target_gid: u32,
) -> Result<CredentialIdentityClass, ProbeErrorCategory> {
    let mut uid = 0;
    let mut gid = 0;
    // SAFETY: Both credential outputs are writable and `fd` remains owned by the caller.
    if unsafe { libc::getpeereid(fd, &raw mut uid, &raw mut gid) } != 0 {
        return Err(ProbeErrorCategory::from_io_kind(
            io::Error::last_os_error().kind(),
        ));
    }
    Ok(classify_peer(uid, gid, target_uid, target_gid))
}

fn local_peercred(
    fd: RawFd,
    allow_unsupported: bool,
    target_uid: u32,
    target_gid: u32,
) -> Result<CredentialIdentityClass, ProbeErrorCategory> {
    let mut credential = MaybeUninit::<libc::xucred>::zeroed();
    let mut length = libc::socklen_t::try_from(std::mem::size_of::<libc::xucred>())
        .map_err(|_| ProbeErrorCategory::InvalidInput)?;
    // SAFETY: The output points to `length` writable bytes and neither pointer is retained.
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERCRED,
            credential.as_mut_ptr().cast(),
            &raw mut length,
        )
    } != 0
    {
        let error = io::Error::last_os_error();
        if allow_unsupported && unsupported_errno(error.raw_os_error()) {
            return Ok(CredentialIdentityClass::Unsupported);
        }
        return Err(ProbeErrorCategory::from_io_kind(error.kind()));
    }
    if usize::try_from(length).ok() != Some(std::mem::size_of::<libc::xucred>()) {
        return Err(ProbeErrorCategory::InvalidInput);
    }
    // SAFETY: Successful `getsockopt` initialized the complete fixed-size structure.
    let credential = unsafe { credential.assume_init() };
    let group_count = usize::try_from(credential.cr_ngroups)
        .ok()
        .filter(|count| *count <= credential.cr_groups.len())
        .ok_or(ProbeErrorCategory::InvalidInput)?;
    if credential.cr_version != XUCRED_VERSION {
        return Err(ProbeErrorCategory::InvalidInput);
    }
    if group_count == 0 {
        return Ok(CredentialIdentityClass::Other);
    }
    Ok(classify_peer(
        credential.cr_uid,
        credential.cr_groups[0],
        target_uid,
        target_gid,
    ))
}

fn exact_peer_pid(
    fd: RawFd,
    expected: libc::pid_t,
    socket_creator: Option<libc::pid_t>,
) -> Result<PeerPidClass, ProbeErrorCategory> {
    classify_peer_pid(crate::macos::peer_pid(fd), expected, socket_creator)
}

fn classify_peer_pid(
    result: io::Result<libc::pid_t>,
    expected: libc::pid_t,
    socket_creator: Option<libc::pid_t>,
) -> Result<PeerPidClass, ProbeErrorCategory> {
    match result {
        Ok(pid) if pid == expected => Ok(PeerPidClass::Exact),
        Ok(pid) if Some(pid) == socket_creator => Ok(PeerPidClass::SocketCreator),
        Ok(_) => Err(ProbeErrorCategory::PermissionDenied),
        Err(error) if socket_creator.is_some() && unsupported_errno(error.raw_os_error()) => {
            Ok(PeerPidClass::Unsupported)
        }
        Err(error) => Err(ProbeErrorCategory::from_io_kind(error.kind())),
    }
}

fn initial_token(fd: RawFd) -> Result<(PeerTokenClass, TokenBaseline), ProbeErrorCategory> {
    match peer_token(fd)? {
        Some(token) => Ok((PeerTokenClass::Baseline, TokenBaseline::Value(token))),
        None => Ok((PeerTokenClass::Unsupported, TokenBaseline::Unsupported)),
    }
}

fn later_token(fd: RawFd, baseline: &TokenBaseline) -> Result<PeerTokenClass, ProbeErrorCategory> {
    match (baseline, peer_token(fd)?) {
        (TokenBaseline::Unsupported, None) => Ok(PeerTokenClass::Unsupported),
        (TokenBaseline::Value(expected), Some(actual)) if expected == &actual => {
            Ok(PeerTokenClass::Unchanged)
        }
        (TokenBaseline::Value(_), Some(_)) => Ok(PeerTokenClass::Changed),
        (TokenBaseline::Unsupported, Some(_)) | (TokenBaseline::Value(_), None) => {
            Err(ProbeErrorCategory::InvalidInput)
        }
    }
}

fn peer_token(fd: RawFd) -> Result<Option<[u32; AUDIT_TOKEN_WORDS]>, ProbeErrorCategory> {
    let mut token = [0_u32; AUDIT_TOKEN_WORDS];
    let mut length = libc::socklen_t::try_from(std::mem::size_of_val(&token))
        .map_err(|_| ProbeErrorCategory::InvalidInput)?;
    // SAFETY: `token` points to `length` writable bytes and no pointer is retained.
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERTOKEN,
            token.as_mut_ptr().cast(),
            &raw mut length,
        )
    } != 0
    {
        let error = io::Error::last_os_error();
        if unsupported_errno(error.raw_os_error()) {
            return Ok(None);
        }
        return Err(ProbeErrorCategory::from_io_kind(error.kind()));
    }
    if usize::try_from(length).ok() != Some(std::mem::size_of_val(&token)) {
        return Err(ProbeErrorCategory::InvalidInput);
    }
    Ok(Some(token))
}

const fn unsupported_errno(errno: Option<libc::c_int>) -> bool {
    matches!(
        errno,
        Some(libc::EINVAL) | Some(libc::ENOPROTOOPT) | Some(libc::EOPNOTSUPP)
    )
}

const fn classify_peer(
    uid: u32,
    gid: u32,
    target_uid: u32,
    target_gid: u32,
) -> CredentialIdentityClass {
    if uid == 0 && gid == 0 {
        if target_uid == 0 && target_gid == 0 {
            CredentialIdentityClass::InitialAndTarget
        } else {
            CredentialIdentityClass::InitialRoot
        }
    } else if uid == target_uid && gid == target_gid {
        CredentialIdentityClass::Target
    } else {
        CredentialIdentityClass::Other
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;
    use std::os::unix::net::{UnixDatagram, UnixStream};

    use super::*;

    #[derive(Clone)]
    struct FakeOps {
        ids: (u32, u32, u32, u32),
        groups: Vec<u32>,
        fail_step: Option<CredentialStep>,
        groups_fail_at: Option<usize>,
        group_calls: usize,
        clear_postcondition_mismatch: bool,
        target_postcondition_mismatch: bool,
        restore_uid_result: Result<(), io::ErrorKind>,
        restore_gid_result: Result<(), io::ErrorKind>,
        restore_groups_result: Result<(), io::ErrorKind>,
        calls: Vec<&'static str>,
    }

    impl FakeOps {
        fn root() -> Self {
            Self {
                ids: (0, 0, 0, 0),
                groups: vec![0, 1],
                fail_step: None,
                groups_fail_at: None,
                group_calls: 0,
                clear_postcondition_mismatch: false,
                target_postcondition_mismatch: false,
                restore_uid_result: Err(io::ErrorKind::PermissionDenied),
                restore_gid_result: Err(io::ErrorKind::PermissionDenied),
                restore_groups_result: Err(io::ErrorKind::PermissionDenied),
                calls: Vec::new(),
            }
        }

        fn fail(&self, step: CredentialStep) -> Result<(), io::ErrorKind> {
            if self.fail_step == Some(step) {
                Err(io::ErrorKind::PermissionDenied)
            } else {
                Ok(())
            }
        }
    }

    impl CredentialOps for FakeOps {
        fn ids(&mut self) -> (u32, u32, u32, u32) {
            self.ids
        }

        fn groups(&mut self) -> Result<Vec<u32>, io::ErrorKind> {
            self.group_calls += 1;
            if self.groups_fail_at == Some(self.group_calls) {
                return Err(io::ErrorKind::InvalidData);
            }
            Ok(self.groups.clone())
        }

        fn clear_groups(&mut self) -> Result<(), io::ErrorKind> {
            self.calls.push("setgroups-empty");
            self.fail(CredentialStep::ClearGroups)?;
            self.groups = if self.clear_postcondition_mismatch {
                vec![self.ids.3, 7]
            } else {
                vec![self.ids.3]
            };
            Ok(())
        }

        fn set_gid(&mut self, gid: u32) -> Result<(), io::ErrorKind> {
            if gid == 0 && self.ids.0 != 0 {
                self.calls.push("setgid-root");
                if self.restore_gid_result.is_ok() {
                    self.ids.2 = 0;
                    self.ids.3 = 0;
                    if self.groups.len() == 1 {
                        self.groups[0] = 0;
                    }
                }
                return self.restore_gid_result;
            }
            self.calls.push("setgid-target");
            self.fail(CredentialStep::SetGid)?;
            self.ids.2 = gid;
            self.ids.3 = gid;
            if self.groups.len() == 1 {
                self.groups[0] = gid;
            }
            Ok(())
        }

        fn set_uid(&mut self, uid: u32) -> Result<(), io::ErrorKind> {
            if uid == 0 {
                self.calls.push("setuid-root");
                if self.restore_uid_result.is_ok() {
                    self.ids.0 = 0;
                    self.ids.1 = 0;
                }
                return self.restore_uid_result;
            }
            self.calls.push("setuid-target");
            self.fail(CredentialStep::SetUid)?;
            self.ids.0 = uid;
            self.ids.1 = if self.target_postcondition_mismatch {
                uid.wrapping_add(1)
            } else {
                uid
            };
            Ok(())
        }

        fn restore_groups(&mut self) -> Result<(), io::ErrorKind> {
            self.calls.push("setgroups-root");
            if self.restore_groups_result.is_ok() {
                self.groups = vec![0];
            }
            self.restore_groups_result
        }
    }

    #[test]
    fn ordered_drop_clears_groups_sets_gid_then_uid_and_proves_irreversibility() {
        let mut ops = FakeOps::root();
        let result = transition_with(&mut ops, ProbeMode::CredentialDrop, 501, 20)
            .expect("complete transition should succeed");
        assert_eq!(result.prefix(), CredentialPrefix::Irreversible);
        assert_eq!(
            result.state(),
            CredentialSelfState::new(
                CredentialIdentityClass::Target,
                CredentialGroupClass::EffectiveOnly,
            )
        );
        assert_eq!(
            ops.calls,
            [
                "setgroups-empty",
                "setgid-target",
                "setuid-target",
                "setuid-root",
                "setgid-root",
                "setgroups-root"
            ]
        );
    }

    #[test]
    fn retained_root_calls_no_mutating_credential_operation() {
        let mut ops = FakeOps::root();
        let result = transition_with(&mut ops, ProbeMode::CredentialRetainRoot, 0, 0)
            .expect("retained root should validate");
        assert_eq!(result.prefix(), CredentialPrefix::RetainedRoot);
        assert!(ops.calls.is_empty());
    }

    #[test]
    fn every_mutating_failure_stops_at_the_exact_prefix() {
        for (step, prefix) in [
            (CredentialStep::ClearGroups, CredentialPrefix::Initial),
            (CredentialStep::SetGid, CredentialPrefix::GroupsCleared),
            (CredentialStep::SetUid, CredentialPrefix::GidSet),
        ] {
            let mut ops = FakeOps::root();
            ops.fail_step = Some(step);
            let failure = transition_with(&mut ops, ProbeMode::CredentialDrop, 501, 20)
                .expect_err("injected failure should stop");
            assert_eq!(failure.step(), step);
            assert_eq!(failure.prefix(), prefix);
        }
    }

    #[test]
    fn every_group_query_boundary_stops_with_its_exact_prefix() {
        for (call, step, prefix) in [
            (1, CredentialStep::InitialIdentity, CredentialPrefix::None),
            (
                2,
                CredentialStep::ValidateClearedGroups,
                CredentialPrefix::Initial,
            ),
            (
                3,
                CredentialStep::ValidateFinalIdentity,
                CredentialPrefix::UidSet,
            ),
            (
                4,
                CredentialStep::ValidateFinalIdentity,
                CredentialPrefix::FinalIdentity,
            ),
        ] {
            let mut ops = FakeOps::root();
            ops.groups_fail_at = Some(call);
            let failure = transition_with(&mut ops, ProbeMode::CredentialDrop, 501, 20)
                .expect_err("injected group query failure should stop");
            assert_eq!(failure.step(), step);
            assert_eq!(failure.prefix(), prefix);
            assert_eq!(failure.category(), ProbeErrorCategory::InvalidInput);
        }
    }

    #[test]
    fn postcondition_mismatches_stop_before_restoration_or_ordinary_work() {
        let mut clear_mismatch = FakeOps::root();
        clear_mismatch.clear_postcondition_mismatch = true;
        let failure = transition_with(&mut clear_mismatch, ProbeMode::CredentialDrop, 501, 20)
            .expect_err("extra supplementary groups must fail");
        assert_eq!(failure.step(), CredentialStep::ValidateClearedGroups);
        assert_eq!(failure.prefix(), CredentialPrefix::Initial);
        assert_eq!(clear_mismatch.calls, ["setgroups-empty"]);

        let mut target_mismatch = FakeOps::root();
        target_mismatch.target_postcondition_mismatch = true;
        let failure = transition_with(&mut target_mismatch, ProbeMode::CredentialDrop, 501, 20)
            .expect_err("mixed target identity must fail");
        assert_eq!(failure.step(), CredentialStep::ValidateFinalIdentity);
        assert_eq!(failure.prefix(), CredentialPrefix::UidSet);
        assert_eq!(
            target_mismatch.calls,
            ["setgroups-empty", "setgid-target", "setuid-target"]
        );
    }

    #[test]
    fn every_root_restoration_outcome_other_than_permission_denied_fails_closed() {
        let mut uid_restored = FakeOps::root();
        uid_restored.restore_uid_result = Ok(());
        let failure = transition_with(&mut uid_restored, ProbeMode::CredentialDrop, 501, 20)
            .expect_err("restored uid zero must fail closed");
        assert_eq!(failure.step(), CredentialStep::RestoreUid);
        assert_eq!(failure.category(), ProbeErrorCategory::Other);
        assert_eq!(failure.prefix(), CredentialPrefix::FinalIdentity);

        let mut uid_other_error = FakeOps::root();
        uid_other_error.restore_uid_result = Err(io::ErrorKind::InvalidData);
        let failure = transition_with(&mut uid_other_error, ProbeMode::CredentialDrop, 501, 20)
            .expect_err("non-permission uid failure remains distinct");
        assert_eq!(failure.step(), CredentialStep::RestoreUid);
        assert_eq!(failure.category(), ProbeErrorCategory::InvalidInput);

        let mut gid_restored = FakeOps::root();
        gid_restored.restore_gid_result = Ok(());
        let failure = transition_with(&mut gid_restored, ProbeMode::CredentialDrop, 501, 20)
            .expect_err("restored gid zero must fail closed");
        assert_eq!(failure.step(), CredentialStep::RestoreGid);
        assert_eq!(failure.category(), ProbeErrorCategory::Other);

        let mut groups_restored = FakeOps::root();
        groups_restored.restore_groups_result = Ok(());
        let failure = transition_with(&mut groups_restored, ProbeMode::CredentialDrop, 501, 20)
            .expect_err("restored root access group must fail closed");
        assert_eq!(failure.step(), CredentialStep::RestoreGroups);
        assert_eq!(failure.category(), ProbeErrorCategory::Other);
    }

    #[test]
    fn initial_identity_and_retained_root_postconditions_are_exact() {
        let mut nonroot = FakeOps::root();
        nonroot.ids = (501, 501, 20, 20);
        let failure = transition_with(&mut nonroot, ProbeMode::CredentialDrop, 501, 20)
            .expect_err("nonroot initial identity must fail");
        assert_eq!(failure.step(), CredentialStep::InitialIdentity);
        assert_eq!(failure.category(), ProbeErrorCategory::PermissionDenied);
        assert!(nonroot.calls.is_empty());

        let mut retained_query_failure = FakeOps::root();
        retained_query_failure.groups_fail_at = Some(2);
        let failure = transition_with(
            &mut retained_query_failure,
            ProbeMode::CredentialRetainRoot,
            0,
            0,
        )
        .expect_err("retained-root final group query must be exact");
        assert_eq!(failure.step(), CredentialStep::ValidateFinalIdentity);
        assert_eq!(failure.prefix(), CredentialPrefix::Initial);
        assert!(retained_query_failure.calls.is_empty());
    }

    #[test]
    fn peer_classification_never_exposes_numeric_values() {
        assert_eq!(
            classify_peer(0, 0, 501, 20),
            CredentialIdentityClass::InitialRoot
        );
        assert_eq!(
            classify_peer(501, 20, 501, 20),
            CredentialIdentityClass::Target
        );
        assert_eq!(
            format!(
                "{:?}",
                CredentialTransition {
                    state: CredentialSelfState::new(
                        CredentialIdentityClass::Target,
                        CredentialGroupClass::EffectiveOnly
                    ),
                    prefix: CredentialPrefix::Irreversible,
                }
            ),
            "CredentialTransition(<redacted>)"
        );
    }

    #[test]
    fn connected_stream_and_datagram_observations_keep_identity_surfaces_separate() {
        let (stream, _stream_peer) = UnixStream::pair().expect("stream pair");
        let (datagram, datagram_peer) = UnixDatagram::pair().expect("datagram pair");
        datagram.send(b"p").expect("datagram possession send");
        let mut possession = [0_u8; 1];
        datagram_peer
            .recv(&mut possession)
            .expect("datagram possession receive");
        // SAFETY: Credential/PID getters have no pointer or ownership contract.
        let (pid, uid, gid) = unsafe { (libc::getpid(), libc::geteuid(), libc::getegid()) };
        let (initial, baseline) =
            observe_initial_peer(stream.as_raw_fd(), datagram.as_raw_fd(), pid, pid, uid, gid)
                .expect("current-process peer observations should succeed");
        assert_eq!(initial.stream_eid(), CredentialIdentityClass::Target);
        assert_eq!(initial.stream_cred(), CredentialIdentityClass::Target);
        assert_eq!(initial.stream_pid(), PeerPidClass::Exact);
        assert!(matches!(
            initial.datagram_cred(),
            CredentialIdentityClass::Target | CredentialIdentityClass::Unsupported
        ));
        assert_eq!(initial.datagram_pid(), PeerPidClass::Exact);
        assert!(matches!(
            initial.datagram_token(),
            PeerTokenClass::Baseline | PeerTokenClass::Unsupported
        ));
        let later = observe_later_peer(
            stream.as_raw_fd(),
            datagram.as_raw_fd(),
            pid,
            pid,
            uid,
            gid,
            &baseline,
        )
        .expect("later current-process observation should succeed");
        assert_eq!(later.stream_pid(), PeerPidClass::Exact);
        assert_eq!(later.datagram_pid(), PeerPidClass::Exact);
        assert!(matches!(
            later.datagram_token(),
            PeerTokenClass::Unchanged | PeerTokenClass::Unsupported
        ));
        assert_eq!(format!("{baseline:?}"), "PeerBaseline(<redacted>)");

        assert_eq!(
            exact_peer_pid(datagram.as_raw_fd(), pid.wrapping_add(1), Some(pid)),
            Ok(PeerPidClass::SocketCreator)
        );
        assert!(exact_peer_pid(datagram.as_raw_fd(), pid.wrapping_add(1), None).is_err());
        assert_eq!(
            classify_peer_pid(
                Err(io::Error::from_raw_os_error(libc::ENOPROTOOPT)),
                pid,
                Some(pid),
            ),
            Ok(PeerPidClass::Unsupported)
        );
        assert!(
            classify_peer_pid(
                Err(io::Error::from_raw_os_error(libc::ENOPROTOOPT)),
                pid,
                None,
            )
            .is_err(),
            "stream LOCAL_PEERPID remains a required surface"
        );
        assert!(stream_eid(datagram.as_raw_fd(), uid, gid).is_err());
        assert!(
            matches!(
                observe_initial_peer(datagram.as_raw_fd(), stream.as_raw_fd(), pid, pid, uid, gid,),
                Err(ProbeErrorCategory::InvalidInput)
            ),
            "stream and datagram descriptor roles must not be interchangeable"
        );
    }
}
