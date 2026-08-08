//! Closed wire contract for the test-only elevated bootstrap evidence harness.

use std::fmt;

use crate::{ObjectIdentity, SessionId};

/// Launcher argv activation compiled only into the evidence bundle.
pub const LAUNCHER_ACTIVATION: &str = "--bangbang-internal-elevated-bootstrap-probe-v1";
/// Worker argv activation compiled only into the evidence bundle.
pub const WORKER_ACTIVATION: &str = "--bangbang-internal-elevated-bootstrap-worker-v1";
/// Fixed inherited descriptor carrying the exact private root.
pub const ROOT_FD: libc::c_int = 8;
/// Fixed worker-ready record sent before live code validation completes.
pub const READY_RECORD: [u8; 16] = *b"BBEP-READY-V1\0\0\0";
/// Encoded bootstrap record length.
pub const BOOTSTRAP_RECORD_BYTES: usize = 64;
/// Encoded terminal result record length.
pub const RESULT_RECORD_BYTES: usize = 48;

const VERSION: u16 = 1;
const BOOTSTRAP_MAGIC: [u8; 4] = *b"BBE1";
const RESULT_MAGIC: [u8; 4] = *b"BBR1";

/// Exact evidence mode selected by the explicit root wrapper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ProbeMode {
    /// Chroot and permanently drop to an explicit ordinary numeric identity.
    Drop = 1,
    /// Chroot while retaining exact uid/gid zero for the upstream no-drop shape.
    RetainRoot = 2,
    /// Exercise a high unmapped numeric identity at the syscall boundary.
    UnmappedSyscall = 3,
    /// Run the same chroot primitive in the unsandboxed launcher control process.
    Control = 4,
}

impl ProbeMode {
    /// Parses the closed root-wrapper spelling.
    pub fn parse(value: &str, uid: u32, gid: u32) -> Option<Self> {
        let mode = match value {
            "drop" => Self::Drop,
            "retain-root" => Self::RetainRoot,
            "unmapped-syscall" => Self::UnmappedSyscall,
            "control" => Self::Control,
            _ => return None,
        };
        mode.accepts_target(uid, gid).then_some(mode)
    }

    /// Returns the fixed, value-free status spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Drop => "drop",
            Self::RetainRoot => "retain-root",
            Self::UnmappedSyscall => "unmapped-syscall",
            Self::Control => "control",
        }
    }

    /// Returns whether this mode admits the exact target category.
    #[must_use]
    pub const fn accepts_target(self, uid: u32, gid: u32) -> bool {
        match self {
            Self::Drop | Self::UnmappedSyscall => uid != 0 && gid != 0,
            Self::RetainRoot | Self::Control => uid == 0 && gid == 0,
        }
    }

    fn from_byte(value: u8) -> Result<Self, ProbeProtocolError> {
        match value {
            1 => Ok(Self::Drop),
            2 => Ok(Self::RetainRoot),
            3 => Ok(Self::UnmappedSyscall),
            4 => Ok(Self::Control),
            _ => Err(ProbeProtocolError),
        }
    }
}

/// Stable point reached by the root bootstrap state machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ProbeStage {
    /// Validate exact initial root credentials and the closed bootstrap.
    InitialIdentity = 1,
    /// Consume the fixed inherited private-root descriptor.
    TakeRoot = 2,
    /// Validate root type, ownership, mode, and identity.
    ValidateRoot = 3,
    /// Enter the retained root directory by descriptor.
    EnterRoot = 4,
    /// Call the public Darwin chroot primitive.
    Chroot = 5,
    /// Reset cwd to slash inside the new root.
    ChangeDirectory = 6,
    /// Fail closed if a future platform unexpectedly permits continuation.
    UnexpectedContinuation = 7,
}

