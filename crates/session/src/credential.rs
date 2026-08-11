//! Stable production credential targets, outcomes, and private records.

use std::fmt;

use crate::SessionId;

const VERSION: u16 = 1;
const BOOTSTRAP_MAGIC: [u8; 4] = *b"BCB1";
const ATTESTATION_MAGIC: [u8; 4] = *b"BCA1";
const TRANSPORT_COUNT: u8 = 2;

/// Encoded size of one production credential bootstrap record.
pub const CREDENTIAL_BOOTSTRAP_BYTES: usize = 64;
/// Encoded size of one production credential attestation record.
pub const CREDENTIAL_ATTESTATION_BYTES: usize = 64;

/// Bounded failure returned for malformed production credential values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CredentialProtocolError;

impl fmt::Display for CredentialProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid credential protocol value")
    }
}

impl std::error::Error for CredentialProtocolError {}

/// Exact production credential behavior selected for both signed endpoints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CredentialMode {
    /// Retain exact real, effective, and saved root credentials without mutation.
    RetainedRoot = 1,
    /// Permanently transition from root to one nonzero numeric identity.
    Transition = 2,
}

impl CredentialMode {
    pub(crate) const fn from_byte(value: u8) -> Result<Self, CredentialProtocolError> {
        match value {
            1 => Ok(Self::RetainedRoot),
            2 => Ok(Self::Transition),
            _ => Err(CredentialProtocolError),
        }
    }

    /// Returns the stable value-free mode spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::RetainedRoot => "retained-root",
            Self::Transition => "transition",
        }
    }
}

/// Validated numeric identity shared by the launcher and worker.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CredentialTarget {
    uid: u32,
    gid: u32,
    mode: CredentialMode,
}

impl CredentialTarget {
    /// Validates one exact production credential target.
    pub const fn new(uid: u32, gid: u32) -> Result<Self, CredentialProtocolError> {
        let mode = match (uid == 0, gid == 0) {
            (true, true) => CredentialMode::RetainedRoot,
            (false, false) => CredentialMode::Transition,
            (true, false) | (false, true) => return Err(CredentialProtocolError),
        };
        Ok(Self { uid, gid, mode })
    }

    /// Returns the selected credential behavior.
    #[must_use]
    pub const fn mode(self) -> CredentialMode {
        self.mode
    }

    /// Returns the exact target user ID for policy construction.
    #[must_use]
    pub const fn uid(self) -> u32 {
        self.uid
    }

    /// Returns the exact target group ID for policy construction.
    #[must_use]
    pub const fn gid(self) -> u32 {
        self.gid
    }
}

impl fmt::Debug for CredentialTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CredentialTarget(<redacted>)")
    }
}

/// Fixed process role in a production credential exchange.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CredentialRole {
    /// The unsandboxed outer launcher endpoint.
    Launcher = 1,
    /// The mandatory App Sandbox and Hypervisor worker endpoint.
    Worker = 2,
}

impl CredentialRole {
    pub(crate) const fn from_byte(value: u8) -> Result<Self, CredentialProtocolError> {
        match value {
            1 => Ok(Self::Launcher),
            2 => Ok(Self::Worker),
            _ => Err(CredentialProtocolError),
        }
    }

    /// Returns the stable value-free role spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Launcher => "launcher",
            Self::Worker => "worker",
        }
    }
}

/// Redacted operating-system failure category.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CredentialErrorCategory {
    /// The kernel rejected the operation for lack of authority.
    PermissionDenied = 1,
    /// The private record or an input was malformed.
    InvalidInput = 2,
    /// Another value-free operating-system failure occurred.
    Other = 3,
}

impl CredentialErrorCategory {
    /// Maps a standard I/O category without retaining raw errno or values.
    #[must_use]
    pub const fn from_io_kind(kind: std::io::ErrorKind) -> Self {
        match kind {
            std::io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            std::io::ErrorKind::InvalidInput | std::io::ErrorKind::InvalidData => {
                Self::InvalidInput
            }
            _ => Self::Other,
        }
    }

    /// Returns the stable diagnostic spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::PermissionDenied => "permission-denied",
            Self::InvalidInput => "invalid-input",
            Self::Other => "other",
        }
    }

    pub(crate) const fn from_byte(value: u8) -> Result<Self, CredentialProtocolError> {
        match value {
            1 => Ok(Self::PermissionDenied),
            2 => Ok(Self::InvalidInput),
            3 => Ok(Self::Other),
            _ => Err(CredentialProtocolError),
        }
    }
}

