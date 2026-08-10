//! Closed wire contract for the test-only elevated bootstrap evidence harness.

use std::fmt;

use crate::{ObjectIdentity, SessionId};

/// Launcher argv activation compiled only into the evidence bundle.
pub const LAUNCHER_ACTIVATION: &str = "--bangbang-internal-elevated-bootstrap-probe-v2";
/// Worker argv activation compiled only into the evidence bundle.
pub const WORKER_ACTIVATION: &str = "--bangbang-internal-elevated-bootstrap-worker-v2";
/// Fixed inherited descriptor carrying the exact private root.
pub const ROOT_FD: libc::c_int = 8;
/// Fixed worker-ready record sent before live code validation completes.
pub const READY_RECORD: [u8; 16] = *b"BBEP-READY-V2\0\0\0";
/// Encoded bootstrap record length.
pub const BOOTSTRAP_RECORD_BYTES: usize = 64;
/// Encoded terminal result record length.
pub const RESULT_RECORD_BYTES: usize = 48;
/// Encoded credential phase record length.
pub const CREDENTIAL_RECORD_BYTES: usize = 80;
/// Encoded credential datagram-possession record length.
pub const CREDENTIAL_DATAGRAM_BYTES: usize = 48;
/// Encoded credential-to-lifecycle continuation acknowledgment length.
pub const CONTINUATION_ACK_BYTES: usize = 48;
/// Encoded launcher-created runtime-session authority length.
pub const RUNTIME_SESSION_AUTHORITY_BYTES: usize = 120;
/// Encoded post-grant guest-evidence record length.
pub const GUEST_EVIDENCE_RECORD_BYTES: usize = 96;
/// Encoded fixed API-listener request or acknowledgment length.
pub const API_LISTENER_RECORD_BYTES: usize = 112;
/// Exact no-API startup-config grant ID.
pub const GUEST_CONFIG_GRANT_ID: &str = "evidence-guest-config";
/// Exact guest kernel grant ID.
pub const GUEST_KERNEL_GRANT_ID: &str = "evidence-guest-kernel";
/// Exact guest initrd grant ID.
pub const GUEST_INITRD_GRANT_ID: &str = "evidence-guest-initrd";
/// Exact read-only root-drive grant ID.
pub const GUEST_ROOTFS_GRANT_ID: &str = "evidence-guest-rootfs";
/// Exact logger-output grant ID.
pub const GUEST_LOGGER_GRANT_ID: &str = "evidence-guest-logger";
/// Exact metrics-output grant ID.
pub const GUEST_METRICS_GRANT_ID: &str = "evidence-guest-metrics";
/// Exact serial-output grant ID.
pub const GUEST_SERIAL_GRANT_ID: &str = "evidence-guest-serial";
/// Exact API socket-directory grant ID.
pub const GUEST_API_DIRECTORY_GRANT_ID: &str = "evidence-guest-api";
/// Exact bounded API socket child.
pub const GUEST_API_SOCKET_CHILD: &str = "evidence-api.sock";
/// Exact contained no-API config-file reference.
pub const GUEST_CONFIG_REFERENCE: &str = "bangbang-grant:evidence-guest-config";
/// Exact contained guest kernel reference.
pub const GUEST_KERNEL_REFERENCE: &str = "bangbang-grant:evidence-guest-kernel";
/// Exact contained guest initrd reference.
pub const GUEST_INITRD_REFERENCE: &str = "bangbang-grant:evidence-guest-initrd";
/// Exact contained read-only root-drive reference.
pub const GUEST_ROOTFS_REFERENCE: &str = "bangbang-grant:evidence-guest-rootfs";
/// Exact contained logger-output reference.
pub const GUEST_LOGGER_REFERENCE: &str = "bangbang-grant:evidence-guest-logger";
/// Exact contained metrics-output reference.
pub const GUEST_METRICS_REFERENCE: &str = "bangbang-grant:evidence-guest-metrics";
/// Exact contained serial-output reference.
pub const GUEST_SERIAL_REFERENCE: &str = "bangbang-grant:evidence-guest-serial";
/// Exact contained API socket reference.
pub const GUEST_API_SOCKET_REFERENCE: &str = "bangbang-grant:evidence-guest-api/evidence-api.sock";
/// Exact canonical guest boot arguments.
pub const GUEST_BOOT_ARGS: &str =
    "console=ttyS0 reboot=k panic=1 quiet loglevel=1 rdinit=/rootfs-poweroff-init";
/// Maximum byte length of either closed guest serial terminal transcript.
pub const MAX_GUEST_SERIAL_TRANSCRIPT_BYTES: usize = 67;
/// Private worker exit used to report the measured target namespace denial.
pub const RUNTIME_NAMESPACE_PERMISSION_EXIT_CODE: u8 = 3;

const GUEST_SUCCESS_SERIAL_LINE: &[u8] = b"BANGBANG_ROOTFS_WORKFLOW_OK\r\n";
const GUEST_FAILURE_SERIAL_LINE: &[u8] = b"BANGBANG_ROOTFS_WORKFLOW_FAIL\r\n";
const GUEST_POWEROFF_SERIAL_SUFFIX: &[u8] = b"] reboot: Power down\r\n";
const GUEST_POWEROFF_TIMESTAMP_BYTES: usize = 12;

/// Closed outcome extracted from the complete pinned guest serial transcript.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuestSerialTranscript {
    /// The exact success record was followed by canonical kernel poweroff.
    Success,
    /// The exact failure record was followed by canonical kernel poweroff.
    Failure,
    /// The transcript was missing, truncated, duplicated, or otherwise noncanonical.
    Invalid,
}

/// Classifies the complete ttyS0 transcript emitted by the pinned guest workflow.
#[must_use]
pub fn classify_guest_serial_transcript(bytes: &[u8]) -> GuestSerialTranscript {
    let (outcome, tail) = if let Some(tail) = bytes.strip_prefix(GUEST_SUCCESS_SERIAL_LINE) {
        (GuestSerialTranscript::Success, tail)
    } else if let Some(tail) = bytes.strip_prefix(GUEST_FAILURE_SERIAL_LINE) {
        (GuestSerialTranscript::Failure, tail)
    } else {
        return GuestSerialTranscript::Invalid;
    };
    if canonical_guest_poweroff_tail(tail) {
        outcome
    } else {
        GuestSerialTranscript::Invalid
    }
}

fn canonical_guest_poweroff_tail(bytes: &[u8]) -> bool {
    let Some(timestamp) = bytes
        .strip_prefix(b"[")
        .and_then(|tail| tail.strip_suffix(GUEST_POWEROFF_SERIAL_SUFFIX))
    else {
        return false;
    };
    if timestamp.len() != GUEST_POWEROFF_TIMESTAMP_BYTES || timestamp.get(5) != Some(&b'.') {
        return false;
    }
    let Some(whole) = timestamp.get(..5) else {
        return false;
    };
    let Some(first_digit) = whole.iter().position(u8::is_ascii_digit) else {
        return false;
    };
    let (Some(spaces), Some(digits), Some(fractional)) = (
        whole.get(..first_digit),
        whole.get(first_digit..),
        timestamp.get(6..),
    ) else {
        return false;
    };
    spaces.iter().all(|byte| *byte == b' ')
        && digits.iter().all(u8::is_ascii_digit)
        && (digits.len() == 1 || digits.first() != Some(&b'0'))
        && fractional.iter().all(u8::is_ascii_digit)
}

const VERSION: u16 = 2;
const BOOTSTRAP_MAGIC: [u8; 4] = *b"BBE2";
const RESULT_MAGIC: [u8; 4] = *b"BBR2";
const CREDENTIAL_VERSION: u16 = 1;
const CREDENTIAL_MAGIC: [u8; 4] = *b"BBC1";
const CREDENTIAL_DATAGRAM_MAGIC: [u8; 4] = *b"BBG1";
const CONTINUATION_ACK_MAGIC: [u8; 4] = *b"BBA1";
const RUNTIME_SESSION_AUTHORITY_MAGIC: [u8; 4] = *b"BBN1";
const RUNTIME_SESSION_AUTHORITY_VERSION: u16 = 1;
const GUEST_EVIDENCE_MAGIC: [u8; 4] = *b"BBW1";
const GUEST_EVIDENCE_VERSION: u16 = 1;
const API_LISTENER_MAGIC: [u8; 4] = *b"BBL1";
const API_LISTENER_VERSION: u16 = 1;
const API_LISTENER_PHASE: u8 = 1;
const API_LISTENER_OPERATION: u8 = 1;
const API_LISTENER_SEQUENCE: u32 = 1;
const RUNTIME_WORKER_FAILURE_EXIT_BASE: u8 = 64;

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
    /// Run a real HVF create/destroy control in the unchrooted signed worker.
    HvfControl = 5,
    /// Inherit a launcher-entered root and run the sandbox/HVF evidence sequence.
    InheritedRoot = 6,
    /// Drop both signed endpoints to one mapped nonzero numeric identity without chroot.
    CredentialDrop = 7,
    /// Retain exact root on both signed endpoints without calling credential mutators.
    CredentialRetainRoot = 8,
    /// Exercise the SDK-maximum unmapped numeric identity without chroot.
    CredentialUnmapped = 9,
    /// Run the same no-chroot credential primitive in the unsandboxed launcher.
    CredentialControl = 10,
    /// Drop both endpoints, then continue into the target-owned runtime.
    RuntimeDrop = 11,
    /// Retain exact root on both endpoints, then continue into the root-owned runtime.
    RuntimeRetainRoot = 12,
    /// Use the SDK-maximum unmapped identity, then attempt the explicit runtime.
    RuntimeUnmapped = 13,
    /// Drop both endpoints, then boot the closed no-API guest workload.
    GuestNoApiDrop = 14,
    /// Retain exact root, then boot the closed no-API guest workload.
    GuestNoApiRetainRoot = 15,
    /// Use the SDK-maximum unmapped identity for the no-API guest workload.
    GuestNoApiUnmapped = 16,
    /// Drop both endpoints, then drive the closed API guest workload.
    GuestApiDrop = 17,
    /// Retain exact root, then drive the closed API guest workload.
    GuestApiRetainRoot = 18,
    /// Use the SDK-maximum unmapped identity for the API guest workload.
    GuestApiUnmapped = 19,
}

/// Closed credential class shared by paired evidence modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialClass {
    /// One mapped, nonzero uid and gid.
    Mapped,
    /// Exact real, effective, and saved uid/gid zero without mutation.
    RetainRoot,
    /// The SDK-maximum deliberately unmapped numeric identity.
    MaximumUnmapped,
}

/// Closed runtime workload selected after the credential exchange.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeWorkload {
    /// Existing representative grant and lifecycle evidence.
    RepresentativeGrants,
    /// Canonical config-file guest startup without an API socket.
    GuestNoApi,
    /// Canonical API-driven guest startup.
    GuestApi,
}

impl RuntimeWorkload {
    /// Returns whether this workload boots one of the closed real guests.
    #[must_use]
    pub const fn is_guest(self) -> bool {
        matches!(self, Self::GuestNoApi | Self::GuestApi)
    }

    /// Returns whether the closed workload needs no session-internal ownership record.
    #[must_use]
    pub const fn supports_retired_record_free_namespace(self) -> bool {
        match self {
            Self::RepresentativeGrants | Self::GuestNoApi | Self::GuestApi => true,
        }
    }
}

impl ProbeMode {
    /// Parses the closed root-wrapper spelling.
    pub fn parse(value: &str, uid: u32, gid: u32) -> Option<Self> {
        let mode = match value {
            "drop" => Self::Drop,
            "retain-root" => Self::RetainRoot,
            "unmapped-syscall" => Self::UnmappedSyscall,
            "control" => Self::Control,
            "hvf-control" => Self::HvfControl,
            "inherited-root" => Self::InheritedRoot,
            "credential-drop" => Self::CredentialDrop,
            "credential-retain-root" => Self::CredentialRetainRoot,
            "credential-unmapped" => Self::CredentialUnmapped,
            "credential-control" => Self::CredentialControl,
            "runtime-drop" => Self::RuntimeDrop,
            "runtime-retain-root" => Self::RuntimeRetainRoot,
            "runtime-unmapped" => Self::RuntimeUnmapped,
            "guest-no-api-drop" => Self::GuestNoApiDrop,
            "guest-no-api-retain-root" => Self::GuestNoApiRetainRoot,
            "guest-no-api-unmapped" => Self::GuestNoApiUnmapped,
            "guest-api-drop" => Self::GuestApiDrop,
            "guest-api-retain-root" => Self::GuestApiRetainRoot,
            "guest-api-unmapped" => Self::GuestApiUnmapped,
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
            Self::HvfControl => "hvf-control",
            Self::InheritedRoot => "inherited-root",
            Self::CredentialDrop => "credential-drop",
            Self::CredentialRetainRoot => "credential-retain-root",
            Self::CredentialUnmapped => "credential-unmapped",
            Self::CredentialControl => "credential-control",
            Self::RuntimeDrop => "runtime-drop",
            Self::RuntimeRetainRoot => "runtime-retain-root",
            Self::RuntimeUnmapped => "runtime-unmapped",
            Self::GuestNoApiDrop => "guest-no-api-drop",
            Self::GuestNoApiRetainRoot => "guest-no-api-retain-root",
            Self::GuestNoApiUnmapped => "guest-no-api-unmapped",
            Self::GuestApiDrop => "guest-api-drop",
            Self::GuestApiRetainRoot => "guest-api-retain-root",
            Self::GuestApiUnmapped => "guest-api-unmapped",
        }
    }

    /// Returns the closed credential class for paired evidence modes.
    #[must_use]
    pub const fn credential_class(self) -> Option<CredentialClass> {
        match self {
            Self::CredentialDrop
            | Self::RuntimeDrop
            | Self::GuestNoApiDrop
            | Self::GuestApiDrop => Some(CredentialClass::Mapped),
            Self::CredentialRetainRoot
            | Self::RuntimeRetainRoot
            | Self::GuestNoApiRetainRoot
            | Self::GuestApiRetainRoot => Some(CredentialClass::RetainRoot),
            Self::CredentialUnmapped
            | Self::RuntimeUnmapped
            | Self::GuestNoApiUnmapped
            | Self::GuestApiUnmapped => Some(CredentialClass::MaximumUnmapped),
            _ => None,
        }
    }

    /// Returns the closed runtime workload, if this mode continues into lifecycle v5.
    #[must_use]
    pub const fn runtime_workload(self) -> Option<RuntimeWorkload> {
        match self {
            Self::RuntimeDrop | Self::RuntimeRetainRoot | Self::RuntimeUnmapped => {
                Some(RuntimeWorkload::RepresentativeGrants)
            }
            Self::GuestNoApiDrop | Self::GuestNoApiRetainRoot | Self::GuestNoApiUnmapped => {
                Some(RuntimeWorkload::GuestNoApi)
            }
            Self::GuestApiDrop | Self::GuestApiRetainRoot | Self::GuestApiUnmapped => {
                Some(RuntimeWorkload::GuestApi)
            }
            _ => None,
        }
    }

    /// Returns whether this mode admits the exact target category.
    #[must_use]
    pub const fn accepts_target(self, uid: u32, gid: u32) -> bool {
        match self.credential_class() {
            Some(CredentialClass::Mapped) => uid != 0 && gid != 0,
            Some(CredentialClass::RetainRoot) => uid == 0 && gid == 0,
            Some(CredentialClass::MaximumUnmapped) => uid == 2_147_483_647 && gid == 2_147_483_647,
            None => match self {
                Self::Drop | Self::UnmappedSyscall => uid != 0 && gid != 0,
                Self::RetainRoot | Self::Control | Self::HvfControl | Self::InheritedRoot => {
                    uid == 0 && gid == 0
                }
                Self::CredentialControl => (uid == 0) == (gid == 0),
                _ => false,
            },
        }
    }

    /// Returns whether this mode runs the paired no-chroot credential protocol.
    #[must_use]
    pub const fn is_credential_pair(self) -> bool {
        self.credential_class().is_some()
    }

    /// Returns whether the credential exchange must hand the same transports to lifecycle v5.
    #[must_use]
    pub const fn continues_runtime(self) -> bool {
        self.runtime_workload().is_some()
    }

    /// Returns whether this mode retains exact root without credential mutation.
    #[must_use]
    pub const fn retains_root(self) -> bool {
        matches!(self.credential_class(), Some(CredentialClass::RetainRoot))
    }