impl ProbeStage {
    /// Returns the stable, value-free diagnostic spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::InitialIdentity => "initial-identity",
            Self::TakeRoot => "take-root",
            Self::ValidateRoot => "validate-root",
            Self::EnterRoot => "enter-root",
            Self::Chroot => "chroot",
            Self::ChangeDirectory => "change-directory",
            Self::UnexpectedContinuation => "unexpected-continuation",
        }
    }

    fn from_byte(value: u8) -> Result<Self, ProbeProtocolError> {
        match value {
            1 => Ok(Self::InitialIdentity),
            2 => Ok(Self::TakeRoot),
            3 => Ok(Self::ValidateRoot),
            4 => Ok(Self::EnterRoot),
            5 => Ok(Self::Chroot),
            6 => Ok(Self::ChangeDirectory),
            7 => Ok(Self::UnexpectedContinuation),
            _ => Err(ProbeProtocolError),
        }
    }
}

/// Redacted error category sent by the signed worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ProbeErrorCategory {
    /// The kernel rejected the operation for lack of authority.
    PermissionDenied = 1,
    /// The closed bootstrap or an input was malformed.
    InvalidInput = 2,
    /// Another value-free operating-system failure occurred.
    Other = 3,
}

impl ProbeErrorCategory {
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

    fn from_byte(value: u8) -> Result<Self, ProbeProtocolError> {
        match value {
            1 => Ok(Self::PermissionDenied),
            2 => Ok(Self::InvalidInput),
            3 => Ok(Self::Other),
            _ => Err(ProbeProtocolError),
        }
    }
}

/// One nonce-bound exact-root bootstrap command.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ProbeBootstrap {
    mode: ProbeMode,
    target_uid: u32,
    target_gid: u32,
    root: ObjectIdentity,
    nonce: SessionId,
}

impl ProbeBootstrap {
    /// Constructs a complete worker command; launcher-only control is rejected.
    pub fn new(
        mode: ProbeMode,
        target_uid: u32,
        target_gid: u32,
        root: ObjectIdentity,
        nonce: SessionId,
    ) -> Result<Self, ProbeProtocolError> {
        if mode == ProbeMode::Control
            || !mode.accepts_target(target_uid, target_gid)
            || root.device == 0
            || root.inode == 0
            || nonce.is_pre_session()
        {
            return Err(ProbeProtocolError);
        }
        Ok(Self {
            mode,
            target_uid,
            target_gid,
            root,
            nonce,
        })
    }

    /// Returns the selected worker mode.
    #[must_use]
    pub const fn mode(self) -> ProbeMode {
        self.mode
    }

    /// Returns the exact target user ID.
    #[must_use]
    pub const fn target_uid(self) -> u32 {
        self.target_uid
    }

    /// Returns the exact target group ID.
    #[must_use]
    pub const fn target_gid(self) -> u32 {
        self.target_gid
    }

    /// Returns the independently measured root identity.
    #[must_use]
    pub const fn root(self) -> ObjectIdentity {
        self.root
    }

    /// Returns the random command identity.
    #[must_use]
    pub const fn nonce(self) -> SessionId {
        self.nonce
    }

    /// Encodes the fixed v1 record.
    #[must_use]
    pub fn encode(self) -> [u8; BOOTSTRAP_RECORD_BYTES] {
        let mut bytes = [0_u8; BOOTSTRAP_RECORD_BYTES];
        bytes[0..4].copy_from_slice(&BOOTSTRAP_MAGIC);
        bytes[4..6].copy_from_slice(&VERSION.to_be_bytes());
        bytes[6] = self.mode as u8;
        bytes[8..12].copy_from_slice(&self.target_uid.to_be_bytes());
        bytes[12..16].copy_from_slice(&self.target_gid.to_be_bytes());
        bytes[16..24].copy_from_slice(&self.root.device.to_be_bytes());
        bytes[24..32].copy_from_slice(&self.root.inode.to_be_bytes());
        bytes[32..64].copy_from_slice(self.nonce.as_bytes());
        bytes
    }