/// Stable credential operation or validation step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CredentialStep {
    /// Validate initial real/effective root and capture supplementary groups.
    InitialIdentity = 1,
    /// Clear the supplementary group list.
    ClearGroups = 2,
    /// Validate the cleared supplementary group list.
    ValidateClearedGroups = 3,
    /// Set the target real/effective/saved group identity.
    SetGid = 4,
    /// Set the target real/effective/saved user identity.
    SetUid = 5,
    /// Validate final real/effective identity and supplementary groups.
    ValidateFinalIdentity = 6,
    /// Prove user identity zero cannot be restored.
    RestoreUid = 7,
    /// Prove group identity zero cannot be restored.
    RestoreGid = 8,
    /// Prove root supplementary groups cannot be restored.
    RestoreGroups = 9,
    /// Observe connected stream and datagram peer surfaces.
    PeerObservation = 10,
    /// Validate the closed multi-phase credential exchange.
    Protocol = 11,
}

impl CredentialStep {
    pub(crate) const fn from_byte(value: u8) -> Result<Self, CredentialProtocolError> {
        match value {
            1 => Ok(Self::InitialIdentity),
            2 => Ok(Self::ClearGroups),
            3 => Ok(Self::ValidateClearedGroups),
            4 => Ok(Self::SetGid),
            5 => Ok(Self::SetUid),
            6 => Ok(Self::ValidateFinalIdentity),
            7 => Ok(Self::RestoreUid),
            8 => Ok(Self::RestoreGid),
            9 => Ok(Self::RestoreGroups),
            10 => Ok(Self::PeerObservation),
            11 => Ok(Self::Protocol),
            _ => Err(CredentialProtocolError),
        }
    }

    /// Returns the stable value-free step spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::InitialIdentity => "initial-identity",
            Self::ClearGroups => "clear-groups",
            Self::ValidateClearedGroups => "validate-cleared-groups",
            Self::SetGid => "set-gid",
            Self::SetUid => "set-uid",
            Self::ValidateFinalIdentity => "validate-final-identity",
            Self::RestoreUid => "restore-uid",
            Self::RestoreGid => "restore-gid",
            Self::RestoreGroups => "restore-groups",
            Self::PeerObservation => "peer-observation",
            Self::Protocol => "protocol",
        }
    }
}

/// Last complete prefix of the ordered credential transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CredentialPrefix {
    /// No credential transition step completed.
    None = 0,
    /// Initial root identity and groups were validated.
    Initial = 1,
    /// Supplementary groups were cleared and revalidated.
    GroupsCleared = 2,
    /// Target gid was installed.
    GidSet = 3,
    /// Target uid was installed.
    UidSet = 4,
    /// Final target identity and effective-group-only access list were validated.
    FinalIdentity = 5,
    /// Root restoration attempts all failed and final state remained exact.
    Irreversible = 6,
    /// Retained-root state remained identical without credential mutation.
    RetainedRoot = 7,
}

impl CredentialPrefix {
    pub(crate) const fn from_byte(value: u8) -> Result<Self, CredentialProtocolError> {
        match value {
            0 => Ok(Self::None),
            1 => Ok(Self::Initial),
            2 => Ok(Self::GroupsCleared),
            3 => Ok(Self::GidSet),
            4 => Ok(Self::UidSet),
            5 => Ok(Self::FinalIdentity),
            6 => Ok(Self::Irreversible),
            7 => Ok(Self::RetainedRoot),
            _ => Err(CredentialProtocolError),
        }
    }

    /// Returns the stable value-free prefix spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Initial => "initial",
            Self::GroupsCleared => "groups-cleared",
            Self::GidSet => "gid-set",
            Self::UidSet => "uid-set",
            Self::FinalIdentity => "final-identity",
            Self::Irreversible => "irreversible",
            Self::RetainedRoot => "retained-root",
        }
    }
}