    fn from_byte(value: u8) -> Result<Self, ProbeProtocolError> {
        match value {
            1 => Ok(Self::Drop),
            2 => Ok(Self::RetainRoot),
            3 => Ok(Self::UnmappedSyscall),
            4 => Ok(Self::Control),
            5 => Ok(Self::HvfControl),
            6 => Ok(Self::InheritedRoot),
            7 => Ok(Self::CredentialDrop),
            8 => Ok(Self::CredentialRetainRoot),
            9 => Ok(Self::CredentialUnmapped),
            10 => Ok(Self::CredentialControl),
            11 => Ok(Self::RuntimeDrop),
            12 => Ok(Self::RuntimeRetainRoot),
            13 => Ok(Self::RuntimeUnmapped),
            14 => Ok(Self::GuestNoApiDrop),
            15 => Ok(Self::GuestNoApiRetainRoot),
            16 => Ok(Self::GuestNoApiUnmapped),
            17 => Ok(Self::GuestApiDrop),
            18 => Ok(Self::GuestApiRetainRoot),
            19 => Ok(Self::GuestApiUnmapped),
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
    /// Validate the complete fixed signed bundle staged inside the root.
    ValidateStagedBundle = 8,
    /// Validate the fixed Darwin loader staged inside the root.
    ValidateStagedLoader = 9,
    /// Spawn the signed worker by its fixed path inside the inherited root.
    SpawnWorker = 10,
    /// Validate the newly spawned worker while it remains suspended.
    SuspendedIdentity = 11,
    /// Prove slash and cwd are the exact inherited private root.
    InheritedRoot = 12,
    /// Re-run direct chroot as an App Sandbox denial control.
    SandboxChrootControl = 13,
    /// Revalidate the live worker after its authenticated ready record.
    LiveIdentity = 14,
    /// Create one real process-local Hypervisor.framework VM.
    HvfCreate = 15,
    /// Destroy the real process-local Hypervisor.framework VM.
    HvfDestroy = 16,
    /// Observe the first authenticated application record from the spawned worker.
    WorkerBootstrap = 17,
    /// Complete the exact credential-to-lifecycle acknowledgment.
    ContinuationAck = 18,
    /// Observe the ordinary lifecycle Hello on the reused stream.
    LifecycleHello = 19,
    /// Create or validate the explicit target-owned runtime namespace.
    RuntimeNamespace = 20,
    /// Transfer the exact committed representative grant batch.
    GrantTransfer = 21,
    /// Consume and validate the committed representative grants.
    GrantAccepted = 22,
    /// Cross the ordinary lifecycle Proceed boundary.
    LifecycleProceed = 23,
    /// Observe and validate the ordinary lifecycle terminal record.
    LifecycleTerminal = 24,
    /// Reap and clean every exact owned runtime object.
    RuntimeCleanup = 25,
    /// Create the canonical session as the permanently transitioned launcher.
    RuntimeSessionCreate = 26,
    /// Open and validate independent session descriptions before publication.
    RuntimeSessionOpen = 27,
    /// Publish the canonical session authority and exact descriptor.
    RuntimeAuthoritySend = 28,
    /// Receive the exact session-authority datagram in the worker.
    RuntimeAuthorityReceive = 29,
    /// Validate the authority record, descriptor, and parent association.
    RuntimeAuthorityValidate = 30,
    /// Acquire and revalidate the worker-owned session lock.
    RuntimeSessionLock = 31,
    /// Enter and revalidate the adopted session after Start.
    RuntimeSessionEnter = 32,
    /// Publish and independently validate Prepared for the adopted session.
    LifecyclePrepared = 33,
    /// Validate the exact evidence-only guest argv and grant contract.
    GuestGrantContract = 34,
    /// Revalidate both endpoints immediately before the first guest resource claim.
    GuestResourceWitness = 35,
    /// Publish and validate the exact API socket child.
    ApiSocketPublication = 36,
    /// Configure the exact logger output through the API.
    ApiLoggerConfiguration = 37,
    /// Configure the exact metrics output through the API.
    ApiMetricsConfiguration = 38,
    /// Configure the exact serial output through the API.
    ApiSerialConfiguration = 39,
    /// Configure the closed guest machine shape through the API.
    ApiMachineConfiguration = 40,
    /// Configure the exact boot resources through the API.
    ApiBootConfiguration = 41,
    /// Configure the exact read-only root drive through the API.
    ApiDriveConfiguration = 42,
    /// Issue the single closed InstanceStart action.
    ApiInstanceStart = 43,
    /// Apply the exact canonical config-file startup.
    NoApiStartup = 44,
    /// Revalidate both endpoints immediately before real HVF creation.
    GuestHvfWitness = 45,
    /// Construct the real guest HVF session.
    GuestHvfCreate = 46,
    /// Execute the canonical guest workload.
    GuestExecution = 47,
    /// Observe the exact guest success oracle.
    GuestOracle = 48,
    /// Observe the canonical guest poweroff path.
    GuestPoweroff = 49,
    /// Stop on the bounded guest deadline.
    GuestTimeout = 50,
    /// Stop on premature launcher or worker endpoint death.
    GuestEndpointDeath = 51,
    /// Validate all terminal guest evidence before lifecycle completion.
    GuestTerminalEvidence = 52,
    /// Clean every guest output, socket, and session object.
    GuestCleanup = 53,
    /// Receive the fixed worker request for launcher-created API authority.
    ApiListenerRequest = 54,
    /// Bind and validate the fixed final API listener beneath its anchor.
    ApiListenerBind = 55,
    /// Transfer the exact launcher-created API listener to the worker.
    ApiListenerTransfer = 56,
    /// Adopt and validate the transferred API listener in the worker.
    ApiListenerAdoption = 57,
    /// Retire and independently observe the exact daemon session name.
    RuntimeNamespaceRetirement = 58,
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
            Self::ValidateStagedBundle => "validate-staged-bundle",
            Self::ValidateStagedLoader => "validate-staged-loader",
            Self::SpawnWorker => "spawn-worker",
            Self::SuspendedIdentity => "suspended-identity",
            Self::InheritedRoot => "inherited-root",
            Self::SandboxChrootControl => "sandbox-chroot-control",
            Self::LiveIdentity => "live-identity",
            Self::HvfCreate => "hvf-create",
            Self::HvfDestroy => "hvf-destroy",
            Self::WorkerBootstrap => "worker-bootstrap",
            Self::ContinuationAck => "continuation-ack",
            Self::LifecycleHello => "lifecycle-hello",
            Self::RuntimeNamespace => "runtime-namespace",
            Self::GrantTransfer => "grant-transfer",
            Self::GrantAccepted => "grant-accepted",
            Self::LifecycleProceed => "lifecycle-proceed",
            Self::LifecycleTerminal => "lifecycle-terminal",
            Self::RuntimeCleanup => "runtime-cleanup",
            Self::RuntimeSessionCreate => "runtime-session-create",
            Self::RuntimeSessionOpen => "runtime-session-open",
            Self::RuntimeAuthoritySend => "runtime-authority-send",
            Self::RuntimeAuthorityReceive => "runtime-authority-receive",
            Self::RuntimeAuthorityValidate => "runtime-authority-validate",
            Self::RuntimeSessionLock => "runtime-session-lock",
            Self::RuntimeSessionEnter => "runtime-session-enter",
            Self::LifecyclePrepared => "lifecycle-prepared",
            Self::GuestGrantContract => "guest-grant-contract",
            Self::GuestResourceWitness => "guest-resource-witness",
            Self::ApiSocketPublication => "api-socket-publication",
            Self::ApiLoggerConfiguration => "api-logger-configuration",
            Self::ApiMetricsConfiguration => "api-metrics-configuration",
            Self::ApiSerialConfiguration => "api-serial-configuration",
            Self::ApiMachineConfiguration => "api-machine-configuration",
            Self::ApiBootConfiguration => "api-boot-configuration",
            Self::ApiDriveConfiguration => "api-drive-configuration",
            Self::ApiInstanceStart => "api-instance-start",
            Self::NoApiStartup => "no-api-startup",
            Self::GuestHvfWitness => "guest-hvf-witness",
            Self::GuestHvfCreate => "guest-hvf-create",
            Self::GuestExecution => "guest-execution",
            Self::GuestOracle => "guest-oracle",
            Self::GuestPoweroff => "guest-poweroff",
            Self::GuestTimeout => "guest-timeout",
            Self::GuestEndpointDeath => "guest-endpoint-death",
            Self::GuestTerminalEvidence => "guest-terminal-evidence",
            Self::GuestCleanup => "guest-cleanup",
            Self::ApiListenerRequest => "api-listener-request",
            Self::ApiListenerBind => "api-listener-bind",
            Self::ApiListenerTransfer => "api-listener-transfer",
            Self::ApiListenerAdoption => "api-listener-adoption",
            Self::RuntimeNamespaceRetirement => "runtime-namespace-retirement",
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
            8 => Ok(Self::ValidateStagedBundle),
            9 => Ok(Self::ValidateStagedLoader),
            10 => Ok(Self::SpawnWorker),
            11 => Ok(Self::SuspendedIdentity),
            12 => Ok(Self::InheritedRoot),
            13 => Ok(Self::SandboxChrootControl),
            14 => Ok(Self::LiveIdentity),
            15 => Ok(Self::HvfCreate),
            16 => Ok(Self::HvfDestroy),
            17 => Ok(Self::WorkerBootstrap),
            18 => Ok(Self::ContinuationAck),
            19 => Ok(Self::LifecycleHello),
            20 => Ok(Self::RuntimeNamespace),
            21 => Ok(Self::GrantTransfer),
            22 => Ok(Self::GrantAccepted),
            23 => Ok(Self::LifecycleProceed),
            24 => Ok(Self::LifecycleTerminal),
            25 => Ok(Self::RuntimeCleanup),
            26 => Ok(Self::RuntimeSessionCreate),
            27 => Ok(Self::RuntimeSessionOpen),
            28 => Ok(Self::RuntimeAuthoritySend),
            29 => Ok(Self::RuntimeAuthorityReceive),
            30 => Ok(Self::RuntimeAuthorityValidate),
            31 => Ok(Self::RuntimeSessionLock),
            32 => Ok(Self::RuntimeSessionEnter),
            33 => Ok(Self::LifecyclePrepared),
            34 => Ok(Self::GuestGrantContract),
            35 => Ok(Self::GuestResourceWitness),
            36 => Ok(Self::ApiSocketPublication),
            37 => Ok(Self::ApiLoggerConfiguration),
            38 => Ok(Self::ApiMetricsConfiguration),
            39 => Ok(Self::ApiSerialConfiguration),
            40 => Ok(Self::ApiMachineConfiguration),
            41 => Ok(Self::ApiBootConfiguration),
            42 => Ok(Self::ApiDriveConfiguration),
            43 => Ok(Self::ApiInstanceStart),
            44 => Ok(Self::NoApiStartup),
            45 => Ok(Self::GuestHvfWitness),
            46 => Ok(Self::GuestHvfCreate),
            47 => Ok(Self::GuestExecution),
            48 => Ok(Self::GuestOracle),
            49 => Ok(Self::GuestPoweroff),
            50 => Ok(Self::GuestTimeout),
            51 => Ok(Self::GuestEndpointDeath),
            52 => Ok(Self::GuestTerminalEvidence),
            53 => Ok(Self::GuestCleanup),
            54 => Ok(Self::ApiListenerRequest),
            55 => Ok(Self::ApiListenerBind),
            56 => Ok(Self::ApiListenerTransfer),
            57 => Ok(Self::ApiListenerAdoption),
            58 => Ok(Self::RuntimeNamespaceRetirement),
            _ => Err(ProbeProtocolError),
        }
    }
}

/// Feature-only deterministic stop boundary for runtime-continuation evidence.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum RuntimeFault {
    /// Do not inject a fault.
    #[default]
    None = 0,
    /// Stop immediately before the continuation acknowledgment.
    PreAck = 1,
    /// Stop after the acknowledgment and before lifecycle Hello.
    PostAck = 2,
    /// Stop at target-owned namespace creation.
    Namespace = 3,
    /// Stop while transferring the committed grant batch.
    GrantTransfer = 4,
    /// Stop at the ordinary Proceed boundary.
    Proceed = 5,
    /// Stop at terminal ownership validation.
    Terminal = 6,
    /// Stop while the launcher creates the canonical target session.
    SessionCreate = 7,
    /// Stop while the launcher opens independent session descriptions.
    SessionOpen = 8,
    /// Stop before the launcher publishes session authority.
    AuthoritySend = 9,
    /// Stop before the worker receives session authority.
    AuthorityReceive = 10,
    /// Stop before the worker validates received session authority.
    AuthorityValidate = 11,
    /// Stop before the worker locks the adopted session.
    SessionLock = 12,
    /// Stop before the worker enters the adopted session.
    SessionEnter = 13,
    /// Stop before the worker publishes Prepared for the adopted session.
    Prepared = 14,
    /// Stop while validating the exact guest grant contract.
    GuestGrantContract = 15,
    /// Stop at the first guest-resource witness.
    GuestResourceWitness = 16,
    /// Stop while publishing the API socket.
    ApiSocketPublication = 17,
    /// Stop while configuring the API logger.
    ApiLoggerConfiguration = 18,
    /// Stop while configuring API metrics.
    ApiMetricsConfiguration = 19,
    /// Stop while configuring API serial output.
    ApiSerialConfiguration = 20,
    /// Stop while configuring the API machine shape.
    ApiMachineConfiguration = 21,
    /// Stop while configuring API boot resources.
    ApiBootConfiguration = 22,
    /// Stop while configuring the API root drive.
    ApiDriveConfiguration = 23,
    /// Stop while issuing the API start action.
    ApiInstanceStart = 24,
    /// Stop while applying the no-API startup config.
    NoApiStartup = 25,
    /// Stop at the pre-HVF guest witness.
    GuestHvfWitness = 26,
    /// Stop while constructing the real HVF session.
    GuestHvfCreate = 27,
    /// Stop while the guest is executing.
    GuestExecution = 28,
    /// Stop while validating the guest success oracle.
    GuestOracle = 29,
    /// Stop while validating guest poweroff.
    GuestPoweroff = 30,
    /// Force the bounded guest timeout path.
    GuestTimeout = 31,
    /// Stop after HVF creation for explicit endpoint-death orchestration.
    GuestEndpointDeath = 32,
    /// Stop while validating terminal guest evidence.
    GuestTerminalEvidence = 33,
    /// Stop while cleaning guest-owned objects.
    GuestCleanup = 34,
    /// Stop after the complete boot-grant batch is accepted by the worker.
    GuestGrantAccepted = 35,
    /// Inject one descriptor-free datagram outside the guest witness protocol.
    GuestTransportContamination = 36,
    /// Stop before accepting the fixed API-listener request.
    ApiListenerRequest = 37,
    /// Stop while binding the fixed final API listener.
    ApiListenerBind = 38,
    /// Stop while transferring the launcher-created API listener.
    ApiListenerTransfer = 39,
    /// Stop while the worker adopts and validates the transferred listener.
    ApiListenerAdoption = 40,
    /// Stop after exact listener adoption and before API readiness.
    ApiListenerEndpointDeath = 41,
    /// Stop after launcher validation and before exact namespace unlink.
    NamespaceRetireBeforeUnlink = 42,
    /// Stop after exact unlink and before grant publication begins.
    NamespaceRetireAfterUnlink = 43,
    /// Stop after the worker observes retirement and before receiving a grant.
    NamespaceRetireObserve = 44,
    /// Attempt one forbidden ownership-record write after retirement observation.
    NamespaceRecordWrite = 45,
}