    /// Decodes one exact fixed-size record.
    pub fn decode(bytes: &[u8; BOOTSTRAP_RECORD_BYTES]) -> Result<Self, ProbeProtocolError> {
        if bytes[0..4] != BOOTSTRAP_MAGIC || bytes[4..6] != VERSION.to_be_bytes() || bytes[7] != 0 {
            return Err(ProbeProtocolError);
        }
        let mode = ProbeMode::from_byte(bytes[6])?;
        let target_uid = u32::from_be_bytes(array(bytes, 8)?);
        let target_gid = u32::from_be_bytes(array(bytes, 12)?);
        let root = ObjectIdentity {
            device: u64::from_be_bytes(array(bytes, 16)?),
            inode: u64::from_be_bytes(array(bytes, 24)?),
        };
        let nonce = SessionId::from_bytes(array(bytes, 32)?);
        Self::new(mode, target_uid, target_gid, root, nonce)
    }
}

impl fmt::Debug for ProbeBootstrap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProbeBootstrap(<redacted>)")
    }
}

/// Authenticated terminal result for one bootstrap command.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ProbeResult {
    mode: ProbeMode,
    outcome: Result<(), (ProbeStage, ProbeErrorCategory)>,
    nonce: SessionId,
}

impl ProbeResult {
    /// Constructs a successful terminal result.
    pub fn success(mode: ProbeMode, nonce: SessionId) -> Result<Self, ProbeProtocolError> {
        if mode == ProbeMode::Control || nonce.is_pre_session() {
            return Err(ProbeProtocolError);
        }
        Ok(Self {
            mode,
            outcome: Ok(()),
            nonce,
        })
    }

    /// Constructs a value-redacted failure result.
    pub fn failure(
        mode: ProbeMode,
        nonce: SessionId,
        stage: ProbeStage,
        category: ProbeErrorCategory,
    ) -> Result<Self, ProbeProtocolError> {
        if mode == ProbeMode::Control || nonce.is_pre_session() {
            return Err(ProbeProtocolError);
        }
        Ok(Self {
            mode,
            outcome: Err((stage, category)),
            nonce,
        })
    }

    /// Returns the selected mode.
    #[must_use]
    pub const fn mode(self) -> ProbeMode {
        self.mode
    }

    /// Returns success or the stable failure pair.
    pub const fn outcome(self) -> Result<(), (ProbeStage, ProbeErrorCategory)> {
        self.outcome
    }

    /// Returns the command nonce echoed by the worker.
    #[must_use]
    pub const fn nonce(self) -> SessionId {
        self.nonce
    }

    /// Encodes the fixed v1 terminal record.
    #[must_use]
    pub fn encode(self) -> [u8; RESULT_RECORD_BYTES] {
        let mut bytes = [0_u8; RESULT_RECORD_BYTES];
        bytes[0..4].copy_from_slice(&RESULT_MAGIC);
        bytes[4..6].copy_from_slice(&VERSION.to_be_bytes());
        bytes[6] = self.mode as u8;
        match self.outcome {
            Ok(()) => bytes[7] = 1,
            Err((stage, category)) => {
                bytes[8] = stage as u8;
                bytes[9] = category as u8;
            }
        }
        bytes[16..48].copy_from_slice(self.nonce.as_bytes());
        bytes
    }

    /// Decodes one exact fixed-size terminal record.
    pub fn decode(bytes: &[u8; RESULT_RECORD_BYTES]) -> Result<Self, ProbeProtocolError> {
        if bytes[0..4] != RESULT_MAGIC
            || bytes[4..6] != VERSION.to_be_bytes()
            || bytes[10..16] != [0; 6]
        {
            return Err(ProbeProtocolError);
        }
        let mode = ProbeMode::from_byte(bytes[6])?;
        if mode == ProbeMode::Control {
            return Err(ProbeProtocolError);
        }
        let nonce = SessionId::from_bytes(array(bytes, 16)?);
        if nonce.is_pre_session() {
            return Err(ProbeProtocolError);
        }
        let outcome = match (bytes[7], bytes[8], bytes[9]) {
            (1, 0, 0) => Ok(()),
            (0, stage, category) => Err((
                ProbeStage::from_byte(stage)?,
                ProbeErrorCategory::from_byte(category)?,
            )),
            _ => return Err(ProbeProtocolError),
        };
        Ok(Self {
            mode,
            outcome,
            nonce,
        })
    }
}

impl fmt::Debug for ProbeResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProbeResult(<redacted>)")
    }
}