/// Value-free classification of one process or peer identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CredentialIdentityClass {
    /// No identity was observed in this record slot.
    NotObserved = 0,
    /// Exact initial root identity.
    InitialRoot = 1,
    /// Exact requested nonzero target identity.
    Target = 2,
    /// Root is both the initial and requested retained identity.
    InitialAndTarget = 3,
    /// A supported query returned another identity class.
    Other = 4,
    /// The public query is unsupported for this socket surface.
    Unsupported = 5,
}

impl CredentialIdentityClass {
    pub(crate) const fn from_byte(value: u8) -> Result<Self, CredentialProtocolError> {
        match value {
            0 => Ok(Self::NotObserved),
            1 => Ok(Self::InitialRoot),
            2 => Ok(Self::Target),
            3 => Ok(Self::InitialAndTarget),
            4 => Ok(Self::Other),
            5 => Ok(Self::Unsupported),
            _ => Err(CredentialProtocolError),
        }
    }

    /// Returns the stable value-free identity spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::NotObserved => "not-observed",
            Self::InitialRoot => "initial-root",
            Self::Target => "target",
            Self::InitialAndTarget => "initial-and-target",
            Self::Other => "other",
            Self::Unsupported => "unsupported",
        }
    }
}

/// Value-free classification of one process supplementary-group state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CredentialGroupClass {
    /// No group state was observed in this record slot.
    NotObserved = 0,
    /// Exact initial supplementary groups were retained.
    Initial = 1,
    /// Darwin reports only the current effective gid after supplementary groups are cleared.
    EffectiveOnly = 2,
    /// Another group state was observed.
    Other = 3,
}

impl CredentialGroupClass {
    pub(crate) const fn from_byte(value: u8) -> Result<Self, CredentialProtocolError> {
        match value {
            0 => Ok(Self::NotObserved),
            1 => Ok(Self::Initial),
            2 => Ok(Self::EffectiveOnly),
            3 => Ok(Self::Other),
            _ => Err(CredentialProtocolError),
        }
    }

    /// Returns the stable value-free supplementary-group spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::NotObserved => "not-observed",
            Self::Initial => "initial",
            Self::EffectiveOnly => "effective-only",
            Self::Other => "other",
        }
    }
}

/// Value-free final self state reported by one credential endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CredentialSelfState {
    identity: CredentialIdentityClass,
    groups: CredentialGroupClass,
}

impl CredentialSelfState {
    /// Constructs one final self-state classification.
    #[must_use]
    pub const fn new(identity: CredentialIdentityClass, groups: CredentialGroupClass) -> Self {
        Self { identity, groups }
    }

    /// Returns the identity class.
    #[must_use]
    pub const fn identity(self) -> CredentialIdentityClass {
        self.identity
    }

    /// Returns the supplementary-group class.
    #[must_use]
    pub const fn groups(self) -> CredentialGroupClass {
        self.groups
    }
}

/// Value-free failure details for a credential operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CredentialFailureValue {
    step: CredentialStep,
    category: CredentialErrorCategory,
    prefix: CredentialPrefix,
    state: CredentialSelfState,
}

impl CredentialFailureValue {
    /// Constructs one exact failure value.
    #[must_use]
    pub const fn new(
        step: CredentialStep,
        category: CredentialErrorCategory,
        prefix: CredentialPrefix,
        state: CredentialSelfState,
    ) -> Self {
        Self {
            step,
            category,
            prefix,
            state,
        }
    }

    /// Returns the failed operation or validation step.
    #[must_use]
    pub const fn step(self) -> CredentialStep {
        self.step
    }

    /// Returns the redacted operating-system category.
    #[must_use]
    pub const fn category(self) -> CredentialErrorCategory {
        self.category
    }

    /// Returns the last complete ordered prefix.
    #[must_use]
    pub const fn prefix(self) -> CredentialPrefix {
        self.prefix
    }

    /// Returns the value-free self state at failure.
    #[must_use]
    pub const fn state(self) -> CredentialSelfState {
        self.state
    }
}

/// Live PID observation for one connected local socket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PeerPidClass {
    /// No PID query was made in this record slot.
    NotObserved = 0,
    /// The public query returned the exact expected live PID.
    Exact = 1,
    /// The public query retained the fixed socketpair creator PID.
    SocketCreator = 2,
    /// The public query returned another positive PID.
    Mismatch = 3,
    /// The public query is unsupported for this socket surface.
    Unsupported = 4,
}