impl RuntimeFault {
    /// Parses the closed wrapper spelling.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "pre-ack" => Some(Self::PreAck),
            "post-ack" => Some(Self::PostAck),
            "namespace" => Some(Self::Namespace),
            "grant-transfer" => Some(Self::GrantTransfer),
            "proceed" => Some(Self::Proceed),
            "terminal" => Some(Self::Terminal),
            "session-create" => Some(Self::SessionCreate),
            "session-open" => Some(Self::SessionOpen),
            "authority-send" => Some(Self::AuthoritySend),
            "authority-receive" => Some(Self::AuthorityReceive),
            "authority-validate" => Some(Self::AuthorityValidate),
            "session-lock" => Some(Self::SessionLock),
            "session-enter" => Some(Self::SessionEnter),
            "prepared" => Some(Self::Prepared),
            "guest-grant-contract" => Some(Self::GuestGrantContract),
            "guest-resource-witness" => Some(Self::GuestResourceWitness),
            "api-socket-publication" => Some(Self::ApiSocketPublication),
            "api-logger-configuration" => Some(Self::ApiLoggerConfiguration),
            "api-metrics-configuration" => Some(Self::ApiMetricsConfiguration),
            "api-serial-configuration" => Some(Self::ApiSerialConfiguration),
            "api-machine-configuration" => Some(Self::ApiMachineConfiguration),
            "api-boot-configuration" => Some(Self::ApiBootConfiguration),
            "api-drive-configuration" => Some(Self::ApiDriveConfiguration),
            "api-instance-start" => Some(Self::ApiInstanceStart),
            "no-api-startup" => Some(Self::NoApiStartup),
            "guest-hvf-witness" => Some(Self::GuestHvfWitness),
            "guest-hvf-create" => Some(Self::GuestHvfCreate),
            "guest-execution" => Some(Self::GuestExecution),
            "guest-oracle" => Some(Self::GuestOracle),
            "guest-poweroff" => Some(Self::GuestPoweroff),
            "guest-timeout" => Some(Self::GuestTimeout),
            "guest-endpoint-death" => Some(Self::GuestEndpointDeath),
            "guest-terminal-evidence" => Some(Self::GuestTerminalEvidence),
            "guest-cleanup" => Some(Self::GuestCleanup),
            "guest-grant-accepted" => Some(Self::GuestGrantAccepted),
            "guest-transport-contamination" => Some(Self::GuestTransportContamination),
            "api-listener-request" => Some(Self::ApiListenerRequest),
            "api-listener-bind" => Some(Self::ApiListenerBind),
            "api-listener-transfer" => Some(Self::ApiListenerTransfer),
            "api-listener-adoption" => Some(Self::ApiListenerAdoption),
            "api-listener-endpoint-death" => Some(Self::ApiListenerEndpointDeath),
            "namespace-retire-before-unlink" => Some(Self::NamespaceRetireBeforeUnlink),
            "namespace-retire-after-unlink" => Some(Self::NamespaceRetireAfterUnlink),
            "namespace-retire-observe" => Some(Self::NamespaceRetireObserve),
            "namespace-record-write" => Some(Self::NamespaceRecordWrite),
            _ => None,
        }
    }

    fn from_byte(value: u8) -> Result<Self, ProbeProtocolError> {
        match value {
            0 => Ok(Self::None),
            1 => Ok(Self::PreAck),
            2 => Ok(Self::PostAck),
            3 => Ok(Self::Namespace),
            4 => Ok(Self::GrantTransfer),
            5 => Ok(Self::Proceed),
            6 => Ok(Self::Terminal),
            7 => Ok(Self::SessionCreate),
            8 => Ok(Self::SessionOpen),
            9 => Ok(Self::AuthoritySend),
            10 => Ok(Self::AuthorityReceive),
            11 => Ok(Self::AuthorityValidate),
            12 => Ok(Self::SessionLock),
            13 => Ok(Self::SessionEnter),
            14 => Ok(Self::Prepared),
            15 => Ok(Self::GuestGrantContract),
            16 => Ok(Self::GuestResourceWitness),
            17 => Ok(Self::ApiSocketPublication),
            18 => Ok(Self::ApiLoggerConfiguration),
            19 => Ok(Self::ApiMetricsConfiguration),
            20 => Ok(Self::ApiSerialConfiguration),
            21 => Ok(Self::ApiMachineConfiguration),
            22 => Ok(Self::ApiBootConfiguration),
            23 => Ok(Self::ApiDriveConfiguration),
            24 => Ok(Self::ApiInstanceStart),
            25 => Ok(Self::NoApiStartup),
            26 => Ok(Self::GuestHvfWitness),
            27 => Ok(Self::GuestHvfCreate),
            28 => Ok(Self::GuestExecution),
            29 => Ok(Self::GuestOracle),
            30 => Ok(Self::GuestPoweroff),
            31 => Ok(Self::GuestTimeout),
            32 => Ok(Self::GuestEndpointDeath),
            33 => Ok(Self::GuestTerminalEvidence),
            34 => Ok(Self::GuestCleanup),
            35 => Ok(Self::GuestGrantAccepted),
            36 => Ok(Self::GuestTransportContamination),
            37 => Ok(Self::ApiListenerRequest),
            38 => Ok(Self::ApiListenerBind),
            39 => Ok(Self::ApiListenerTransfer),
            40 => Ok(Self::ApiListenerAdoption),
            41 => Ok(Self::ApiListenerEndpointDeath),
            42 => Ok(Self::NamespaceRetireBeforeUnlink),
            43 => Ok(Self::NamespaceRetireAfterUnlink),
            44 => Ok(Self::NamespaceRetireObserve),
            45 => Ok(Self::NamespaceRecordWrite),
            _ => Err(ProbeProtocolError),
        }
    }

    /// Returns the stable value-free fault spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::PreAck => "pre-ack",
            Self::PostAck => "post-ack",
            Self::Namespace => "namespace",
            Self::GrantTransfer => "grant-transfer",
            Self::Proceed => "proceed",
            Self::Terminal => "terminal",
            Self::SessionCreate => "session-create",
            Self::SessionOpen => "session-open",
            Self::AuthoritySend => "authority-send",
            Self::AuthorityReceive => "authority-receive",
            Self::AuthorityValidate => "authority-validate",
            Self::SessionLock => "session-lock",
            Self::SessionEnter => "session-enter",
            Self::Prepared => "prepared",
            Self::GuestGrantContract => "guest-grant-contract",
            Self::GuestResourceWitness => "guest-resource-witness",
            Self::ApiSocketPublication => "api-socket-publication",
            Self::ApiLoggerConfiguration => "api-logger-configuration",
            Self::ApiMetricsConfiguration => "api-metrics-configuration",
            Self::ApiSerialConfiguration => "api-serial-configuration",
            Self::ApiMachineConfiguration => "api-machine-configuration",
            Self::ApiBootConfiguration => "api-boot-configuration",
            Self::ApiDriveConfiguration => "api-drive-configuration",
            Self::ApiInstanceStart => "api-instance-start",
            Self::NoApiStartup => "no-api-startup",
            Self::GuestHvfWitness => "guest-hvf-witness",
            Self::GuestHvfCreate => "guest-hvf-create",
            Self::GuestExecution => "guest-execution",
            Self::GuestOracle => "guest-oracle",
            Self::GuestPoweroff => "guest-poweroff",
            Self::GuestTimeout => "guest-timeout",
            Self::GuestEndpointDeath => "guest-endpoint-death",
            Self::GuestTerminalEvidence => "guest-terminal-evidence",
            Self::GuestCleanup => "guest-cleanup",
            Self::GuestGrantAccepted => "guest-grant-accepted",
            Self::GuestTransportContamination => "guest-transport-contamination",
            Self::ApiListenerRequest => "api-listener-request",
            Self::ApiListenerBind => "api-listener-bind",
            Self::ApiListenerTransfer => "api-listener-transfer",
            Self::ApiListenerAdoption => "api-listener-adoption",
            Self::ApiListenerEndpointDeath => "api-listener-endpoint-death",
            Self::NamespaceRetireBeforeUnlink => "namespace-retire-before-unlink",
            Self::NamespaceRetireAfterUnlink => "namespace-retire-after-unlink",
            Self::NamespaceRetireObserve => "namespace-retire-observe",
            Self::NamespaceRecordWrite => "namespace-record-write",
        }
    }

    /// Returns the exact value-free stage forced by this fault.
    #[must_use]
    pub const fn stage(self) -> Option<ProbeStage> {
        match self {
            Self::None => None,
            Self::PreAck => Some(ProbeStage::ContinuationAck),
            Self::PostAck => Some(ProbeStage::LifecycleHello),
            Self::Namespace => Some(ProbeStage::RuntimeNamespace),
            Self::GrantTransfer => Some(ProbeStage::GrantTransfer),
            Self::Proceed => Some(ProbeStage::LifecycleProceed),
            Self::Terminal => Some(ProbeStage::LifecycleTerminal),
            Self::SessionCreate => Some(ProbeStage::RuntimeSessionCreate),
            Self::SessionOpen => Some(ProbeStage::RuntimeSessionOpen),
            Self::AuthoritySend => Some(ProbeStage::RuntimeAuthoritySend),
            Self::AuthorityReceive => Some(ProbeStage::RuntimeAuthorityReceive),
            Self::AuthorityValidate => Some(ProbeStage::RuntimeAuthorityValidate),
            Self::SessionLock => Some(ProbeStage::RuntimeSessionLock),
            Self::SessionEnter => Some(ProbeStage::RuntimeSessionEnter),
            Self::Prepared => Some(ProbeStage::LifecyclePrepared),
            Self::GuestGrantContract => Some(ProbeStage::GuestGrantContract),
            Self::GuestResourceWitness => Some(ProbeStage::GuestResourceWitness),
            Self::ApiSocketPublication => Some(ProbeStage::ApiSocketPublication),
            Self::ApiLoggerConfiguration => Some(ProbeStage::ApiLoggerConfiguration),
            Self::ApiMetricsConfiguration => Some(ProbeStage::ApiMetricsConfiguration),
            Self::ApiSerialConfiguration => Some(ProbeStage::ApiSerialConfiguration),
            Self::ApiMachineConfiguration => Some(ProbeStage::ApiMachineConfiguration),
            Self::ApiBootConfiguration => Some(ProbeStage::ApiBootConfiguration),
            Self::ApiDriveConfiguration => Some(ProbeStage::ApiDriveConfiguration),
            Self::ApiInstanceStart => Some(ProbeStage::ApiInstanceStart),
            Self::NoApiStartup => Some(ProbeStage::NoApiStartup),
            Self::GuestHvfWitness => Some(ProbeStage::GuestHvfWitness),
            Self::GuestHvfCreate => Some(ProbeStage::GuestHvfCreate),
            Self::GuestExecution => Some(ProbeStage::GuestExecution),
            Self::GuestOracle => Some(ProbeStage::GuestOracle),
            Self::GuestPoweroff => Some(ProbeStage::GuestPoweroff),
            Self::GuestTimeout => Some(ProbeStage::GuestTimeout),
            Self::GuestEndpointDeath => Some(ProbeStage::GuestEndpointDeath),
            Self::GuestTerminalEvidence => Some(ProbeStage::GuestTerminalEvidence),
            Self::GuestCleanup => Some(ProbeStage::GuestCleanup),
            Self::GuestGrantAccepted => Some(ProbeStage::GrantAccepted),
            Self::GuestTransportContamination => Some(ProbeStage::GuestResourceWitness),
            Self::ApiListenerRequest => Some(ProbeStage::ApiListenerRequest),
            Self::ApiListenerBind => Some(ProbeStage::ApiListenerBind),
            Self::ApiListenerTransfer => Some(ProbeStage::ApiListenerTransfer),
            Self::ApiListenerAdoption => Some(ProbeStage::ApiListenerAdoption),
            Self::ApiListenerEndpointDeath => Some(ProbeStage::ApiListenerAdoption),
            Self::NamespaceRetireBeforeUnlink
            | Self::NamespaceRetireAfterUnlink
            | Self::NamespaceRetireObserve
            | Self::NamespaceRecordWrite => Some(ProbeStage::RuntimeNamespaceRetirement),
        }
    }
}

/// Value-free capable-host result class for one runtime continuation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeResultClass {
    /// Target/no-drop execution completed the full foreground lifecycle.
    Complete,
    /// The exact credential-to-lifecycle continuation could not be completed.
    ContinuationBoundary,
    /// Live target/no-drop process or code identity could not be revalidated.
    IdentityBoundary,
    /// The explicit root could not be acquired or validated.
    ExplicitRootBoundary,
    /// Namespace creation, locking, validation, or recovery stopped progress.
    NamespaceBoundary,
    /// Grant transfer, commitment, or target-side consumption stopped progress.
    GrantBoundary,
    /// The ordinary lifecycle stopped after committed grants.
    LifecycleBoundary,
    /// API socket publication or one closed API request stopped progress.
    ApiBoundary,
    /// The late witness or real HVF construction stopped progress.
    HvfBoundary,
    /// Guest execution, oracle, poweroff, terminal evidence, or cleanup failed.
    GuestBoundary,
}

impl RuntimeResultClass {
    /// Returns the stable value-free result spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::ContinuationBoundary => "continuation-boundary",
            Self::IdentityBoundary => "identity-boundary",
            Self::ExplicitRootBoundary => "explicit-root-boundary",
            Self::NamespaceBoundary => "namespace-boundary",
            Self::GrantBoundary => "grant-boundary",
            Self::LifecycleBoundary => "lifecycle-boundary",
            Self::ApiBoundary => "api-boundary",
            Self::HvfBoundary => "hvf-boundary",
            Self::GuestBoundary => "guest-boundary",
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

/// Closed private worker exit used before a lifecycle failure can be framed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeWorkerFailure {
    stage: ProbeStage,
    category: ProbeErrorCategory,
}

impl RuntimeWorkerFailure {
    /// Constructs one worker-owned runtime failure.
    pub fn new(
        stage: ProbeStage,
        category: ProbeErrorCategory,
    ) -> Result<Self, ProbeProtocolError> {
        if runtime_worker_stage_index(stage).is_none() {
            return Err(ProbeProtocolError);
        }
        Ok(Self { stage, category })
    }

    /// Returns the exact value-free stage.
    #[must_use]
    pub const fn stage(self) -> ProbeStage {
        self.stage
    }

    /// Returns the exact value-free category.
    #[must_use]
    pub const fn category(self) -> ProbeErrorCategory {
        self.category
    }

    /// Encodes the failure into the reserved feature-only worker exit range.
    #[must_use]
    pub fn exit_code(self) -> u8 {
        let stage = runtime_worker_stage_index(self.stage).unwrap_or(0);
        RUNTIME_WORKER_FAILURE_EXIT_BASE + stage * 3 + self.category as u8 - 1
    }

    /// Decodes one exact feature-only worker exit.
    pub fn from_exit_code(exit_code: u8) -> Result<Self, ProbeProtocolError> {
        let offset = exit_code
            .checked_sub(RUNTIME_WORKER_FAILURE_EXIT_BASE)
            .ok_or(ProbeProtocolError)?;
        let stage = runtime_worker_stage(offset / 3).ok_or(ProbeProtocolError)?;
        let category = ProbeErrorCategory::from_byte(offset % 3 + 1)?;
        Self::new(stage, category)
    }
}

const fn runtime_worker_stage_index(stage: ProbeStage) -> Option<u8> {
    match stage {
        ProbeStage::RuntimeAuthorityReceive => Some(0),
        ProbeStage::RuntimeAuthorityValidate => Some(1),
        ProbeStage::RuntimeSessionLock => Some(2),
        ProbeStage::RuntimeSessionEnter => Some(3),
        ProbeStage::LifecyclePrepared => Some(4),
        ProbeStage::RuntimeNamespaceRetirement => Some(5),
        _ => None,
    }
}

const fn runtime_worker_stage(index: u8) -> Option<ProbeStage> {
    match index {
        0 => Some(ProbeStage::RuntimeAuthorityReceive),
        1 => Some(ProbeStage::RuntimeAuthorityValidate),
        2 => Some(ProbeStage::RuntimeSessionLock),
        3 => Some(ProbeStage::RuntimeSessionEnter),
        4 => Some(ProbeStage::LifecyclePrepared),
        5 => Some(ProbeStage::RuntimeNamespaceRetirement),
        _ => None,
    }
}

/// Fixed process role in the credential evidence exchange.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CredentialRole {
    /// The unsandboxed outer launcher endpoint.
    Launcher = 1,
    /// The mandatory App Sandbox and Hypervisor worker endpoint.
    Worker = 2,
}

impl CredentialRole {
    fn from_byte(value: u8) -> Result<Self, ProbeProtocolError> {
        match value {
            1 => Ok(Self::Launcher),
            2 => Ok(Self::Worker),
            _ => Err(ProbeProtocolError),
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

/// Exact phase in the credential datagram possession barrier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CredentialDatagramPhase {
    /// Launcher proves possession after sending the stream bootstrap.
    Challenge = 1,
    /// Worker proves possession while both endpoints remain root.
    WorkerReady = 2,
    /// Launcher releases the worker only after its own initial observation.
    LauncherRelease = 3,
}

impl CredentialDatagramPhase {
    fn from_byte(value: u8) -> Result<Self, ProbeProtocolError> {
        match value {
            1 => Ok(Self::Challenge),
            2 => Ok(Self::WorkerReady),
            3 => Ok(Self::LauncherRelease),
            _ => Err(ProbeProtocolError),
        }
    }
}

/// Nonce-bound proof that the expected process owns the inherited datagram endpoint.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CredentialDatagramProof {
    mode: ProbeMode,
    phase: CredentialDatagramPhase,
    role: CredentialRole,
    nonce: SessionId,
}

impl CredentialDatagramProof {
    /// Constructs the launcher challenge.
    pub fn challenge(mode: ProbeMode, nonce: SessionId) -> Result<Self, ProbeProtocolError> {
        Self::new(
            mode,
            CredentialDatagramPhase::Challenge,
            CredentialRole::Launcher,
            nonce,
        )
    }

    /// Constructs the worker possession response.
    pub fn worker_ready(mode: ProbeMode, nonce: SessionId) -> Result<Self, ProbeProtocolError> {
        Self::new(
            mode,
            CredentialDatagramPhase::WorkerReady,
            CredentialRole::Worker,
            nonce,
        )
    }

    /// Constructs the launcher release barrier.
    pub fn launcher_release(mode: ProbeMode, nonce: SessionId) -> Result<Self, ProbeProtocolError> {
        Self::new(
            mode,
            CredentialDatagramPhase::LauncherRelease,
            CredentialRole::Launcher,
            nonce,
        )
    }

    fn new(
        mode: ProbeMode,
        phase: CredentialDatagramPhase,
        role: CredentialRole,
        nonce: SessionId,
    ) -> Result<Self, ProbeProtocolError> {
        let expected_role = match phase {
            CredentialDatagramPhase::Challenge | CredentialDatagramPhase::LauncherRelease => {
                CredentialRole::Launcher
            }
            CredentialDatagramPhase::WorkerReady => CredentialRole::Worker,
        };
        if !mode.is_credential_pair() || nonce.is_pre_session() || role != expected_role {
            return Err(ProbeProtocolError);
        }
        Ok(Self {
            mode,
            phase,
            role,
            nonce,
        })
    }

    /// Returns the selected credential mode.
    #[must_use]
    pub const fn mode(self) -> ProbeMode {
        self.mode
    }

    /// Returns the exact barrier phase.
    #[must_use]
    pub const fn phase(self) -> CredentialDatagramPhase {
        self.phase
    }

    /// Returns the sending process role.
    #[must_use]
    pub const fn role(self) -> CredentialRole {
        self.role
    }

    /// Returns the command nonce.
    #[must_use]
    pub const fn nonce(self) -> SessionId {
        self.nonce
    }

    /// Returns whether this proof is the exact next barrier frame.
    #[must_use]
    pub fn matches_expected(
        self,
        mode: ProbeMode,
        phase: CredentialDatagramPhase,
        role: CredentialRole,
        nonce: SessionId,
    ) -> bool {
        self.mode == mode && self.phase == phase && self.role == role && self.nonce == nonce
    }

    /// Encodes the exact datagram record.
    #[must_use]
    pub fn encode(self) -> [u8; CREDENTIAL_DATAGRAM_BYTES] {
        let mut bytes = [0_u8; CREDENTIAL_DATAGRAM_BYTES];
        bytes[0..4].copy_from_slice(&CREDENTIAL_DATAGRAM_MAGIC);
        bytes[4..6].copy_from_slice(&CREDENTIAL_VERSION.to_be_bytes());
        bytes[6] = self.mode as u8;
        bytes[7] = self.phase as u8;
        bytes[8] = self.role as u8;
        bytes[16..48].copy_from_slice(self.nonce.as_bytes());
        bytes
    }

    /// Decodes and validates one exact datagram record.
    pub fn decode(bytes: &[u8; CREDENTIAL_DATAGRAM_BYTES]) -> Result<Self, ProbeProtocolError> {
        if bytes[0..4] != CREDENTIAL_DATAGRAM_MAGIC
            || bytes[4..6] != CREDENTIAL_VERSION.to_be_bytes()
            || bytes[9..16] != [0; 7]
        {
            return Err(ProbeProtocolError);
        }
        Self::new(
            ProbeMode::from_byte(bytes[6])?,
            CredentialDatagramPhase::from_byte(bytes[7])?,
            CredentialRole::from_byte(bytes[8])?,
            SessionId::from_bytes(array(bytes, 16)?),
        )
    }
}

impl fmt::Debug for CredentialDatagramProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CredentialDatagramProof(<redacted>)")
    }
}