/// Malformed or inconsistent test-only bootstrap protocol data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeProtocolError;

impl fmt::Display for ProbeProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid elevated bootstrap probe record")
    }
}

impl std::error::Error for ProbeProtocolError {}

fn array<const N: usize>(bytes: &[u8], start: usize) -> Result<[u8; N], ProbeProtocolError> {
    bytes
        .get(start..start.checked_add(N).ok_or(ProbeProtocolError)?)
        .and_then(|value| value.try_into().ok())
        .ok_or(ProbeProtocolError)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bootstrap() -> ProbeBootstrap {
        ProbeBootstrap::new(
            ProbeMode::Drop,
            501,
            20,
            ObjectIdentity {
                device: 12,
                inode: 34,
            },
            SessionId::from_bytes([7; 32]),
        )
        .expect("valid probe bootstrap")
    }

    #[test]
    fn bootstrap_round_trip_is_exact_and_redacted() {
        let bootstrap = bootstrap();
        let encoded = bootstrap.encode();
        assert_eq!(encoded.len(), BOOTSTRAP_RECORD_BYTES);
        assert_eq!(ProbeBootstrap::decode(&encoded), Ok(bootstrap));
        assert_eq!(format!("{bootstrap:?}"), "ProbeBootstrap(<redacted>)");
        assert!(!format!("{bootstrap:?}").contains("501"));
    }

    #[test]
    fn bootstrap_rejects_malformed_version_reserved_nonce_and_targets() {
        let encoded = bootstrap().encode();
        for index in [0, 4, 7] {
            let mut malformed = encoded;
            malformed[index] ^= 0xff;
            assert_eq!(ProbeBootstrap::decode(&malformed), Err(ProbeProtocolError));
        }
        let mut zero_nonce = encoded;
        zero_nonce[32..64].fill(0);
        assert_eq!(ProbeBootstrap::decode(&zero_nonce), Err(ProbeProtocolError));
        assert!(
            ProbeBootstrap::new(
                ProbeMode::Control,
                0,
                0,
                ObjectIdentity {
                    device: 1,
                    inode: 1,
                },
                SessionId::from_bytes([1; 32]),
            )
            .is_err()
        );
        assert!(
            ProbeBootstrap::new(
                ProbeMode::Drop,
                0,
                0,
                ObjectIdentity {
                    device: 1,
                    inode: 1,
                },
                SessionId::from_bytes([1; 32]),
            )
            .is_err()
        );
    }

    #[test]
    fn terminal_results_round_trip_and_reject_inconsistent_shapes() {
        let nonce = SessionId::from_bytes([9; 32]);
        for result in [
            ProbeResult::success(ProbeMode::Drop, nonce).expect("valid success"),
            ProbeResult::failure(
                ProbeMode::Drop,
                nonce,
                ProbeStage::Chroot,
                ProbeErrorCategory::PermissionDenied,
            )
            .expect("valid failure"),
        ] {
            let encoded = result.encode();
            assert_eq!(ProbeResult::decode(&encoded), Ok(result));
            assert_eq!(format!("{result:?}"), "ProbeResult(<redacted>)");
        }
        let mut malformed = ProbeResult::success(ProbeMode::Drop, nonce)
            .expect("valid success")
            .encode();
        malformed[8] = ProbeStage::Chroot as u8;
        assert_eq!(ProbeResult::decode(&malformed), Err(ProbeProtocolError));
        assert!(ProbeResult::success(ProbeMode::Control, nonce).is_err());
        assert!(ProbeResult::success(ProbeMode::Drop, SessionId::pre_session()).is_err());
    }

    #[test]
    fn mode_parser_requires_exact_target_categories() {
        assert_eq!(ProbeMode::parse("drop", 501, 20), Some(ProbeMode::Drop));
        assert_eq!(ProbeMode::parse("drop", 0, 0), None);
        assert_eq!(
            ProbeMode::parse("retain-root", 0, 0),
            Some(ProbeMode::RetainRoot)
        );
        assert_eq!(ProbeMode::parse("retain-root", 501, 20), None);
        assert_eq!(ProbeMode::parse("unknown", 501, 20), None);
    }
}