/// Opaque audit-token observation relative to the initial connected peer token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PeerTokenClass {
    /// No token query was made in this record slot.
    NotObserved = 0,
    /// The initial opaque peer token was captured.
    Baseline = 1,
    /// The later opaque peer token is byte-for-byte unchanged.
    Unchanged = 2,
    /// The later opaque peer token changed without decoding its fields.
    Changed = 3,
    /// The public query is unsupported for this socket surface.
    Unsupported = 4,
}

/// Bounded semantic observations for one stream/datagram peer boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerObservation {
    stream_eid: CredentialIdentityClass,
    stream_cred: CredentialIdentityClass,
    stream_pid: PeerPidClass,
    datagram_cred: CredentialIdentityClass,
    datagram_pid: PeerPidClass,
    datagram_token: PeerTokenClass,
}

impl PeerObservation {
    /// The exact empty observation used for a phase that has not run.
    pub const NONE: Self = Self {
        stream_eid: CredentialIdentityClass::NotObserved,
        stream_cred: CredentialIdentityClass::NotObserved,
        stream_pid: PeerPidClass::NotObserved,
        datagram_cred: CredentialIdentityClass::NotObserved,
        datagram_pid: PeerPidClass::NotObserved,
        datagram_token: PeerTokenClass::NotObserved,
    };

    /// Constructs one complete value-free observation.
    pub fn new(
        stream_eid: CredentialIdentityClass,
        stream_cred: CredentialIdentityClass,
        stream_pid: PeerPidClass,
        datagram_cred: CredentialIdentityClass,
        datagram_pid: PeerPidClass,
        datagram_token: PeerTokenClass,
    ) -> Result<Self, CredentialProtocolError> {
        let observation = Self {
            stream_eid,
            stream_cred,
            stream_pid,
            datagram_cred,
            datagram_pid,
            datagram_token,
        };
        if observation.is_none()
            || (!matches!(stream_eid, CredentialIdentityClass::NotObserved)
                && !matches!(stream_cred, CredentialIdentityClass::NotObserved)
                && !matches!(stream_pid, PeerPidClass::NotObserved)
                && !matches!(datagram_cred, CredentialIdentityClass::NotObserved)
                && !matches!(datagram_pid, PeerPidClass::NotObserved)
                && !matches!(datagram_token, PeerTokenClass::NotObserved))
        {
            Ok(observation)
        } else {
            Err(CredentialProtocolError)
        }
    }

    /// Returns whether no observation was recorded.
    #[must_use]
    pub const fn is_none(self) -> bool {
        matches!(self.stream_eid, CredentialIdentityClass::NotObserved)
            && matches!(self.stream_cred, CredentialIdentityClass::NotObserved)
            && matches!(self.stream_pid, PeerPidClass::NotObserved)
            && matches!(self.datagram_cred, CredentialIdentityClass::NotObserved)
            && matches!(self.datagram_pid, PeerPidClass::NotObserved)
            && matches!(self.datagram_token, PeerTokenClass::NotObserved)
    }

    /// Returns the stream `getpeereid` class.
    #[must_use]
    pub const fn stream_eid(self) -> CredentialIdentityClass {
        self.stream_eid
    }

    /// Returns the stream `LOCAL_PEERCRED` class.
    #[must_use]
    pub const fn stream_cred(self) -> CredentialIdentityClass {
        self.stream_cred
    }

    /// Returns the stream live-PID class.
    #[must_use]
    pub const fn stream_pid(self) -> PeerPidClass {
        self.stream_pid
    }

    /// Returns the datagram `LOCAL_PEERCRED` class.
    #[must_use]
    pub const fn datagram_cred(self) -> CredentialIdentityClass {
        self.datagram_cred
    }

    /// Returns the datagram live-PID class.
    #[must_use]
    pub const fn datagram_pid(self) -> PeerPidClass {
        self.datagram_pid
    }

    /// Returns the opaque datagram token class.
    #[must_use]
    pub const fn datagram_token(self) -> PeerTokenClass {
        self.datagram_token
    }
}

/// Strict fixed-size production credential bootstrap.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CredentialBootstrap {
    target: CredentialTarget,
    nonce: SessionId,
}