/// Exact single-use handoff from the credential exchange to lifecycle v5.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ContinuationAck {
    mode: ProbeMode,
    role: CredentialRole,
    nonce: SessionId,
}

impl ContinuationAck {
    /// Constructs the launcher's acknowledgment for one runtime continuation.
    pub fn launcher(mode: ProbeMode, nonce: SessionId) -> Result<Self, ProbeProtocolError> {
        if !mode.continues_runtime() || nonce.is_pre_session() {
            return Err(ProbeProtocolError);
        }
        Ok(Self {
            mode,
            role: CredentialRole::Launcher,
            nonce,
        })
    }

    /// Returns the selected runtime mode.
    #[must_use]
    pub const fn mode(self) -> ProbeMode {
        self.mode
    }

    /// Returns the fixed acknowledging role.
    #[must_use]
    pub const fn role(self) -> CredentialRole {
        self.role
    }

    /// Returns the exact command nonce.
    #[must_use]
    pub const fn nonce(self) -> SessionId {
        self.nonce
    }

    /// Returns whether this is the exact acknowledgment for an exchange.
    #[must_use]
    pub fn matches_expected(self, mode: ProbeMode, nonce: SessionId) -> bool {
        self.mode == mode && self.role == CredentialRole::Launcher && self.nonce == nonce
    }

    /// Encodes the fixed acknowledgment record.
    #[must_use]
    pub fn encode(self) -> [u8; CONTINUATION_ACK_BYTES] {
        let mut bytes = [0_u8; CONTINUATION_ACK_BYTES];
        bytes[0..4].copy_from_slice(&CONTINUATION_ACK_MAGIC);
        bytes[4..6].copy_from_slice(&CREDENTIAL_VERSION.to_be_bytes());
        bytes[6] = self.mode as u8;
        bytes[7] = self.role as u8;
        bytes[16..48].copy_from_slice(self.nonce.as_bytes());
        bytes
    }

    /// Decodes and canonically validates one acknowledgment record.
    pub fn decode(bytes: &[u8; CONTINUATION_ACK_BYTES]) -> Result<Self, ProbeProtocolError> {
        if bytes[0..4] != CONTINUATION_ACK_MAGIC
            || bytes[4..6] != CREDENTIAL_VERSION.to_be_bytes()
            || bytes[8..16] != [0; 8]
        {
            return Err(ProbeProtocolError);
        }
        let ack = Self::launcher(
            ProbeMode::from_byte(bytes[6])?,
            SessionId::from_bytes(array(bytes, 16)?),
        )?;
        if CredentialRole::from_byte(bytes[7])? != CredentialRole::Launcher
            || ack.encode() != *bytes
        {
            return Err(ProbeProtocolError);
        }
        Ok(ack)
    }
}

impl fmt::Debug for ContinuationAck {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ContinuationAck(<redacted>)")
    }
}

/// Canonical one-descriptor authority for a launcher-created runtime session.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RuntimeSessionAuthority {
    mode: ProbeMode,
    role: CredentialRole,
    target_uid: u32,
    target_gid: u32,
    root: ObjectIdentity,
    session_identity: ObjectIdentity,
    nonce: SessionId,
    session: SessionId,
}

impl RuntimeSessionAuthority {
    /// Constructs the launcher's exact authority for one adopted session.
    pub fn launcher(
        mode: ProbeMode,
        target_uid: u32,
        target_gid: u32,
        root: ObjectIdentity,
        session_identity: ObjectIdentity,
        nonce: SessionId,
        session: SessionId,
    ) -> Result<Self, ProbeProtocolError> {
        if !mode.continues_runtime()
            || !mode.accepts_target(target_uid, target_gid)
            || root.device == 0
            || root.inode == 0
            || session_identity.device == 0
            || session_identity.inode == 0
            || nonce.is_pre_session()
            || session.is_pre_session()
        {
            return Err(ProbeProtocolError);
        }
        Ok(Self {
            mode,
            role: CredentialRole::Launcher,
            target_uid,
            target_gid,
            root,
            session_identity,
            nonce,
            session,
        })
    }

    /// Returns the selected runtime mode.
    #[must_use]
    pub const fn mode(self) -> ProbeMode {
        self.mode
    }

    /// Returns the fixed sending role.
    #[must_use]
    pub const fn role(self) -> CredentialRole {
        self.role
    }

    /// Returns the exact target uid.
    #[must_use]
    pub const fn target_uid(self) -> u32 {
        self.target_uid
    }

    /// Returns the exact target gid.
    #[must_use]
    pub const fn target_gid(self) -> u32 {
        self.target_gid
    }

    /// Returns the exact inherited runtime-root identity.
    #[must_use]
    pub const fn root(self) -> ObjectIdentity {
        self.root
    }

    /// Returns the exact launcher-created session identity.
    #[must_use]
    pub const fn session_identity(self) -> ObjectIdentity {
        self.session_identity
    }

    /// Returns the credential/bootstrap nonce.
    #[must_use]
    pub const fn nonce(self) -> SessionId {
        self.nonce
    }

    /// Returns the lifecycle session bound to the authority.
    #[must_use]
    pub const fn session(self) -> SessionId {
        self.session
    }

    /// Returns whether every bootstrap, session, and object field is exact.
    #[must_use]
    pub fn matches_expected(
        self,
        bootstrap: ProbeBootstrap,
        session: SessionId,
        session_identity: ObjectIdentity,
    ) -> bool {
        self.mode == bootstrap.mode()
            && self.role == CredentialRole::Launcher
            && self.target_uid == bootstrap.target_uid()
            && self.target_gid == bootstrap.target_gid()
            && self.root == bootstrap.root()
            && self.session_identity == session_identity
            && self.nonce == bootstrap.nonce()
            && self.session == session
    }

    /// Encodes the fixed canonical authority record.
    #[must_use]
    pub fn encode(self) -> [u8; RUNTIME_SESSION_AUTHORITY_BYTES] {
        let mut bytes = [0_u8; RUNTIME_SESSION_AUTHORITY_BYTES];
        bytes[0..4].copy_from_slice(&RUNTIME_SESSION_AUTHORITY_MAGIC);
        bytes[4..6].copy_from_slice(&RUNTIME_SESSION_AUTHORITY_VERSION.to_be_bytes());
        bytes[6] = self.mode as u8;
        bytes[7] = self.role as u8;
        bytes[8] = 1;
        bytes[16..20].copy_from_slice(&self.target_uid.to_be_bytes());
        bytes[20..24].copy_from_slice(&self.target_gid.to_be_bytes());
        bytes[24..32].copy_from_slice(&self.root.device.to_be_bytes());
        bytes[32..40].copy_from_slice(&self.root.inode.to_be_bytes());
        bytes[40..48].copy_from_slice(&self.session_identity.device.to_be_bytes());
        bytes[48..56].copy_from_slice(&self.session_identity.inode.to_be_bytes());
        bytes[56..88].copy_from_slice(self.nonce.as_bytes());
        bytes[88..120].copy_from_slice(self.session.as_bytes());
        bytes
    }

    /// Decodes and validates one exact authority record.
    pub fn decode(
        bytes: &[u8; RUNTIME_SESSION_AUTHORITY_BYTES],
    ) -> Result<Self, ProbeProtocolError> {
        if bytes[0..4] != RUNTIME_SESSION_AUTHORITY_MAGIC
            || bytes[4..6] != RUNTIME_SESSION_AUTHORITY_VERSION.to_be_bytes()
            || bytes[8] != 1
            || bytes[9..16] != [0; 7]
        {
            return Err(ProbeProtocolError);
        }
        let authority = Self::launcher(
            ProbeMode::from_byte(bytes[6])?,
            u32::from_be_bytes(array(bytes, 16)?),
            u32::from_be_bytes(array(bytes, 20)?),
            ObjectIdentity {
                device: u64::from_be_bytes(array(bytes, 24)?),
                inode: u64::from_be_bytes(array(bytes, 32)?),
            },
            ObjectIdentity {
                device: u64::from_be_bytes(array(bytes, 40)?),
                inode: u64::from_be_bytes(array(bytes, 48)?),
            },
            SessionId::from_bytes(array(bytes, 56)?),
            SessionId::from_bytes(array(bytes, 88)?),
        )?;
        if CredentialRole::from_byte(bytes[7])? != CredentialRole::Launcher
            || authority.encode() != *bytes
        {
            return Err(ProbeProtocolError);
        }
        Ok(authority)
    }
}

impl fmt::Debug for RuntimeSessionAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RuntimeSessionAuthority(<redacted>)")
    }
}

/// Direction of the one fixed API-listener authority exchange.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ApiListenerKind {
    /// Worker requests the fixed final API listener without ancillary authority.
    Request = 1,
    /// Launcher acknowledges the request with exactly one listener descriptor.
    Ack = 2,
}

impl ApiListenerKind {
    fn from_byte(value: u8) -> Result<Self, ProbeProtocolError> {
        match value {
            1 => Ok(Self::Request),
            2 => Ok(Self::Ack),
            _ => Err(ProbeProtocolError),
        }
    }
}

/// Canonical request or acknowledgment for the fixed elevated API listener.
///
/// The phase, sequence, operation, and child name are deliberately not caller
/// selected. The one operation always names [`GUEST_API_SOCKET_CHILD`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ApiListenerRecord {
    mode: ProbeMode,
    kind: ApiListenerKind,
    role: CredentialRole,
    descriptor_count: u8,
    nonce: SessionId,
    session: SessionId,
    path_identity: Option<ObjectIdentity>,
}

impl ApiListenerRecord {
    /// Constructs the worker's descriptor-free request for the fixed listener.
    pub fn worker_request(
        mode: ProbeMode,
        nonce: SessionId,
        session: SessionId,
    ) -> Result<Self, ProbeProtocolError> {
        Self::new(
            mode,
            ApiListenerKind::Request,
            CredentialRole::Worker,
            0,
            nonce,
            session,
            None,
        )
    }

    /// Constructs the launcher's one-descriptor acknowledgment.
    pub fn launcher_ack(
        mode: ProbeMode,
        nonce: SessionId,
        session: SessionId,
        path_identity: ObjectIdentity,
    ) -> Result<Self, ProbeProtocolError> {
        Self::new(
            mode,
            ApiListenerKind::Ack,
            CredentialRole::Launcher,
            1,
            nonce,
            session,
            Some(path_identity),
        )
    }

    fn new(
        mode: ProbeMode,
        kind: ApiListenerKind,
        role: CredentialRole,
        descriptor_count: u8,
        nonce: SessionId,
        session: SessionId,
        path_identity: Option<ObjectIdentity>,
    ) -> Result<Self, ProbeProtocolError> {
        if mode.runtime_workload() != Some(RuntimeWorkload::GuestApi)
            || nonce.is_pre_session()
            || session.is_pre_session()
        {
            return Err(ProbeProtocolError);
        }
        let valid_shape = match (kind, role, descriptor_count, path_identity) {
            (ApiListenerKind::Request, CredentialRole::Worker, 0, None) => true,
            (
                ApiListenerKind::Ack,
                CredentialRole::Launcher,
                1,
                Some(ObjectIdentity { device, inode }),
            ) => device != 0 && inode != 0,
            _ => false,
        };
        if !valid_shape {
            return Err(ProbeProtocolError);
        }
        Ok(Self {
            mode,
            kind,
            role,
            descriptor_count,
            nonce,
            session,
            path_identity,
        })
    }

    /// Returns the selected API guest mode.
    #[must_use]
    pub const fn mode(self) -> ProbeMode {
        self.mode
    }

    /// Returns whether this is the request or acknowledgment.
    #[must_use]
    pub const fn kind(self) -> ApiListenerKind {
        self.kind
    }

    /// Returns the exact sender role implied by the record kind.
    #[must_use]
    pub const fn role(self) -> CredentialRole {
        self.role
    }

    /// Returns the exact ancillary-descriptor count implied by the record kind.
    #[must_use]
    pub const fn descriptor_count(self) -> u8 {
        self.descriptor_count
    }

    /// Returns the bootstrap nonce.
    #[must_use]
    pub const fn nonce(self) -> SessionId {
        self.nonce
    }

    /// Returns the lifecycle session identity.
    #[must_use]
    pub const fn session(self) -> SessionId {
        self.session
    }

    /// Returns the final socket pathname identity carried only by an acknowledgment.
    #[must_use]
    pub const fn path_identity(self) -> Option<ObjectIdentity> {
        self.path_identity
    }

    /// Returns the only child selected by this closed operation.
    #[must_use]
    pub const fn child(self) -> &'static str {
        GUEST_API_SOCKET_CHILD
    }

    /// Returns whether every correlation and kind-specific field is exact.
    #[must_use]
    pub fn matches_expected(
        self,
        mode: ProbeMode,
        kind: ApiListenerKind,
        nonce: SessionId,
        session: SessionId,
        path_identity: Option<ObjectIdentity>,
    ) -> bool {
        let (role, descriptor_count) = match kind {
            ApiListenerKind::Request => (CredentialRole::Worker, 0),
            ApiListenerKind::Ack => (CredentialRole::Launcher, 1),
        };
        self.mode == mode
            && self.kind == kind
            && self.role == role
            && self.descriptor_count == descriptor_count
            && self.nonce == nonce
            && self.session == session
            && self.path_identity == path_identity
    }

    /// Encodes the fixed canonical listener-authority record.
    #[must_use]
    pub fn encode(self) -> [u8; API_LISTENER_RECORD_BYTES] {
        let mut bytes = [0_u8; API_LISTENER_RECORD_BYTES];
        bytes[0..4].copy_from_slice(&API_LISTENER_MAGIC);
        bytes[4..6].copy_from_slice(&API_LISTENER_VERSION.to_be_bytes());
        bytes[6] = self.mode as u8;
        bytes[7] = API_LISTENER_PHASE;
        bytes[8] = self.kind as u8;
        bytes[9] = self.role as u8;
        bytes[10] = API_LISTENER_OPERATION;
        bytes[11] = self.descriptor_count;
        bytes[12..16].copy_from_slice(&API_LISTENER_SEQUENCE.to_be_bytes());
        bytes[16..48].copy_from_slice(self.nonce.as_bytes());
        bytes[48..80].copy_from_slice(self.session.as_bytes());
        if let Some(identity) = self.path_identity {
            bytes[80..88].copy_from_slice(&identity.device.to_be_bytes());
            bytes[88..96].copy_from_slice(&identity.inode.to_be_bytes());
        }
        bytes
    }

    /// Decodes and validates one exact canonical listener-authority record.
    pub fn decode(bytes: &[u8; API_LISTENER_RECORD_BYTES]) -> Result<Self, ProbeProtocolError> {
        if bytes[0..4] != API_LISTENER_MAGIC
            || bytes[4..6] != API_LISTENER_VERSION.to_be_bytes()
            || bytes[7] != API_LISTENER_PHASE
            || bytes[10] != API_LISTENER_OPERATION
            || u32::from_be_bytes(array(bytes, 12)?) != API_LISTENER_SEQUENCE
            || bytes[96..112] != [0; 16]
        {
            return Err(ProbeProtocolError);
        }
        let kind = ApiListenerKind::from_byte(bytes[8])?;
        let identity = ObjectIdentity {
            device: u64::from_be_bytes(array(bytes, 80)?),
            inode: u64::from_be_bytes(array(bytes, 88)?),
        };
        let path_identity = match kind {
            ApiListenerKind::Request if identity.device == 0 && identity.inode == 0 => None,
            ApiListenerKind::Ack => Some(identity),
            _ => return Err(ProbeProtocolError),
        };
        let record = Self::new(
            ProbeMode::from_byte(bytes[6])?,
            kind,
            CredentialRole::from_byte(bytes[9])?,
            bytes[11],
            SessionId::from_bytes(array(bytes, 16)?),
            SessionId::from_bytes(array(bytes, 48)?),
            path_identity,
        )?;
        if record.encode() != *bytes {
            return Err(ProbeProtocolError);
        }
        Ok(record)
    }
}

impl fmt::Debug for ApiListenerRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ApiListenerRecord(<redacted>)")
    }
}

/// Exact phase in the post-grant guest evidence exchange.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum GuestEvidencePhase {
    /// Worker is about to claim its first guest resource authority.
    ResourceClaim = 1,
    /// Worker is about to construct the real HVF guest session.
    HvfCreate = 2,
    /// Worker has successfully constructed the real HVF guest session.
    HvfCreated = 3,
    /// Worker observed canonical guest shutdown after the success oracle.
    GuestShutdown = 4,
}

impl GuestEvidencePhase {
    const fn sequence(self) -> u32 {
        self as u32
    }

    fn from_byte(value: u8) -> Result<Self, ProbeProtocolError> {
        match value {
            1 => Ok(Self::ResourceClaim),
            2 => Ok(Self::HvfCreate),
            3 => Ok(Self::HvfCreated),
            4 => Ok(Self::GuestShutdown),
            _ => Err(ProbeProtocolError),
        }
    }
}

/// Direction and purpose of one guest-evidence record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum GuestEvidenceKind {
    /// Worker requests one exact late revalidation barrier.
    Request = 1,
    /// Launcher acknowledges one exact request after revalidation.
    Ack = 2,
    /// Worker reports one exact value-free milestone.
    Report = 3,
}

impl GuestEvidenceKind {
    fn from_byte(value: u8) -> Result<Self, ProbeProtocolError> {
        match value {
            1 => Ok(Self::Request),
            2 => Ok(Self::Ack),
            3 => Ok(Self::Report),
            _ => Err(ProbeProtocolError),
        }
    }
}

/// Canonical post-grant record carried by the already authenticated grant datagram.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct GuestEvidenceRecord {
    mode: ProbeMode,
    phase: GuestEvidencePhase,
    kind: GuestEvidenceKind,
    role: CredentialRole,
    sequence: u32,
    nonce: SessionId,
    session: SessionId,
}