impl CredentialBootstrap {
    /// Constructs a bootstrap bound to one fresh session nonce.
    pub fn new(
        target: CredentialTarget,
        nonce: SessionId,
    ) -> Result<Self, CredentialProtocolError> {
        if nonce.is_pre_session() {
            return Err(CredentialProtocolError);
        }
        Ok(Self { target, nonce })
    }

    /// Returns the exact validated credential target.
    #[must_use]
    pub const fn target(self) -> CredentialTarget {
        self.target
    }

    /// Returns the nonce binding this private exchange.
    #[must_use]
    pub const fn nonce(self) -> SessionId {
        self.nonce
    }

    /// Encodes this record into its canonical fixed-size form.
    #[must_use]
    pub fn encode(self) -> [u8; CREDENTIAL_BOOTSTRAP_BYTES] {
        let mut bytes = [0_u8; CREDENTIAL_BOOTSTRAP_BYTES];
        bytes[..4].copy_from_slice(&BOOTSTRAP_MAGIC);
        bytes[4..6].copy_from_slice(&VERSION.to_be_bytes());
        bytes[6] = self.target.mode() as u8;
        bytes[7] = TRANSPORT_COUNT;
        bytes[8..12].copy_from_slice(&self.target.uid().to_be_bytes());
        bytes[12..16].copy_from_slice(&self.target.gid().to_be_bytes());
        bytes[16..48].copy_from_slice(self.nonce.as_bytes());
        bytes
    }

    /// Decodes and canonically validates one fixed-size record.
    pub fn decode(
        bytes: &[u8; CREDENTIAL_BOOTSTRAP_BYTES],
    ) -> Result<Self, CredentialProtocolError> {
        if bytes[..4] != BOOTSTRAP_MAGIC
            || bytes[4..6] != VERSION.to_be_bytes()
            || bytes[7] != TRANSPORT_COUNT
            || bytes[48..] != [0; 16]
        {
            return Err(CredentialProtocolError);
        }
        let mode = CredentialMode::from_byte(bytes[6])?;
        let target = CredentialTarget::new(
            u32::from_be_bytes(array(bytes, 8)?),
            u32::from_be_bytes(array(bytes, 12)?),
        )?;
        if target.mode() != mode {
            return Err(CredentialProtocolError);
        }
        let record = Self::new(target, SessionId::from_bytes(array(bytes, 16)?))?;
        if record.encode() != *bytes {
            return Err(CredentialProtocolError);
        }
        Ok(record)
    }
}

impl fmt::Debug for CredentialBootstrap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CredentialBootstrap(<redacted>)")
    }
}

/// Value-free result carried by a production credential attestation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialAttestationValue {
    /// The exact terminal postcondition completed.
    Success {
        /// Exact final self-state classification.
        state: CredentialSelfState,
        /// Complete ordered credential prefix.
        prefix: CredentialPrefix,
    },
    /// The operation stopped at one exact redacted boundary.
    Failure(CredentialFailureValue),
}

/// Strict fixed-size production credential attestation.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CredentialAttestation {
    target: CredentialTarget,
    role: CredentialRole,
    nonce: SessionId,
    value: CredentialAttestationValue,
}

impl CredentialAttestation {
    /// Constructs the only canonical success value for a target.
    pub fn success(
        target: CredentialTarget,
        role: CredentialRole,
        nonce: SessionId,
        state: CredentialSelfState,
        prefix: CredentialPrefix,
    ) -> Result<Self, CredentialProtocolError> {
        let expected = terminal_state(target);
        if nonce.is_pre_session() || (state, prefix) != expected {
            return Err(CredentialProtocolError);
        }
        Ok(Self {
            target,
            role,
            nonce,
            value: CredentialAttestationValue::Success { state, prefix },
        })
    }

    /// Constructs one canonical value-free failure attestation.
    pub fn failure(
        target: CredentialTarget,
        role: CredentialRole,
        nonce: SessionId,
        failure: CredentialFailureValue,
    ) -> Result<Self, CredentialProtocolError> {
        if nonce.is_pre_session() || !valid_failure(target, failure) {
            return Err(CredentialProtocolError);
        }
        Ok(Self {
            target,
            role,
            nonce,
            value: CredentialAttestationValue::Failure(failure),
        })
    }

    /// Returns the exact validated target.
    #[must_use]
    pub const fn target(self) -> CredentialTarget {
        self.target
    }

    /// Returns the process role producing this record.
    #[must_use]
    pub const fn role(self) -> CredentialRole {
        self.role
    }

    /// Returns the nonce binding this record to its bootstrap.
    #[must_use]
    pub const fn nonce(self) -> SessionId {
        self.nonce
    }

    /// Returns the exact value-free attestation outcome.
    #[must_use]
    pub const fn value(self) -> CredentialAttestationValue {
        self.value
    }

    /// Encodes this record into its canonical fixed-size form.
    #[must_use]
    pub fn encode(self) -> [u8; CREDENTIAL_ATTESTATION_BYTES] {
        let mut bytes = [0_u8; CREDENTIAL_ATTESTATION_BYTES];
        bytes[..4].copy_from_slice(&ATTESTATION_MAGIC);
        bytes[4..6].copy_from_slice(&VERSION.to_be_bytes());
        bytes[6] = self.target.mode() as u8;
        bytes[7] = self.role as u8;
        let (outcome, state, prefix, step, category) = match self.value {
            CredentialAttestationValue::Success { state, prefix } => (1, state, prefix, 0, 0),
            CredentialAttestationValue::Failure(failure) => (
                2,
                failure.state(),
                failure.prefix(),
                failure.step() as u8,
                failure.category() as u8,
            ),
        };
        bytes[8] = outcome;
        bytes[9] = prefix as u8;
        bytes[10] = state.identity() as u8;
        bytes[11] = state.groups() as u8;
        bytes[12] = step;
        bytes[13] = category;
        bytes[16..20].copy_from_slice(&self.target.uid().to_be_bytes());
        bytes[20..24].copy_from_slice(&self.target.gid().to_be_bytes());
        bytes[24..56].copy_from_slice(self.nonce.as_bytes());
        bytes
    }

    /// Decodes and canonically validates one fixed-size record.
    pub fn decode(
        bytes: &[u8; CREDENTIAL_ATTESTATION_BYTES],
    ) -> Result<Self, CredentialProtocolError> {
        if bytes[..4] != ATTESTATION_MAGIC
            || bytes[4..6] != VERSION.to_be_bytes()
            || bytes[14..16] != [0; 2]
            || bytes[56..] != [0; 8]
        {
            return Err(CredentialProtocolError);
        }
        let mode = CredentialMode::from_byte(bytes[6])?;
        let role = CredentialRole::from_byte(bytes[7])?;
        let target = CredentialTarget::new(
            u32::from_be_bytes(array(bytes, 16)?),
            u32::from_be_bytes(array(bytes, 20)?),
        )?;
        if target.mode() != mode {
            return Err(CredentialProtocolError);
        }
        let nonce = SessionId::from_bytes(array(bytes, 24)?);
        let state = CredentialSelfState::new(
            CredentialIdentityClass::from_byte(bytes[10])?,
            CredentialGroupClass::from_byte(bytes[11])?,
        );
        let record = match bytes[8] {
            1 if bytes[12] == 0 && bytes[13] == 0 => Self::success(
                target,
                role,
                nonce,
                state,
                CredentialPrefix::from_byte(bytes[9])?,
            )?,
            2 => Self::failure(
                target,
                role,
                nonce,
                CredentialFailureValue::new(
                    CredentialStep::from_byte(bytes[12])?,
                    CredentialErrorCategory::from_byte(bytes[13])?,
                    CredentialPrefix::from_byte(bytes[9])?,
                    state,
                ),
            )?,
            _ => return Err(CredentialProtocolError),
        };
        if record.encode() != *bytes {
            return Err(CredentialProtocolError);
        }
        Ok(record)
    }
}

impl fmt::Debug for CredentialAttestation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CredentialAttestation(<redacted>)")
    }
}

const fn terminal_state(target: CredentialTarget) -> (CredentialSelfState, CredentialPrefix) {
    match target.mode() {
        CredentialMode::RetainedRoot => (
            CredentialSelfState::new(
                CredentialIdentityClass::InitialAndTarget,
                CredentialGroupClass::Initial,
            ),
            CredentialPrefix::RetainedRoot,
        ),
        CredentialMode::Transition => (
            CredentialSelfState::new(
                CredentialIdentityClass::Target,
                CredentialGroupClass::EffectiveOnly,
            ),
            CredentialPrefix::Irreversible,
        ),
    }
}