impl GuestEvidenceRecord {
    /// Constructs a worker request for one of the two late witness barriers.
    pub fn worker_request(
        mode: ProbeMode,
        phase: GuestEvidencePhase,
        nonce: SessionId,
        session: SessionId,
    ) -> Result<Self, ProbeProtocolError> {
        Self::new(
            mode,
            phase,
            GuestEvidenceKind::Request,
            CredentialRole::Worker,
            nonce,
            session,
        )
    }

    /// Constructs the launcher's exact acknowledgment for one late witness barrier.
    pub fn launcher_ack(
        mode: ProbeMode,
        phase: GuestEvidencePhase,
        nonce: SessionId,
        session: SessionId,
    ) -> Result<Self, ProbeProtocolError> {
        Self::new(
            mode,
            phase,
            GuestEvidenceKind::Ack,
            CredentialRole::Launcher,
            nonce,
            session,
        )
    }

    /// Constructs one value-free worker milestone report.
    pub fn worker_report(
        mode: ProbeMode,
        phase: GuestEvidencePhase,
        nonce: SessionId,
        session: SessionId,
    ) -> Result<Self, ProbeProtocolError> {
        Self::new(
            mode,
            phase,
            GuestEvidenceKind::Report,
            CredentialRole::Worker,
            nonce,
            session,
        )
    }

    fn new(
        mode: ProbeMode,
        phase: GuestEvidencePhase,
        kind: GuestEvidenceKind,
        role: CredentialRole,
        nonce: SessionId,
        session: SessionId,
    ) -> Result<Self, ProbeProtocolError> {
        if !matches!(
            mode.runtime_workload(),
            Some(RuntimeWorkload::GuestNoApi | RuntimeWorkload::GuestApi)
        ) || nonce.is_pre_session()
            || session.is_pre_session()
        {
            return Err(ProbeProtocolError);
        }
        let valid_shape = matches!(
            (phase, kind, role),
            (
                GuestEvidencePhase::ResourceClaim | GuestEvidencePhase::HvfCreate,
                GuestEvidenceKind::Request,
                CredentialRole::Worker
            ) | (
                GuestEvidencePhase::ResourceClaim | GuestEvidencePhase::HvfCreate,
                GuestEvidenceKind::Ack,
                CredentialRole::Launcher
            ) | (
                GuestEvidencePhase::HvfCreated | GuestEvidencePhase::GuestShutdown,
                GuestEvidenceKind::Report,
                CredentialRole::Worker
            )
        );
        if !valid_shape {
            return Err(ProbeProtocolError);
        }
        Ok(Self {
            mode,
            phase,
            kind,
            role,
            sequence: phase.sequence(),
            nonce,
            session,
        })
    }

    /// Returns the selected guest mode.
    #[must_use]
    pub const fn mode(self) -> ProbeMode {
        self.mode
    }

    /// Returns the exact exchange phase.
    #[must_use]
    pub const fn phase(self) -> GuestEvidencePhase {
        self.phase
    }

    /// Returns the exact record kind.
    #[must_use]
    pub const fn kind(self) -> GuestEvidenceKind {
        self.kind
    }

    /// Returns the exact sender role.
    #[must_use]
    pub const fn role(self) -> CredentialRole {
        self.role
    }

    /// Returns the canonical monotonic sequence number.
    #[must_use]
    pub const fn sequence(self) -> u32 {
        self.sequence
    }

    /// Returns the bootstrap nonce.
    #[must_use]
    pub const fn nonce(self) -> SessionId {
        self.nonce
    }

    /// Returns the lifecycle session.
    #[must_use]
    pub const fn session(self) -> SessionId {
        self.session
    }

    /// Returns whether every binding and state-machine field is exact.
    #[must_use]
    pub fn matches_expected(
        self,
        mode: ProbeMode,
        phase: GuestEvidencePhase,
        kind: GuestEvidenceKind,
        role: CredentialRole,
        nonce: SessionId,
        session: SessionId,
    ) -> bool {
        self.mode == mode
            && self.phase == phase
            && self.kind == kind
            && self.role == role
            && self.sequence == phase.sequence()
            && self.nonce == nonce
            && self.session == session
    }

    /// Encodes the fixed canonical record.
    #[must_use]
    pub fn encode(self) -> [u8; GUEST_EVIDENCE_RECORD_BYTES] {
        let mut bytes = [0_u8; GUEST_EVIDENCE_RECORD_BYTES];
        bytes[0..4].copy_from_slice(&GUEST_EVIDENCE_MAGIC);
        bytes[4..6].copy_from_slice(&GUEST_EVIDENCE_VERSION.to_be_bytes());
        bytes[6] = self.mode as u8;
        bytes[7] = self.phase as u8;
        bytes[8] = self.kind as u8;
        bytes[9] = self.role as u8;
        bytes[12..16].copy_from_slice(&self.sequence.to_be_bytes());
        bytes[16..48].copy_from_slice(self.nonce.as_bytes());
        bytes[48..80].copy_from_slice(self.session.as_bytes());
        bytes
    }

    /// Decodes and validates one exact canonical record.
    pub fn decode(bytes: &[u8; GUEST_EVIDENCE_RECORD_BYTES]) -> Result<Self, ProbeProtocolError> {
        if bytes[0..4] != GUEST_EVIDENCE_MAGIC
            || bytes[4..6] != GUEST_EVIDENCE_VERSION.to_be_bytes()
            || bytes[10..12] != [0; 2]
            || bytes[80..96] != [0; 16]
        {
            return Err(ProbeProtocolError);
        }
        let phase = GuestEvidencePhase::from_byte(bytes[7])?;
        let record = Self::new(
            ProbeMode::from_byte(bytes[6])?,
            phase,
            GuestEvidenceKind::from_byte(bytes[8])?,
            CredentialRole::from_byte(bytes[9])?,
            SessionId::from_bytes(array(bytes, 16)?),
            SessionId::from_bytes(array(bytes, 48)?),
        )?;
        if u32::from_be_bytes(array(bytes, 12)?) != phase.sequence() || record.encode() != *bytes {
            return Err(ProbeProtocolError);
        }
        Ok(record)
    }
}