const fn valid_failure(target: CredentialTarget, failure: CredentialFailureValue) -> bool {
    if matches!(
        failure.state().identity(),
        CredentialIdentityClass::NotObserved | CredentialIdentityClass::Unsupported
    ) || matches!(failure.state().groups(), CredentialGroupClass::NotObserved)
    {
        return false;
    }
    matches!(
        (target.mode(), failure.step(), failure.prefix()),
        (_, CredentialStep::InitialIdentity, CredentialPrefix::None)
            | (
                CredentialMode::RetainedRoot,
                CredentialStep::ValidateFinalIdentity,
                CredentialPrefix::Initial,
            )
            | (
                CredentialMode::RetainedRoot,
                CredentialStep::PeerObservation | CredentialStep::Protocol,
                CredentialPrefix::RetainedRoot,
            )
            | (
                CredentialMode::Transition,
                CredentialStep::ClearGroups | CredentialStep::ValidateClearedGroups,
                CredentialPrefix::Initial,
            )
            | (
                CredentialMode::Transition,
                CredentialStep::SetGid,
                CredentialPrefix::GroupsCleared
            )
            | (
                CredentialMode::Transition,
                CredentialStep::SetUid,
                CredentialPrefix::GidSet
            )
            | (
                CredentialMode::Transition,
                CredentialStep::ValidateFinalIdentity,
                CredentialPrefix::UidSet | CredentialPrefix::FinalIdentity,
            )
            | (
                CredentialMode::Transition,
                CredentialStep::RestoreUid
                    | CredentialStep::RestoreGid
                    | CredentialStep::RestoreGroups,
                CredentialPrefix::FinalIdentity,
            )
            | (
                CredentialMode::Transition,
                CredentialStep::PeerObservation | CredentialStep::Protocol,
                CredentialPrefix::Irreversible,
            )
    )
}