impl fmt::Debug for GuestEvidenceRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GuestEvidenceRecord(<redacted>)")
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
    fn from_byte(value: u8) -> Result<Self, ProbeProtocolError> {
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
            _ => Err(ProbeProtocolError),
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
    fn from_byte(value: u8) -> Result<Self, ProbeProtocolError> {
        match value {
            0 => Ok(Self::None),
            1 => Ok(Self::Initial),
            2 => Ok(Self::GroupsCleared),
            3 => Ok(Self::GidSet),
            4 => Ok(Self::UidSet),
            5 => Ok(Self::FinalIdentity),
            6 => Ok(Self::Irreversible),
            7 => Ok(Self::RetainedRoot),
            _ => Err(ProbeProtocolError),
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
    fn from_byte(value: u8) -> Result<Self, ProbeProtocolError> {
        match value {
            0 => Ok(Self::NotObserved),
            1 => Ok(Self::InitialRoot),
            2 => Ok(Self::Target),
            3 => Ok(Self::InitialAndTarget),
            4 => Ok(Self::Other),
            5 => Ok(Self::Unsupported),
            _ => Err(ProbeProtocolError),
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
    fn from_byte(value: u8) -> Result<Self, ProbeProtocolError> {
        match value {
            0 => Ok(Self::NotObserved),
            1 => Ok(Self::Initial),
            2 => Ok(Self::EffectiveOnly),
            3 => Ok(Self::Other),
            _ => Err(ProbeProtocolError),
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

/// Value-free failure details carried by a credential phase record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CredentialFailureValue {
    step: CredentialStep,
    category: ProbeErrorCategory,
    prefix: CredentialPrefix,
    state: CredentialSelfState,
}

impl CredentialFailureValue {
    /// Constructs one exact failure value.
    #[must_use]
    pub const fn new(
        step: CredentialStep,
        category: ProbeErrorCategory,
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
    pub const fn category(self) -> ProbeErrorCategory {
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

impl PeerPidClass {
    fn from_byte(value: u8) -> Result<Self, ProbeProtocolError> {
        match value {
            0 => Ok(Self::NotObserved),
            1 => Ok(Self::Exact),
            2 => Ok(Self::SocketCreator),
            3 => Ok(Self::Mismatch),
            4 => Ok(Self::Unsupported),
            _ => Err(ProbeProtocolError),
        }
    }
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

impl PeerTokenClass {
    fn from_byte(value: u8) -> Result<Self, ProbeProtocolError> {
        match value {
            0 => Ok(Self::NotObserved),
            1 => Ok(Self::Baseline),
            2 => Ok(Self::Unchanged),
            3 => Ok(Self::Changed),
            4 => Ok(Self::Unsupported),
            _ => Err(ProbeProtocolError),
        }
    }
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
    ) -> Result<Self, ProbeProtocolError> {
        if matches!(stream_eid, CredentialIdentityClass::NotObserved)
            || matches!(stream_cred, CredentialIdentityClass::NotObserved)
            || matches!(stream_pid, PeerPidClass::NotObserved)
            || matches!(datagram_cred, CredentialIdentityClass::NotObserved)
            || matches!(datagram_pid, PeerPidClass::NotObserved)
            || matches!(datagram_token, PeerTokenClass::NotObserved)
        {
            return Err(ProbeProtocolError);
        }
        Ok(Self {
            stream_eid,
            stream_cred,
            stream_pid,
            datagram_cred,
            datagram_pid,
            datagram_token,
        })
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

    fn encode(self) -> [u8; 6] {
        [
            self.stream_eid as u8,
            self.stream_cred as u8,
            self.stream_pid as u8,
            self.datagram_cred as u8,
            self.datagram_pid as u8,
            self.datagram_token as u8,
        ]
    }

    fn decode(bytes: &[u8]) -> Result<Self, ProbeProtocolError> {
        let [
            stream_eid,
            stream_cred,
            stream_pid,
            datagram_cred,
            datagram_pid,
            datagram_token,
        ] = array::<6>(bytes, 0)?;
        let observation = Self {
            stream_eid: CredentialIdentityClass::from_byte(stream_eid)?,
            stream_cred: CredentialIdentityClass::from_byte(stream_cred)?,
            stream_pid: PeerPidClass::from_byte(stream_pid)?,
            datagram_cred: CredentialIdentityClass::from_byte(datagram_cred)?,
            datagram_pid: PeerPidClass::from_byte(datagram_pid)?,
            datagram_token: PeerTokenClass::from_byte(datagram_token)?,
        };
        if observation.is_none()
            || (!matches!(observation.stream_eid, CredentialIdentityClass::NotObserved)
                && !matches!(
                    observation.stream_cred,
                    CredentialIdentityClass::NotObserved
                )
                && !matches!(observation.stream_pid, PeerPidClass::NotObserved)
                && !matches!(
                    observation.datagram_cred,
                    CredentialIdentityClass::NotObserved
                )
                && !matches!(observation.datagram_pid, PeerPidClass::NotObserved)
                && !matches!(observation.datagram_token, PeerTokenClass::NotObserved))
        {
            Ok(observation)
        } else {
            Err(ProbeProtocolError)
        }
    }
}

/// Fixed state carried by one credential phase record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CredentialRecordKind {
    /// Worker completed its transition and first two observations.
    WorkerTransitioned = 1,
    /// Launcher completed its transition and all three local observations.
    LauncherTransitioned = 2,
    /// Worker completed its final after-both observation.
    WorkerFinal = 3,
    /// One role stopped at an exact credential or protocol step.
    Failure = 4,
}

impl CredentialRecordKind {
    fn from_byte(value: u8) -> Result<Self, ProbeProtocolError> {
        match value {
            1 => Ok(Self::WorkerTransitioned),
            2 => Ok(Self::LauncherTransitioned),
            3 => Ok(Self::WorkerFinal),
            4 => Ok(Self::Failure),
            _ => Err(ProbeProtocolError),
        }
    }
}

/// One authenticated, value-free credential phase or failure record.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CredentialRecord {
    mode: ProbeMode,
    kind: CredentialRecordKind,
    role: CredentialRole,
    failure: Option<(CredentialStep, ProbeErrorCategory, CredentialPrefix)>,
    identity: CredentialIdentityClass,
    groups: CredentialGroupClass,
    observations: [PeerObservation; 3],
    nonce: SessionId,
}

impl CredentialRecord {
    /// Constructs a successful worker-transition record.
    pub fn worker_transitioned(
        mode: ProbeMode,
        state: CredentialSelfState,
        initial: PeerObservation,
        after_worker: PeerObservation,
        nonce: SessionId,
    ) -> Result<Self, ProbeProtocolError> {
        Self::success(
            mode,
            CredentialRecordKind::WorkerTransitioned,
            CredentialRole::Worker,
            state,
            [initial, after_worker, PeerObservation::NONE],
            nonce,
        )
    }

    /// Constructs a successful launcher-transition record.
    pub fn launcher_transitioned(
        mode: ProbeMode,
        state: CredentialSelfState,
        observations: [PeerObservation; 3],
        nonce: SessionId,
    ) -> Result<Self, ProbeProtocolError> {
        Self::success(
            mode,
            CredentialRecordKind::LauncherTransitioned,
            CredentialRole::Launcher,
            state,
            observations,
            nonce,
        )
    }

    /// Constructs a successful worker-final record.
    pub fn worker_final(
        mode: ProbeMode,
        state: CredentialSelfState,
        observations: [PeerObservation; 3],
        nonce: SessionId,
    ) -> Result<Self, ProbeProtocolError> {
        Self::success(
            mode,
            CredentialRecordKind::WorkerFinal,
            CredentialRole::Worker,
            state,
            observations,
            nonce,
        )
    }

    /// Constructs a value-free failure record.
    pub fn failure(
        mode: ProbeMode,
        role: CredentialRole,
        failure: CredentialFailureValue,
        initial: PeerObservation,
        nonce: SessionId,
    ) -> Result<Self, ProbeProtocolError> {
        if !mode.is_credential_pair()
            || nonce.is_pre_session()
            || matches!(
                failure.state().identity(),
                CredentialIdentityClass::NotObserved | CredentialIdentityClass::Unsupported
            )
            || matches!(failure.state().groups(), CredentialGroupClass::NotObserved)
            || (failure.prefix() != CredentialPrefix::None && initial.is_none())
        {
            return Err(ProbeProtocolError);
        }
        Ok(Self {
            mode,
            kind: CredentialRecordKind::Failure,
            role,
            failure: Some((failure.step(), failure.category(), failure.prefix())),
            identity: failure.state().identity(),
            groups: failure.state().groups(),
            observations: [initial, PeerObservation::NONE, PeerObservation::NONE],
            nonce,
        })
    }

    fn success(
        mode: ProbeMode,
        kind: CredentialRecordKind,
        role: CredentialRole,
        state: CredentialSelfState,
        observations: [PeerObservation; 3],
        nonce: SessionId,
    ) -> Result<Self, ProbeProtocolError> {
        let expected = if mode.retains_root() {
            (
                CredentialIdentityClass::InitialAndTarget,
                CredentialGroupClass::Initial,
            )
        } else {
            (
                CredentialIdentityClass::Target,
                CredentialGroupClass::EffectiveOnly,
            )
        };
        let observation_shape = match kind {
            CredentialRecordKind::WorkerTransitioned => {
                !observations[0].is_none()
                    && !observations[1].is_none()
                    && observations[2].is_none()
                    && role == CredentialRole::Worker
            }
            CredentialRecordKind::LauncherTransitioned => {
                observations
                    .iter()
                    .all(|observation| !observation.is_none())
                    && role == CredentialRole::Launcher
            }
            CredentialRecordKind::WorkerFinal => {
                observations
                    .iter()
                    .all(|observation| !observation.is_none())
                    && role == CredentialRole::Worker
            }
            CredentialRecordKind::Failure => false,
        };
        if !mode.is_credential_pair()
            || nonce.is_pre_session()
            || (state.identity(), state.groups()) != expected
            || !observation_shape
        {
            return Err(ProbeProtocolError);
        }
        Ok(Self {
            mode,
            kind,
            role,
            failure: None,
            identity: state.identity(),
            groups: state.groups(),
            observations,
            nonce,
        })
    }

    /// Returns the selected probe mode.
    #[must_use]
    pub const fn mode(self) -> ProbeMode {
        self.mode
    }

    /// Returns the exact phase kind.
    #[must_use]
    pub const fn kind(self) -> CredentialRecordKind {
        self.kind
    }

    /// Returns the sender or failing process role.
    #[must_use]
    pub const fn role(self) -> CredentialRole {
        self.role
    }

    /// Returns the fixed failure triple, if this is a failure record.
    #[must_use]
    pub const fn failure_value(
        self,
    ) -> Option<(CredentialStep, ProbeErrorCategory, CredentialPrefix)> {
        self.failure
    }

    /// Returns the sender's final identity class.
    #[must_use]
    pub const fn identity(self) -> CredentialIdentityClass {
        self.identity
    }

    /// Returns the sender's final supplementary-group class.
    #[must_use]
    pub const fn groups(self) -> CredentialGroupClass {
        self.groups
    }

    /// Returns the phase-ordered peer observations.
    #[must_use]
    pub const fn observations(self) -> [PeerObservation; 3] {
        self.observations
    }

    /// Returns the command nonce.
    #[must_use]
    pub const fn nonce(self) -> SessionId {
        self.nonce
    }

    /// Returns whether this record belongs to the exact exchange and sender.
    #[must_use]
    pub fn matches_exchange(self, mode: ProbeMode, role: CredentialRole, nonce: SessionId) -> bool {
        self.mode == mode && self.role == role && self.nonce == nonce
    }

    /// Returns whether this record is the exact next successful phase.
    #[must_use]
    pub fn matches_expected(
        self,
        mode: ProbeMode,
        kind: CredentialRecordKind,
        role: CredentialRole,
        nonce: SessionId,
    ) -> bool {
        self.matches_exchange(mode, role, nonce) && self.kind == kind
    }

    /// Encodes the exact credential phase record.
    #[must_use]
    pub fn encode(self) -> [u8; CREDENTIAL_RECORD_BYTES] {
        let mut bytes = [0_u8; CREDENTIAL_RECORD_BYTES];
        bytes[0..4].copy_from_slice(&CREDENTIAL_MAGIC);
        bytes[4..6].copy_from_slice(&CREDENTIAL_VERSION.to_be_bytes());
        bytes[6] = self.mode as u8;
        bytes[7] = self.kind as u8;
        bytes[8] = self.role as u8;
        bytes[9] = u8::from(self.failure.is_none());
        if let Some((step, category, prefix)) = self.failure {
            bytes[10] = step as u8;
            bytes[11] = category as u8;
            bytes[12] = prefix as u8;
        }
        bytes[13] = self.identity as u8;
        bytes[14] = self.groups as u8;
        for (index, observation) in self.observations.into_iter().enumerate() {
            let start = 16 + index * 6;
            if let Some(slot) = bytes.get_mut(start..start + 6) {
                slot.copy_from_slice(&observation.encode());
            }
        }
        bytes[48..80].copy_from_slice(self.nonce.as_bytes());
        bytes
    }

    /// Decodes and validates one exact credential phase record.
    pub fn decode(bytes: &[u8; CREDENTIAL_RECORD_BYTES]) -> Result<Self, ProbeProtocolError> {
        if bytes[0..4] != CREDENTIAL_MAGIC
            || bytes[4..6] != CREDENTIAL_VERSION.to_be_bytes()
            || bytes[15] != 0
            || bytes[34..48] != [0; 14]
        {
            return Err(ProbeProtocolError);
        }
        let mode = ProbeMode::from_byte(bytes[6])?;
        let kind = CredentialRecordKind::from_byte(bytes[7])?;
        let role = CredentialRole::from_byte(bytes[8])?;
        let identity = CredentialIdentityClass::from_byte(bytes[13])?;
        let groups = CredentialGroupClass::from_byte(bytes[14])?;
        let observations = [
            PeerObservation::decode(&bytes[16..22])?,
            PeerObservation::decode(&bytes[22..28])?,
            PeerObservation::decode(&bytes[28..34])?,
        ];
        let nonce = SessionId::from_bytes(array(bytes, 48)?);
        let record = match (kind, bytes[9], bytes[10], bytes[11], bytes[12]) {
            (CredentialRecordKind::Failure, 0, step, category, prefix) => Self::failure(
                mode,
                role,
                CredentialFailureValue::new(
                    CredentialStep::from_byte(step)?,
                    ProbeErrorCategory::from_byte(category)?,
                    CredentialPrefix::from_byte(prefix)?,
                    CredentialSelfState::new(identity, groups),
                ),
                observations[0],
                nonce,
            ),
            (CredentialRecordKind::WorkerTransitioned, 1, 0, 0, 0) => Self::worker_transitioned(
                mode,
                CredentialSelfState::new(identity, groups),
                observations[0],
                observations[1],
                nonce,
            ),
            (CredentialRecordKind::LauncherTransitioned, 1, 0, 0, 0) => {
                Self::launcher_transitioned(
                    mode,
                    CredentialSelfState::new(identity, groups),
                    observations,
                    nonce,
                )
            }
            (CredentialRecordKind::WorkerFinal, 1, 0, 0, 0) => Self::worker_final(
                mode,
                CredentialSelfState::new(identity, groups),
                observations,
                nonce,
            ),
            _ => Err(ProbeProtocolError),
        }?;
        if record.encode() == *bytes {
            Ok(record)
        } else {
            Err(ProbeProtocolError)
        }
    }
}

impl fmt::Debug for CredentialRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CredentialRecord(<redacted>)")
    }
}

/// One nonce-bound exact-root bootstrap command.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ProbeBootstrap {
    mode: ProbeMode,
    fault: RuntimeFault,
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
        Self::new_with_fault(
            mode,
            RuntimeFault::None,
            target_uid,
            target_gid,
            root,
            nonce,
        )
    }

    /// Constructs a complete worker command with a new-mode-only deterministic fault.
    pub fn new_with_fault(
        mode: ProbeMode,
        fault: RuntimeFault,
        target_uid: u32,
        target_gid: u32,
        root: ObjectIdentity,
        nonce: SessionId,
    ) -> Result<Self, ProbeProtocolError> {
        if matches!(mode, ProbeMode::Control | ProbeMode::CredentialControl)
            || !mode.accepts_target(target_uid, target_gid)
            || (!mode.continues_runtime() && fault != RuntimeFault::None)
            || root.device == 0
            || root.inode == 0
            || nonce.is_pre_session()
        {
            return Err(ProbeProtocolError);
        }
        Ok(Self {
            mode,
            fault,
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

    /// Returns the feature-only deterministic fault boundary.
    #[must_use]
    pub const fn fault(self) -> RuntimeFault {
        self.fault
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

    /// Encodes the fixed v2 record.
    #[must_use]
    pub fn encode(self) -> [u8; BOOTSTRAP_RECORD_BYTES] {
        let mut bytes = [0_u8; BOOTSTRAP_RECORD_BYTES];
        bytes[0..4].copy_from_slice(&BOOTSTRAP_MAGIC);
        bytes[4..6].copy_from_slice(&VERSION.to_be_bytes());
        bytes[6] = self.mode as u8;
        bytes[7] = self.fault as u8;
        bytes[8..12].copy_from_slice(&self.target_uid.to_be_bytes());
        bytes[12..16].copy_from_slice(&self.target_gid.to_be_bytes());
        bytes[16..24].copy_from_slice(&self.root.device.to_be_bytes());
        bytes[24..32].copy_from_slice(&self.root.inode.to_be_bytes());
        bytes[32..64].copy_from_slice(self.nonce.as_bytes());
        bytes
    }

    /// Decodes one exact fixed-size record.
    pub fn decode(bytes: &[u8; BOOTSTRAP_RECORD_BYTES]) -> Result<Self, ProbeProtocolError> {
        if bytes[0..4] != BOOTSTRAP_MAGIC || bytes[4..6] != VERSION.to_be_bytes() {
            return Err(ProbeProtocolError);
        }
        let mode = ProbeMode::from_byte(bytes[6])?;
        let fault = RuntimeFault::from_byte(bytes[7])?;
        let target_uid = u32::from_be_bytes(array(bytes, 8)?);
        let target_gid = u32::from_be_bytes(array(bytes, 12)?);
        let root = ObjectIdentity {
            device: u64::from_be_bytes(array(bytes, 16)?),
            inode: u64::from_be_bytes(array(bytes, 24)?),
        };
        let nonce = SessionId::from_bytes(array(bytes, 32)?);
        Self::new_with_fault(mode, fault, target_uid, target_gid, root, nonce)
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
        if matches!(mode, ProbeMode::Control | ProbeMode::CredentialControl)
            || nonce.is_pre_session()
        {
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
        if matches!(mode, ProbeMode::Control | ProbeMode::CredentialControl)
            || nonce.is_pre_session()
        {
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

    /// Encodes the fixed v2 terminal record.
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
        if matches!(mode, ProbeMode::Control | ProbeMode::CredentialControl) {
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

        let runtime = ProbeBootstrap::new_with_fault(
            ProbeMode::RuntimeDrop,
            RuntimeFault::PostAck,
            501,
            20,
            ObjectIdentity {
                device: 1,
                inode: 1,
            },
            SessionId::from_bytes([2; 32]),
        )
        .expect("runtime faults are explicit in new modes");
        assert_eq!(runtime.fault(), RuntimeFault::PostAck);
        assert_eq!(ProbeBootstrap::decode(&runtime.encode()), Ok(runtime));
        assert!(
            ProbeBootstrap::new_with_fault(
                ProbeMode::CredentialDrop,
                RuntimeFault::PostAck,
                501,
                20,
                ObjectIdentity {
                    device: 1,
                    inode: 1,
                },
                SessionId::from_bytes([2; 32]),
            )
            .is_err(),
            "historical mode bytes must retain a zero reserved byte"
        );
        let mut version_one = encoded;
        version_one[4..6].copy_from_slice(&1_u16.to_be_bytes());
        assert_eq!(
            ProbeBootstrap::decode(&version_one),
            Err(ProbeProtocolError)
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
        for index in [0, 4, 10, 15] {
            let mut malformed = ProbeResult::success(ProbeMode::Drop, nonce)
                .expect("valid success")
                .encode();
            malformed[index] ^= 0xff;
            assert_eq!(ProbeResult::decode(&malformed), Err(ProbeProtocolError));
        }
        let mut version_one = ProbeResult::success(ProbeMode::Drop, nonce)
            .expect("valid success")
            .encode();
        version_one[4..6].copy_from_slice(&1_u16.to_be_bytes());
        assert_eq!(ProbeResult::decode(&version_one), Err(ProbeProtocolError));
        let mut unknown_mode = ProbeResult::success(ProbeMode::Drop, nonce)
            .expect("valid success")
            .encode();
        unknown_mode[6] = u8::MAX;
        assert_eq!(ProbeResult::decode(&unknown_mode), Err(ProbeProtocolError));
        let mut unknown_failure = ProbeResult::failure(
            ProbeMode::Drop,
            nonce,
            ProbeStage::Chroot,
            ProbeErrorCategory::Other,
        )
        .expect("valid failure")
        .encode();
        unknown_failure[8] = u8::MAX;
        assert_eq!(
            ProbeResult::decode(&unknown_failure),
            Err(ProbeProtocolError)
        );
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
        assert_eq!(
            ProbeMode::parse("hvf-control", 0, 0),
            Some(ProbeMode::HvfControl)
        );
        assert_eq!(
            ProbeMode::parse("inherited-root", 0, 0),
            Some(ProbeMode::InheritedRoot)
        );
        assert_eq!(ProbeMode::parse("hvf-control", 501, 20), None);
        assert_eq!(ProbeMode::parse("inherited-root", 501, 20), None);
        assert_eq!(
            ProbeMode::parse("credential-drop", 501, 20),
            Some(ProbeMode::CredentialDrop)
        );
        assert_eq!(ProbeMode::parse("credential-drop", 0, 0), None);
        assert_eq!(
            ProbeMode::parse("credential-retain-root", 0, 0),
            Some(ProbeMode::CredentialRetainRoot)
        );
        assert_eq!(
            ProbeMode::parse("credential-unmapped", 2_147_483_647, 2_147_483_647),
            Some(ProbeMode::CredentialUnmapped)
        );
        assert_eq!(
            ProbeMode::parse("credential-unmapped", u32::MAX, u32::MAX),
            None
        );
        assert_eq!(
            ProbeMode::parse("credential-control", 501, 20),
            Some(ProbeMode::CredentialControl)
        );
        assert_eq!(
            ProbeMode::parse("credential-control", 0, 0),
            Some(ProbeMode::CredentialControl)
        );
        assert_eq!(ProbeMode::parse("credential-control", 501, 0), None);
        assert_eq!(
            ProbeMode::parse("runtime-drop", 501, 20),
            Some(ProbeMode::RuntimeDrop)
        );
        assert_eq!(ProbeMode::parse("runtime-drop", 0, 0), None);
        assert_eq!(
            ProbeMode::parse("runtime-retain-root", 0, 0),
            Some(ProbeMode::RuntimeRetainRoot)
        );
        assert_eq!(
            ProbeMode::parse("runtime-unmapped", 2_147_483_647, 2_147_483_647),
            Some(ProbeMode::RuntimeUnmapped)
        );
        for (name, mode, uid, gid, credential, workload) in [
            (
                "guest-no-api-drop",
                ProbeMode::GuestNoApiDrop,
                501,
                20,
                CredentialClass::Mapped,
                RuntimeWorkload::GuestNoApi,
            ),
            (
                "guest-no-api-retain-root",
                ProbeMode::GuestNoApiRetainRoot,
                0,
                0,
                CredentialClass::RetainRoot,
                RuntimeWorkload::GuestNoApi,
            ),
            (
                "guest-no-api-unmapped",
                ProbeMode::GuestNoApiUnmapped,
                2_147_483_647,
                2_147_483_647,
                CredentialClass::MaximumUnmapped,
                RuntimeWorkload::GuestNoApi,
            ),
            (
                "guest-api-drop",
                ProbeMode::GuestApiDrop,
                501,
                20,
                CredentialClass::Mapped,
                RuntimeWorkload::GuestApi,
            ),
            (
                "guest-api-retain-root",
                ProbeMode::GuestApiRetainRoot,
                0,
                0,
                CredentialClass::RetainRoot,
                RuntimeWorkload::GuestApi,
            ),
            (
                "guest-api-unmapped",
                ProbeMode::GuestApiUnmapped,
                2_147_483_647,
                2_147_483_647,
                CredentialClass::MaximumUnmapped,
                RuntimeWorkload::GuestApi,
            ),
        ] {
            assert_eq!(ProbeMode::parse(name, uid, gid), Some(mode));
            assert_eq!(mode.name(), name);
            assert_eq!(mode.credential_class(), Some(credential));
            assert_eq!(mode.runtime_workload(), Some(workload));
            assert!(workload.is_guest());
            assert!(mode.is_credential_pair());
            assert!(mode.continues_runtime());
            assert_eq!(
                mode.retains_root(),
                credential == CredentialClass::RetainRoot
            );
        }
        assert_eq!(
            ProbeMode::RuntimeDrop.runtime_workload(),
            Some(RuntimeWorkload::RepresentativeGrants)
        );
        assert!(!RuntimeWorkload::RepresentativeGrants.is_guest());
        for workload in [
            RuntimeWorkload::RepresentativeGrants,
            RuntimeWorkload::GuestNoApi,
            RuntimeWorkload::GuestApi,
        ] {
            assert!(workload.supports_retired_record_free_namespace());
        }
        assert_eq!(ProbeMode::Drop.credential_class(), None);
        assert_eq!(ProbeMode::CredentialDrop.runtime_workload(), None);
        assert_eq!(ProbeMode::parse("unknown", 501, 20), None);
    }

    #[test]
    fn every_stage_round_trips_through_a_failure_result() {
        let nonce = SessionId::from_bytes([0x5a; 32]);
        for stage in [
            ProbeStage::InitialIdentity,
            ProbeStage::TakeRoot,
            ProbeStage::ValidateRoot,
            ProbeStage::EnterRoot,
            ProbeStage::Chroot,
            ProbeStage::ChangeDirectory,
            ProbeStage::UnexpectedContinuation,
            ProbeStage::ValidateStagedBundle,
            ProbeStage::ValidateStagedLoader,
            ProbeStage::SpawnWorker,
            ProbeStage::SuspendedIdentity,
            ProbeStage::InheritedRoot,
            ProbeStage::SandboxChrootControl,
            ProbeStage::LiveIdentity,
            ProbeStage::HvfCreate,
            ProbeStage::HvfDestroy,
            ProbeStage::WorkerBootstrap,
            ProbeStage::ContinuationAck,
            ProbeStage::LifecycleHello,
            ProbeStage::RuntimeNamespace,
            ProbeStage::GrantTransfer,
            ProbeStage::GrantAccepted,
            ProbeStage::LifecycleProceed,
            ProbeStage::LifecycleTerminal,
            ProbeStage::RuntimeCleanup,
            ProbeStage::RuntimeSessionCreate,
            ProbeStage::RuntimeSessionOpen,
            ProbeStage::RuntimeAuthoritySend,
            ProbeStage::RuntimeAuthorityReceive,
            ProbeStage::RuntimeAuthorityValidate,
            ProbeStage::RuntimeSessionLock,
            ProbeStage::RuntimeSessionEnter,
            ProbeStage::LifecyclePrepared,
            ProbeStage::GuestGrantContract,
            ProbeStage::GuestResourceWitness,
            ProbeStage::ApiSocketPublication,
            ProbeStage::ApiLoggerConfiguration,
            ProbeStage::ApiMetricsConfiguration,
            ProbeStage::ApiSerialConfiguration,
            ProbeStage::ApiMachineConfiguration,
            ProbeStage::ApiBootConfiguration,
            ProbeStage::ApiDriveConfiguration,
            ProbeStage::ApiInstanceStart,
            ProbeStage::NoApiStartup,
            ProbeStage::GuestHvfWitness,
            ProbeStage::GuestHvfCreate,
            ProbeStage::GuestExecution,
            ProbeStage::GuestOracle,
            ProbeStage::GuestPoweroff,
            ProbeStage::GuestTimeout,
            ProbeStage::GuestEndpointDeath,
            ProbeStage::GuestTerminalEvidence,
            ProbeStage::GuestCleanup,
            ProbeStage::ApiListenerRequest,
            ProbeStage::ApiListenerBind,
            ProbeStage::ApiListenerTransfer,
            ProbeStage::ApiListenerAdoption,
            ProbeStage::RuntimeNamespaceRetirement,
        ] {
            let result = ProbeResult::failure(
                ProbeMode::InheritedRoot,
                nonce,
                stage,
                ProbeErrorCategory::Other,
            )
            .expect("stage should produce a valid failure result");
            assert_eq!(ProbeResult::decode(&result.encode()), Ok(result));
        }
    }

    #[test]
    fn every_worker_mode_and_error_category_round_trips() {
        let nonce = SessionId::from_bytes([0x6b; 32]);
        for (mode, uid, gid) in [
            (ProbeMode::Drop, 501, 20),
            (ProbeMode::RetainRoot, 0, 0),
            (ProbeMode::UnmappedSyscall, u32::MAX, u32::MAX),
            (ProbeMode::HvfControl, 0, 0),
            (ProbeMode::InheritedRoot, 0, 0),
            (ProbeMode::CredentialDrop, 501, 20),
            (ProbeMode::CredentialRetainRoot, 0, 0),
            (ProbeMode::CredentialUnmapped, 2_147_483_647, 2_147_483_647),
            (ProbeMode::RuntimeDrop, 501, 20),
            (ProbeMode::RuntimeRetainRoot, 0, 0),
            (ProbeMode::RuntimeUnmapped, 2_147_483_647, 2_147_483_647),
            (ProbeMode::GuestNoApiDrop, 501, 20),
            (ProbeMode::GuestNoApiRetainRoot, 0, 0),
            (ProbeMode::GuestNoApiUnmapped, 2_147_483_647, 2_147_483_647),
            (ProbeMode::GuestApiDrop, 501, 20),
            (ProbeMode::GuestApiRetainRoot, 0, 0),
            (ProbeMode::GuestApiUnmapped, 2_147_483_647, 2_147_483_647),
        ] {
            let bootstrap = ProbeBootstrap::new(
                mode,
                uid,
                gid,
                ObjectIdentity {
                    device: 0x12,
                    inode: 0x34,
                },
                nonce,
            )
            .expect("mode and target should construct");
            assert_eq!(ProbeBootstrap::decode(&bootstrap.encode()), Ok(bootstrap));
            for category in [
                ProbeErrorCategory::PermissionDenied,
                ProbeErrorCategory::InvalidInput,
                ProbeErrorCategory::Other,
            ] {
                let result =
                    ProbeResult::failure(mode, nonce, ProbeStage::WorkerBootstrap, category)
                        .expect("mode and category should construct");
                assert_eq!(ProbeResult::decode(&result.encode()), Ok(result));
            }
        }
    }

    #[test]
    fn runtime_fault_and_result_vocabularies_are_closed_and_exhaustive() {
        for (fault, byte, name) in [
            (RuntimeFault::None, 0, "none"),
            (RuntimeFault::PreAck, 1, "pre-ack"),
            (RuntimeFault::PostAck, 2, "post-ack"),
            (RuntimeFault::Namespace, 3, "namespace"),
            (RuntimeFault::GrantTransfer, 4, "grant-transfer"),
            (RuntimeFault::Proceed, 5, "proceed"),
            (RuntimeFault::Terminal, 6, "terminal"),
            (RuntimeFault::SessionCreate, 7, "session-create"),
            (RuntimeFault::SessionOpen, 8, "session-open"),
            (RuntimeFault::AuthoritySend, 9, "authority-send"),
            (RuntimeFault::AuthorityReceive, 10, "authority-receive"),
            (RuntimeFault::AuthorityValidate, 11, "authority-validate"),
            (RuntimeFault::SessionLock, 12, "session-lock"),
            (RuntimeFault::SessionEnter, 13, "session-enter"),
            (RuntimeFault::Prepared, 14, "prepared"),
            (RuntimeFault::GuestGrantContract, 15, "guest-grant-contract"),
            (
                RuntimeFault::GuestResourceWitness,
                16,
                "guest-resource-witness",
            ),
            (
                RuntimeFault::ApiSocketPublication,
                17,
                "api-socket-publication",
            ),
            (
                RuntimeFault::ApiLoggerConfiguration,
                18,
                "api-logger-configuration",
            ),
            (
                RuntimeFault::ApiMetricsConfiguration,
                19,
                "api-metrics-configuration",
            ),
            (
                RuntimeFault::ApiSerialConfiguration,
                20,
                "api-serial-configuration",
            ),
            (
                RuntimeFault::ApiMachineConfiguration,
                21,
                "api-machine-configuration",
            ),
            (
                RuntimeFault::ApiBootConfiguration,
                22,
                "api-boot-configuration",
            ),
            (
                RuntimeFault::ApiDriveConfiguration,
                23,
                "api-drive-configuration",
            ),
            (RuntimeFault::ApiInstanceStart, 24, "api-instance-start"),
            (RuntimeFault::NoApiStartup, 25, "no-api-startup"),
            (RuntimeFault::GuestHvfWitness, 26, "guest-hvf-witness"),
            (RuntimeFault::GuestHvfCreate, 27, "guest-hvf-create"),
            (RuntimeFault::GuestExecution, 28, "guest-execution"),
            (RuntimeFault::GuestOracle, 29, "guest-oracle"),
            (RuntimeFault::GuestPoweroff, 30, "guest-poweroff"),
            (RuntimeFault::GuestTimeout, 31, "guest-timeout"),
            (RuntimeFault::GuestEndpointDeath, 32, "guest-endpoint-death"),
            (
                RuntimeFault::GuestTerminalEvidence,
                33,
                "guest-terminal-evidence",
            ),
            (RuntimeFault::GuestCleanup, 34, "guest-cleanup"),
            (RuntimeFault::GuestGrantAccepted, 35, "guest-grant-accepted"),
            (
                RuntimeFault::GuestTransportContamination,
                36,
                "guest-transport-contamination",
            ),
            (RuntimeFault::ApiListenerRequest, 37, "api-listener-request"),
            (RuntimeFault::ApiListenerBind, 38, "api-listener-bind"),
            (
                RuntimeFault::ApiListenerTransfer,
                39,
                "api-listener-transfer",
            ),
            (
                RuntimeFault::ApiListenerAdoption,
                40,
                "api-listener-adoption",
            ),
            (
                RuntimeFault::ApiListenerEndpointDeath,
                41,
                "api-listener-endpoint-death",
            ),
            (
                RuntimeFault::NamespaceRetireBeforeUnlink,
                42,
                "namespace-retire-before-unlink",
            ),
            (
                RuntimeFault::NamespaceRetireAfterUnlink,
                43,
                "namespace-retire-after-unlink",
            ),
            (
                RuntimeFault::NamespaceRetireObserve,
                44,
                "namespace-retire-observe",
            ),
            (
                RuntimeFault::NamespaceRecordWrite,
                45,
                "namespace-record-write",
            ),
        ] {
            assert_eq!(RuntimeFault::parse(name), Some(fault));
            assert_eq!(RuntimeFault::from_byte(byte), Ok(fault));
            assert_eq!(fault.name(), name);
        }
        assert_eq!(RuntimeFault::parse("unknown"), None);
        assert_eq!(RuntimeFault::from_byte(46), Err(ProbeProtocolError));

        for (result, name) in [
            (RuntimeResultClass::Complete, "complete"),
            (
                RuntimeResultClass::ContinuationBoundary,
                "continuation-boundary",
            ),
            (RuntimeResultClass::IdentityBoundary, "identity-boundary"),
            (
                RuntimeResultClass::ExplicitRootBoundary,
                "explicit-root-boundary",
            ),
            (RuntimeResultClass::NamespaceBoundary, "namespace-boundary"),
            (RuntimeResultClass::GrantBoundary, "grant-boundary"),
            (RuntimeResultClass::LifecycleBoundary, "lifecycle-boundary"),
            (RuntimeResultClass::ApiBoundary, "api-boundary"),
            (RuntimeResultClass::HvfBoundary, "hvf-boundary"),
            (RuntimeResultClass::GuestBoundary, "guest-boundary"),
        ] {
            assert_eq!(result.name(), name);
        }
    }

    #[test]
    fn runtime_session_authority_is_canonical_bound_and_redacted() {
        let bootstrap = ProbeBootstrap::new(
            ProbeMode::RuntimeDrop,
            501,
            20,
            ObjectIdentity {
                device: 101,
                inode: 103,
            },
            SessionId::from_bytes([0x81; 32]),
        )
        .expect("runtime bootstrap should construct");
        let session = SessionId::from_bytes([0x82; 32]);
        let identity = ObjectIdentity {
            device: 107,
            inode: 109,
        };
        let authority = RuntimeSessionAuthority::launcher(
            bootstrap.mode(),
            bootstrap.target_uid(),
            bootstrap.target_gid(),
            bootstrap.root(),
            identity,
            bootstrap.nonce(),
            session,
        )
        .expect("authority should construct");
        let encoded = authority.encode();
        assert_eq!(encoded.len(), RUNTIME_SESSION_AUTHORITY_BYTES);
        assert_eq!(RuntimeSessionAuthority::decode(&encoded), Ok(authority));
        assert!(authority.matches_expected(bootstrap, session, identity));
        assert_eq!(authority.mode(), bootstrap.mode());
        assert_eq!(authority.role(), CredentialRole::Launcher);
        assert_eq!(authority.target_uid(), bootstrap.target_uid());
        assert_eq!(authority.target_gid(), bootstrap.target_gid());
        assert_eq!(authority.root(), bootstrap.root());
        assert_eq!(authority.session_identity(), identity);
        assert_eq!(authority.nonce(), bootstrap.nonce());
        assert_eq!(authority.session(), session);
        assert_eq!(
            format!("{authority:?}"),
            "RuntimeSessionAuthority(<redacted>)"
        );

        for index in [0, 4, 6, 7, 8, 9] {
            let mut malformed = encoded;
            malformed[index] ^= 0xff;
            assert_eq!(
                RuntimeSessionAuthority::decode(&malformed),
                Err(ProbeProtocolError),
                "byte {index} must be canonical"
            );
        }
        for index in 9..16 {
            let mut malformed = encoded;
            malformed[index] = 1;
            assert_eq!(
                RuntimeSessionAuthority::decode(&malformed),
                Err(ProbeProtocolError)
            );
        }
        for range in [
            16..20,
            20..24,
            24..32,
            32..40,
            40..48,
            48..56,
            56..88,
            88..120,
        ] {
            let mut malformed = encoded;
            malformed[range].fill(0);
            assert_eq!(
                RuntimeSessionAuthority::decode(&malformed),
                Err(ProbeProtocolError)
            );
        }
        for index in 0..encoded.len() {
            let mut hostile = encoded;
            hostile[index] ^= 1;
            if let Ok(decoded) = RuntimeSessionAuthority::decode(&hostile) {
                assert!(
                    !decoded.matches_expected(bootstrap, session, identity),
                    "mutated byte {index} must not retain the original authority"
                );
            }
        }

        let other_session = SessionId::from_bytes([0x83; 32]);
        let other_identity = ObjectIdentity {
            device: 107,
            inode: 113,
        };
        assert!(!authority.matches_expected(bootstrap, other_session, identity));
        assert!(!authority.matches_expected(bootstrap, session, other_identity));
        let other_bootstrap = ProbeBootstrap::new(
            ProbeMode::RuntimeDrop,
            502,
            20,
            bootstrap.root(),
            bootstrap.nonce(),
        )
        .expect("other bootstrap should construct");
        assert!(!authority.matches_expected(other_bootstrap, session, identity));
        let other_root = ProbeBootstrap::new(
            ProbeMode::RuntimeDrop,
            501,
            20,
            ObjectIdentity {
                device: 101,
                inode: 127,
            },
            bootstrap.nonce(),
        )
        .expect("other-root bootstrap should construct");
        assert!(!authority.matches_expected(other_root, session, identity));
        let other_nonce = ProbeBootstrap::new(
            ProbeMode::RuntimeDrop,
            501,
            20,
            bootstrap.root(),
            SessionId::from_bytes([0x84; 32]),
        )
        .expect("other-nonce bootstrap should construct");
        assert!(!authority.matches_expected(other_nonce, session, identity));
        let other_mode = ProbeBootstrap::new(
            ProbeMode::RuntimeUnmapped,
            2_147_483_647,
            2_147_483_647,
            bootstrap.root(),
            bootstrap.nonce(),
        )
        .expect("other-mode bootstrap should construct");
        assert!(!authority.matches_expected(other_mode, session, identity));
        assert!(
            RuntimeSessionAuthority::launcher(
                ProbeMode::CredentialDrop,
                501,
                20,
                bootstrap.root(),
                identity,
                bootstrap.nonce(),
                session,
            )
            .is_err()
        );
        assert!(
            RuntimeSessionAuthority::launcher(
                ProbeMode::RuntimeDrop,
                501,
                20,
                ObjectIdentity {
                    device: 0,
                    inode: 1,
                },
                identity,
                bootstrap.nonce(),
                session,
            )
            .is_err()
        );
        assert!(
            RuntimeSessionAuthority::launcher(
                ProbeMode::RuntimeDrop,
                501,
                20,
                bootstrap.root(),
                identity,
                bootstrap.nonce(),
                SessionId::pre_session(),
            )
            .is_err()
        );
    }

    #[test]
    fn api_listener_records_are_canonical_closed_bound_and_redacted() {
        let nonce = SessionId::from_bytes([0xa1; 32]);
        let session = SessionId::from_bytes([0xa2; 32]);
        let identity = ObjectIdentity {
            device: 0x1234,
            inode: 0x5678,
        };
        for mode in [
            ProbeMode::GuestApiDrop,
            ProbeMode::GuestApiRetainRoot,
            ProbeMode::GuestApiUnmapped,
        ] {
            let records = [
                ApiListenerRecord::worker_request(mode, nonce, session)
                    .expect("request should construct"),
                ApiListenerRecord::launcher_ack(mode, nonce, session, identity)
                    .expect("acknowledgment should construct"),
            ];
            for record in records {
                let encoded = record.encode();
                assert_eq!(encoded.len(), API_LISTENER_RECORD_BYTES);
                assert_eq!(ApiListenerRecord::decode(&encoded), Ok(record));
                assert_eq!(record.mode(), mode);
                assert_eq!(record.child(), GUEST_API_SOCKET_CHILD);
                assert_eq!(record.nonce(), nonce);
                assert_eq!(record.session(), session);
                assert!(record.matches_expected(
                    mode,
                    record.kind(),
                    nonce,
                    session,
                    record.path_identity(),
                ));
                assert_eq!(
                    record.descriptor_count(),
                    u8::from(record.kind() == ApiListenerKind::Ack)
                );
                assert_eq!(
                    record.role(),
                    match record.kind() {
                        ApiListenerKind::Request => CredentialRole::Worker,
                        ApiListenerKind::Ack => CredentialRole::Launcher,
                    }
                );
                assert_eq!(format!("{record:?}"), "ApiListenerRecord(<redacted>)");
                for index in 0..encoded.len() {
                    let mut malformed = encoded;
                    malformed[index] ^= 1;
                    if let Ok(decoded) = ApiListenerRecord::decode(&malformed) {
                        assert_ne!(decoded, record);
                        assert!(!decoded.matches_expected(
                            record.mode(),
                            record.kind(),
                            record.nonce(),
                            record.session(),
                            record.path_identity(),
                        ));
                    }
                }
            }
        }

        let request = ApiListenerRecord::worker_request(ProbeMode::GuestApiDrop, nonce, session)
            .expect("request should construct");
        assert_eq!(request.path_identity(), None);
        assert!(!request.matches_expected(
            ProbeMode::GuestApiRetainRoot,
            ApiListenerKind::Request,
            nonce,
            session,
            None,
        ));
        assert!(!request.matches_expected(
            ProbeMode::GuestApiDrop,
            ApiListenerKind::Ack,
            nonce,
            session,
            Some(identity),
        ));
        assert!(
            ApiListenerRecord::worker_request(ProbeMode::GuestNoApiDrop, nonce, session).is_err()
        );
        assert!(
            ApiListenerRecord::worker_request(
                ProbeMode::GuestApiDrop,
                SessionId::pre_session(),
                session,
            )
            .is_err()
        );
        assert!(
            ApiListenerRecord::launcher_ack(
                ProbeMode::GuestApiDrop,
                nonce,
                session,
                ObjectIdentity {
                    device: 0,
                    inode: 0,
                },
            )
            .is_err()
        );
        assert!(
            ApiListenerRecord::new(
                ProbeMode::GuestApiDrop,
                ApiListenerKind::Request,
                CredentialRole::Launcher,
                0,
                nonce,
                session,
                None,
            )
            .is_err()
        );
        assert!(
            ApiListenerRecord::new(
                ProbeMode::GuestApiDrop,
                ApiListenerKind::Ack,
                CredentialRole::Launcher,
                0,
                nonce,
                session,
                Some(identity),
            )
            .is_err()
        );
    }

    #[test]
    fn guest_evidence_records_are_canonical_ordered_bound_and_redacted() {
        let nonce = SessionId::from_bytes([0x91; 32]);
        let session = SessionId::from_bytes([0x92; 32]);
        for mode in [ProbeMode::GuestNoApiDrop, ProbeMode::GuestApiRetainRoot] {
            let records = [
                GuestEvidenceRecord::worker_request(
                    mode,
                    GuestEvidencePhase::ResourceClaim,
                    nonce,
                    session,
                )
                .expect("resource request should construct"),
                GuestEvidenceRecord::launcher_ack(
                    mode,
                    GuestEvidencePhase::ResourceClaim,
                    nonce,
                    session,
                )
                .expect("resource acknowledgment should construct"),
                GuestEvidenceRecord::worker_request(
                    mode,
                    GuestEvidencePhase::HvfCreate,
                    nonce,
                    session,
                )
                .expect("HVF request should construct"),
                GuestEvidenceRecord::launcher_ack(
                    mode,
                    GuestEvidencePhase::HvfCreate,
                    nonce,
                    session,
                )
                .expect("HVF acknowledgment should construct"),
                GuestEvidenceRecord::worker_report(
                    mode,
                    GuestEvidencePhase::HvfCreated,
                    nonce,
                    session,
                )
                .expect("HVF report should construct"),
                GuestEvidenceRecord::worker_report(
                    mode,
                    GuestEvidencePhase::GuestShutdown,
                    nonce,
                    session,
                )
                .expect("shutdown report should construct"),
            ];
            for record in records {
                let encoded = record.encode();
                assert_eq!(encoded.len(), GUEST_EVIDENCE_RECORD_BYTES);
                assert_eq!(GuestEvidenceRecord::decode(&encoded), Ok(record));
                assert_eq!(record.mode(), mode);
                assert_eq!(record.sequence(), record.phase() as u32);
                assert_eq!(record.nonce(), nonce);
                assert_eq!(record.session(), session);
                assert!(record.matches_expected(
                    mode,
                    record.phase(),
                    record.kind(),
                    record.role(),
                    nonce,
                    session,
                ));
                assert_eq!(format!("{record:?}"), "GuestEvidenceRecord(<redacted>)");
                for index in 0..encoded.len() {
                    let mut malformed = encoded;
                    malformed[index] ^= 1;
                    if let Ok(decoded) = GuestEvidenceRecord::decode(&malformed) {
                        assert_ne!(decoded, record);
                        assert!(!decoded.matches_expected(
                            record.mode(),
                            record.phase(),
                            record.kind(),
                            record.role(),
                            record.nonce(),
                            record.session(),
                        ));
                    }
                }
            }
        }

        let request = GuestEvidenceRecord::worker_request(
            ProbeMode::GuestApiDrop,
            GuestEvidencePhase::ResourceClaim,
            nonce,
            session,
        )
        .expect("request should construct");
        assert!(!request.matches_expected(
            ProbeMode::GuestNoApiDrop,
            GuestEvidencePhase::ResourceClaim,
            GuestEvidenceKind::Request,
            CredentialRole::Worker,
            nonce,
            session,
        ));
        assert!(!request.matches_expected(
            ProbeMode::GuestApiDrop,
            GuestEvidencePhase::HvfCreate,
            GuestEvidenceKind::Request,
            CredentialRole::Worker,
            nonce,
            session,
        ));
        assert!(!request.matches_expected(
            ProbeMode::GuestApiDrop,
            GuestEvidencePhase::ResourceClaim,
            GuestEvidenceKind::Request,
            CredentialRole::Worker,
            SessionId::from_bytes([0x93; 32]),
            session,
        ));
        assert!(!request.matches_expected(
            ProbeMode::GuestApiDrop,
            GuestEvidencePhase::ResourceClaim,
            GuestEvidenceKind::Request,
            CredentialRole::Worker,
            nonce,
            SessionId::from_bytes([0x94; 32]),
        ));
        assert!(
            GuestEvidenceRecord::worker_request(
                ProbeMode::RuntimeDrop,
                GuestEvidencePhase::ResourceClaim,
                nonce,
                session,
            )
            .is_err()
        );
        assert!(
            GuestEvidenceRecord::worker_request(
                ProbeMode::GuestApiDrop,
                GuestEvidencePhase::HvfCreated,
                nonce,
                session,
            )
            .is_err()
        );
        assert!(
            GuestEvidenceRecord::launcher_ack(
                ProbeMode::GuestApiDrop,
                GuestEvidencePhase::GuestShutdown,
                nonce,
                session,
            )
            .is_err()
        );
        assert!(
            GuestEvidenceRecord::worker_report(
                ProbeMode::GuestApiDrop,
                GuestEvidencePhase::ResourceClaim,
                nonce,
                session,
            )
            .is_err()
        );
        assert!(
            GuestEvidenceRecord::worker_report(
                ProbeMode::GuestApiDrop,
                GuestEvidencePhase::GuestShutdown,
                SessionId::pre_session(),
                session,
            )
            .is_err()
        );
    }

    #[test]
    fn runtime_worker_failure_exit_range_is_closed_and_exact() {
        let stages = [
            ProbeStage::RuntimeAuthorityReceive,
            ProbeStage::RuntimeAuthorityValidate,
            ProbeStage::RuntimeSessionLock,
            ProbeStage::RuntimeSessionEnter,
            ProbeStage::LifecyclePrepared,
            ProbeStage::RuntimeNamespaceRetirement,
        ];
        let categories = [
            ProbeErrorCategory::PermissionDenied,
            ProbeErrorCategory::InvalidInput,
            ProbeErrorCategory::Other,
        ];
        for (stage_index, stage) in stages.into_iter().enumerate() {
            for category in categories {
                let failure = RuntimeWorkerFailure::new(stage, category)
                    .expect("worker failure should construct");
                assert_eq!(
                    failure.exit_code(),
                    RUNTIME_WORKER_FAILURE_EXIT_BASE
                        + u8::try_from(stage_index).expect("small stage index") * 3
                        + category as u8
                        - 1
                );
                assert_eq!(
                    RuntimeWorkerFailure::from_exit_code(failure.exit_code()),
                    Ok(failure)
                );
                assert_eq!(failure.stage(), stage);
                assert_eq!(failure.category(), category);
            }
        }
        assert!(
            RuntimeWorkerFailure::new(ProbeStage::RuntimeSessionCreate, ProbeErrorCategory::Other)
                .is_err()
        );
        assert_eq!(
            RuntimeWorkerFailure::from_exit_code(RUNTIME_WORKER_FAILURE_EXIT_BASE - 1),
            Err(ProbeProtocolError)
        );
        assert_eq!(
            RuntimeWorkerFailure::from_exit_code(RUNTIME_WORKER_FAILURE_EXIT_BASE + 18),
            Err(ProbeProtocolError)
        );
    }

    #[test]
    fn continuation_ack_is_closed_nonce_bound_and_new_mode_only() {
        let nonce = SessionId::from_bytes([0x71; 32]);
        for mode in [
            ProbeMode::RuntimeDrop,
            ProbeMode::RuntimeRetainRoot,
            ProbeMode::RuntimeUnmapped,
            ProbeMode::GuestNoApiDrop,
            ProbeMode::GuestNoApiRetainRoot,
            ProbeMode::GuestNoApiUnmapped,
            ProbeMode::GuestApiDrop,
            ProbeMode::GuestApiRetainRoot,
            ProbeMode::GuestApiUnmapped,
        ] {
            let ack = ContinuationAck::launcher(mode, nonce).expect("new mode should acknowledge");
            let encoded = ack.encode();
            assert_eq!(encoded.len(), CONTINUATION_ACK_BYTES);
            assert_eq!(ContinuationAck::decode(&encoded), Ok(ack));
            assert!(ack.matches_expected(mode, nonce));
            assert_eq!(format!("{ack:?}"), "ContinuationAck(<redacted>)");
            for index in [0, 4, 7] {
                let mut malformed = encoded;
                malformed[index] ^= 0xff;
                assert_eq!(ContinuationAck::decode(&malformed), Err(ProbeProtocolError));
            }
            for index in 8..16 {
                let mut malformed = encoded;
                malformed[index] = 1;
                assert_eq!(ContinuationAck::decode(&malformed), Err(ProbeProtocolError));
            }
            let other_mode = if mode == ProbeMode::RuntimeDrop {
                ProbeMode::RuntimeRetainRoot
            } else {
                ProbeMode::RuntimeDrop
            };
            let cross_mode = ContinuationAck::launcher(other_mode, nonce)
                .expect("another runtime acknowledgment should construct");
            assert!(!cross_mode.matches_expected(mode, nonce));
            assert!(!ack.matches_expected(mode, SessionId::from_bytes([0x72; 32])));
        }
        assert!(ContinuationAck::launcher(ProbeMode::CredentialDrop, nonce).is_err());
        assert!(
            ContinuationAck::launcher(ProbeMode::RuntimeDrop, SessionId::pre_session()).is_err()
        );
    }

    fn observation(identity: CredentialIdentityClass, token: PeerTokenClass) -> PeerObservation {
        PeerObservation::new(
            identity,
            identity,
            PeerPidClass::Exact,
            CredentialIdentityClass::Unsupported,
            PeerPidClass::Exact,
            token,
        )
        .expect("complete observation")
    }

    #[test]
    fn credential_datagram_proofs_are_closed_nonce_bound_and_redacted() {
        let nonce = SessionId::from_bytes([0x31; 32]);
        for proof in [
            CredentialDatagramProof::challenge(ProbeMode::CredentialDrop, nonce)
                .expect("valid challenge"),
            CredentialDatagramProof::worker_ready(ProbeMode::CredentialDrop, nonce)
                .expect("valid worker response"),
            CredentialDatagramProof::launcher_release(ProbeMode::CredentialDrop, nonce)
                .expect("valid release"),
        ] {
            let encoded = proof.encode();
            assert_eq!(encoded.len(), CREDENTIAL_DATAGRAM_BYTES);
            assert_eq!(CredentialDatagramProof::decode(&encoded), Ok(proof));
            assert_eq!(format!("{proof:?}"), "CredentialDatagramProof(<redacted>)");
            for index in [0, 4, 9, 15] {
                let mut malformed = encoded;
                malformed[index] ^= 0xff;
                assert_eq!(
                    CredentialDatagramProof::decode(&malformed),
                    Err(ProbeProtocolError)
                );
            }
        }
        assert!(
            CredentialDatagramProof::challenge(ProbeMode::Drop, nonce).is_err(),
            "historical modes must not enter the credential barrier"
        );
        assert!(
            CredentialDatagramProof::challenge(ProbeMode::CredentialDrop, SessionId::pre_session())
                .is_err()
        );
        let mut wrong_role = CredentialDatagramProof::challenge(ProbeMode::CredentialDrop, nonce)
            .expect("valid challenge")
            .encode();
        wrong_role[8] = CredentialRole::Worker as u8;
        assert_eq!(
            CredentialDatagramProof::decode(&wrong_role),
            Err(ProbeProtocolError)
        );

        let challenge = CredentialDatagramProof::challenge(ProbeMode::CredentialDrop, nonce)
            .expect("valid challenge");
        let worker_ready = CredentialDatagramProof::worker_ready(ProbeMode::CredentialDrop, nonce)
            .expect("valid worker response");
        let release = CredentialDatagramProof::launcher_release(ProbeMode::CredentialDrop, nonce)
            .expect("valid release");
        assert!(challenge.matches_expected(
            ProbeMode::CredentialDrop,
            CredentialDatagramPhase::Challenge,
            CredentialRole::Launcher,
            nonce,
        ));
        assert!(worker_ready.matches_expected(
            ProbeMode::CredentialDrop,
            CredentialDatagramPhase::WorkerReady,
            CredentialRole::Worker,
            nonce,
        ));
        assert!(release.matches_expected(
            ProbeMode::CredentialDrop,
            CredentialDatagramPhase::LauncherRelease,
            CredentialRole::Launcher,
            nonce,
        ));
        assert!(
            !challenge.matches_expected(
                ProbeMode::CredentialDrop,
                CredentialDatagramPhase::WorkerReady,
                CredentialRole::Worker,
                nonce,
            ),
            "a replayed challenge must not advance the barrier"
        );
        assert!(
            !release.matches_expected(
                ProbeMode::CredentialDrop,
                CredentialDatagramPhase::Challenge,
                CredentialRole::Launcher,
                nonce,
            ),
            "an out-of-order release must not start a barrier"
        );
        assert!(!worker_ready.matches_expected(
            ProbeMode::CredentialDrop,
            CredentialDatagramPhase::WorkerReady,
            CredentialRole::Worker,
            SessionId::from_bytes([0x32; 32]),
        ));
        assert!(!worker_ready.matches_expected(
            ProbeMode::CredentialRetainRoot,
            CredentialDatagramPhase::WorkerReady,
            CredentialRole::Worker,
            nonce,
        ));
    }

    #[test]
    fn credential_phase_records_round_trip_and_reject_contradictory_shapes() {
        let nonce = SessionId::from_bytes([0x42; 32]);
        let initial = observation(
            CredentialIdentityClass::InitialRoot,
            PeerTokenClass::Baseline,
        );
        let snapshot = observation(
            CredentialIdentityClass::InitialRoot,
            PeerTokenClass::Unchanged,
        );
        let target = observation(CredentialIdentityClass::Target, PeerTokenClass::Changed);
        let state = CredentialSelfState::new(
            CredentialIdentityClass::Target,
            CredentialGroupClass::EffectiveOnly,
        );
        for record in [
            CredentialRecord::worker_transitioned(
                ProbeMode::CredentialDrop,
                state,
                initial,
                snapshot,
                nonce,
            )
            .expect("valid worker transition"),
            CredentialRecord::launcher_transitioned(
                ProbeMode::CredentialDrop,
                state,
                [initial, target, target],
                nonce,
            )
            .expect("valid launcher transition"),
            CredentialRecord::worker_final(
                ProbeMode::CredentialDrop,
                state,
                [initial, snapshot, target],
                nonce,
            )
            .expect("valid worker final"),
            CredentialRecord::failure(
                ProbeMode::CredentialDrop,
                CredentialRole::Worker,
                CredentialFailureValue::new(
                    CredentialStep::SetUid,
                    ProbeErrorCategory::PermissionDenied,
                    CredentialPrefix::GidSet,
                    CredentialSelfState::new(
                        CredentialIdentityClass::Other,
                        CredentialGroupClass::EffectiveOnly,
                    ),
                ),
                initial,
                nonce,
            )
            .expect("valid partial failure"),
        ] {
            let encoded = record.encode();
            assert_eq!(encoded.len(), CREDENTIAL_RECORD_BYTES);
            assert_eq!(CredentialRecord::decode(&encoded), Ok(record));
            assert_eq!(format!("{record:?}"), "CredentialRecord(<redacted>)");
            for index in [0, 4, 15, 34, 47] {
                let mut malformed = encoded;
                malformed[index] ^= 0xff;
                assert_eq!(
                    CredentialRecord::decode(&malformed),
                    Err(ProbeProtocolError)
                );
            }
        }

        assert!(
            CredentialRecord::worker_transitioned(
                ProbeMode::CredentialDrop,
                CredentialSelfState::new(
                    CredentialIdentityClass::InitialRoot,
                    CredentialGroupClass::Initial,
                ),
                initial,
                snapshot,
                nonce,
            )
            .is_err(),
            "success self-attestation must match the selected mode"
        );
        assert!(
            CredentialRecord::failure(
                ProbeMode::CredentialDrop,
                CredentialRole::Worker,
                CredentialFailureValue::new(
                    CredentialStep::SetUid,
                    ProbeErrorCategory::PermissionDenied,
                    CredentialPrefix::GidSet,
                    state,
                ),
                PeerObservation::NONE,
                nonce,
            )
            .is_err(),
            "partial transition failures require the initial observation"
        );
        assert!(
            CredentialRecord::failure(
                ProbeMode::CredentialDrop,
                CredentialRole::Worker,
                CredentialFailureValue::new(
                    CredentialStep::Protocol,
                    ProbeErrorCategory::InvalidInput,
                    CredentialPrefix::None,
                    CredentialSelfState::new(
                        CredentialIdentityClass::NotObserved,
                        CredentialGroupClass::NotObserved,
                    ),
                ),
                PeerObservation::NONE,
                nonce,
            )
            .is_err(),
            "failure self-attestation must remain meaningful"
        );

        let worker_transitioned = CredentialRecord::worker_transitioned(
            ProbeMode::CredentialDrop,
            state,
            initial,
            snapshot,
            nonce,
        )
        .expect("valid worker transition");
        let worker_final = CredentialRecord::worker_final(
            ProbeMode::CredentialDrop,
            state,
            [initial, snapshot, target],
            nonce,
        )
        .expect("valid worker final");
        let mut worker_with_unexpected_final_observation = worker_transitioned.encode();
        let unexpected_observation: [u8; 6] = worker_with_unexpected_final_observation[16..22]
            .try_into()
            .expect("fixed observation slot");
        worker_with_unexpected_final_observation[28..34].copy_from_slice(&unexpected_observation);
        assert_eq!(
            CredentialRecord::decode(&worker_with_unexpected_final_observation),
            Err(ProbeProtocolError),
            "ignored observation slots must remain canonical zeroes"
        );
        let failure = CredentialRecord::failure(
            ProbeMode::CredentialDrop,
            CredentialRole::Worker,
            CredentialFailureValue::new(
                CredentialStep::SetUid,
                ProbeErrorCategory::PermissionDenied,
                CredentialPrefix::GidSet,
                CredentialSelfState::new(
                    CredentialIdentityClass::Other,
                    CredentialGroupClass::EffectiveOnly,
                ),
            ),
            initial,
            nonce,
        )
        .expect("valid partial failure");
        let mut failure_with_unexpected_later_observation = failure.encode();
        failure_with_unexpected_later_observation[22..28].copy_from_slice(&unexpected_observation);
        assert_eq!(
            CredentialRecord::decode(&failure_with_unexpected_later_observation),
            Err(ProbeProtocolError),
            "failure records must reject ignored later observations"
        );
        assert!(worker_transitioned.matches_expected(
            ProbeMode::CredentialDrop,
            CredentialRecordKind::WorkerTransitioned,
            CredentialRole::Worker,
            nonce,
        ));
        assert!(
            !worker_transitioned.matches_expected(
                ProbeMode::CredentialDrop,
                CredentialRecordKind::WorkerFinal,
                CredentialRole::Worker,
                nonce,
            ),
            "a replayed transition record must not satisfy the final phase"
        );
        assert!(
            !worker_final.matches_exchange(
                ProbeMode::CredentialDrop,
                CredentialRole::Worker,
                SessionId::from_bytes([0x43; 32]),
            ),
            "a record from another session must not bind to this exchange"
        );
        assert!(!worker_final.matches_exchange(
            ProbeMode::CredentialDrop,
            CredentialRole::Launcher,
            nonce,
        ));

        let datagram = CredentialDatagramProof::challenge(ProbeMode::CredentialDrop, nonce)
            .expect("valid challenge")
            .encode();
        let mut wrong_stream_family = [0_u8; CREDENTIAL_RECORD_BYTES];
        wrong_stream_family[..CREDENTIAL_DATAGRAM_BYTES].copy_from_slice(&datagram);
        assert_eq!(
            CredentialRecord::decode(&wrong_stream_family),
            Err(ProbeProtocolError)
        );
        let record = worker_transitioned.encode();
        let wrong_datagram_family: &[u8; CREDENTIAL_DATAGRAM_BYTES] = record
            .get(..CREDENTIAL_DATAGRAM_BYTES)
            .and_then(|bytes| bytes.try_into().ok())
            .expect("fixed record prefix");
        assert_eq!(
            CredentialDatagramProof::decode(wrong_datagram_family),
            Err(ProbeProtocolError)
        );
    }
}