fn array<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u8; N], CredentialProtocolError> {
    bytes
        .get(offset..offset + N)
        .and_then(|slice| slice.try_into().ok())
        .ok_or(CredentialProtocolError)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nonce() -> SessionId {
        SessionId::from_bytes([7; 32])
    }

    #[test]
    fn targets_are_exact_and_redacted() {
        let retained = CredentialTarget::new(0, 0).expect("retained root");
        assert_eq!(retained.mode(), CredentialMode::RetainedRoot);
        let transition = CredentialTarget::new(u32::MAX, u32::MAX).expect("nonzero target");
        assert_eq!(transition.mode(), CredentialMode::Transition);
        assert_eq!(format!("{transition:?}"), "CredentialTarget(<redacted>)");
        assert!(CredentialTarget::new(0, 1).is_err());
        assert!(CredentialTarget::new(1, 0).is_err());
    }

    #[test]
    fn bootstrap_roundtrip_is_strict_and_redacted() {
        let record =
            CredentialBootstrap::new(CredentialTarget::new(501, 20).expect("target"), nonce())
                .expect("bootstrap");
        let encoded = record.encode();
        assert_eq!(CredentialBootstrap::decode(&encoded), Ok(record));
        assert_eq!(format!("{record:?}"), "CredentialBootstrap(<redacted>)");
        assert!(CredentialBootstrap::new(record.target(), SessionId::pre_session()).is_err());

        for index in [0, 4, 6, 7, 48, 63] {
            let mut malformed = encoded;
            malformed[index] ^= 0xff;
            assert!(
                CredentialBootstrap::decode(&malformed).is_err(),
                "index {index}"
            );
        }
        let mut mixed_target = encoded;
        mixed_target[8..12].copy_from_slice(&0_u32.to_be_bytes());
        assert!(CredentialBootstrap::decode(&mixed_target).is_err());
        let mut pre_session = encoded;
        pre_session[16..48].fill(0);
        assert!(CredentialBootstrap::decode(&pre_session).is_err());
    }

    #[test]
    fn success_and_failure_attestations_roundtrip_canonically() {
        for (target, state, prefix) in [
            (
                CredentialTarget::new(0, 0).expect("root"),
                CredentialSelfState::new(
                    CredentialIdentityClass::InitialAndTarget,
                    CredentialGroupClass::Initial,
                ),
                CredentialPrefix::RetainedRoot,
            ),
            (
                CredentialTarget::new(501, 20).expect("target"),
                CredentialSelfState::new(
                    CredentialIdentityClass::Target,
                    CredentialGroupClass::EffectiveOnly,
                ),
                CredentialPrefix::Irreversible,
            ),
        ] {
            let record = CredentialAttestation::success(
                target,
                CredentialRole::Worker,
                nonce(),
                state,
                prefix,
            )
            .expect("success");
            assert_eq!(CredentialAttestation::decode(&record.encode()), Ok(record));
            assert_eq!(format!("{record:?}"), "CredentialAttestation(<redacted>)");
        }

        let target = CredentialTarget::new(501, 20).expect("target");
        let failure = CredentialFailureValue::new(
            CredentialStep::SetUid,
            CredentialErrorCategory::PermissionDenied,
            CredentialPrefix::GidSet,
            CredentialSelfState::new(
                CredentialIdentityClass::InitialRoot,
                CredentialGroupClass::EffectiveOnly,
            ),
        );
        let record =
            CredentialAttestation::failure(target, CredentialRole::Launcher, nonce(), failure)
                .expect("failure");
        assert_eq!(CredentialAttestation::decode(&record.encode()), Ok(record));
    }

    #[test]
    fn attestation_rejects_noncanonical_shapes_and_reserved_bytes() {
        let target = CredentialTarget::new(501, 20).expect("target");
        assert!(
            CredentialAttestation::success(
                target,
                CredentialRole::Launcher,
                nonce(),
                CredentialSelfState::new(
                    CredentialIdentityClass::InitialAndTarget,
                    CredentialGroupClass::Initial,
                ),
                CredentialPrefix::RetainedRoot,
            )
            .is_err()
        );
        let record = CredentialAttestation::success(
            target,
            CredentialRole::Launcher,
            nonce(),
            CredentialSelfState::new(
                CredentialIdentityClass::Target,
                CredentialGroupClass::EffectiveOnly,
            ),
            CredentialPrefix::Irreversible,
        )
        .expect("success");
        for index in [0, 4, 6, 7, 8, 9, 10, 11, 12, 13, 14, 56, 63] {
            let mut malformed = record.encode();
            malformed[index] ^= 0xff;
            assert!(
                CredentialAttestation::decode(&malformed).is_err(),
                "index {index}"
            );
        }
        let mut mixed_target = record.encode();
        mixed_target[16..20].copy_from_slice(&0_u32.to_be_bytes());
        assert!(CredentialAttestation::decode(&mixed_target).is_err());
        let mut pre_session = record.encode();
        pre_session[24..56].fill(0);
        assert!(CredentialAttestation::decode(&pre_session).is_err());

        let contradictory = CredentialFailureValue::new(
            CredentialStep::SetUid,
            CredentialErrorCategory::PermissionDenied,
            CredentialPrefix::Irreversible,
            CredentialSelfState::new(
                CredentialIdentityClass::Target,
                CredentialGroupClass::EffectiveOnly,
            ),
        );
        assert!(
            CredentialAttestation::failure(target, CredentialRole::Worker, nonce(), contradictory,)
                .is_err()
        );
        let valid_failure = CredentialFailureValue::new(
            CredentialStep::PeerObservation,
            CredentialErrorCategory::Other,
            CredentialPrefix::Irreversible,
            CredentialSelfState::new(
                CredentialIdentityClass::Target,
                CredentialGroupClass::EffectiveOnly,
            ),
        );
        assert!(
            CredentialAttestation::failure(
                target,
                CredentialRole::Worker,
                SessionId::pre_session(),
                valid_failure,
            )
            .is_err()
        );
    }

    #[test]
    fn peer_observation_requires_all_or_none() {
        assert!(PeerObservation::NONE.is_none());
        assert!(
            PeerObservation::new(
                CredentialIdentityClass::Target,
                CredentialIdentityClass::Target,
                PeerPidClass::Exact,
                CredentialIdentityClass::Unsupported,
                PeerPidClass::Exact,
                PeerTokenClass::Unsupported,
            )
            .is_ok()
        );
        assert!(
            PeerObservation::new(
                CredentialIdentityClass::NotObserved,
                CredentialIdentityClass::Target,
                PeerPidClass::Exact,
                CredentialIdentityClass::Target,
                PeerPidClass::Exact,
                PeerTokenClass::Baseline,
            )
            .is_err()
        );
    }
}
