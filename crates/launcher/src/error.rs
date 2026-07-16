use std::fmt;
use std::io;

/// Firecracker jailer isolation arguments that have no equivalent macOS process boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JailerIsolationArgument {
    /// Configure one Linux cgroup controller property.
    Cgroup,
    /// Select the Linux cgroup hierarchy version.
    CgroupVersion,
    /// Select the parent Linux cgroup.
    ParentCgroup,
    /// Join a path-named Linux network namespace.
    NetworkNamespace,
    /// Create a nested Linux PID namespace.
    PidNamespace,
}

impl JailerIsolationArgument {
    /// Return the fixed Firecracker jailer argument name without its `--` prefix.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Cgroup => "cgroup",
            Self::CgroupVersion => "cgroup-version",
            Self::ParentCgroup => "parent-cgroup",
            Self::NetworkNamespace => "netns",
            Self::PidNamespace => "new-pid-ns",
        }
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        match name {
            "cgroup" => Some(Self::Cgroup),
            "cgroup-version" => Some(Self::CgroupVersion),
            "parent-cgroup" => Some(Self::ParentCgroup),
            "netns" => Some(Self::NetworkNamespace),
            "new-pid-ns" => Some(Self::PidNamespace),
            _ => None,
        }
    }
}

/// Stable launcher failure categories that do not expose package paths or tool output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LauncherError {
    /// The running executable is not in the fixed production bundle layout.
    InvalidBundleLayout,
    /// The fixed bundle or embedded worker is missing or not a regular code object.
    InvalidBundleEntry,
    /// Static code-signature or requirement validation failed.
    InvalidBundleSignature,
    /// Graceful signal observation could not be installed.
    SignalSetup(io::ErrorKind),
    /// The embedded worker could not be started.
    WorkerSpawn(io::ErrorKind),
    /// The private session transport or spawn allowlist could not be established.
    SessionSetup(io::ErrorKind),
    /// The live worker did not match the fixed signed identity.
    InvalidWorkerIdentity,
    /// A daemon handoff peer did not match the fixed outer-launcher identity.
    InvalidDaemonIdentity,
    /// The bounded private launcher-worker protocol failed.
    SessionProtocol,
    /// The explicit external-resource manifest or launcher envelope is invalid.
    InvalidGrantInput,
    /// The versioned production launch-control envelope is invalid.
    InvalidLaunchPolicy,
    /// A Linux-only Firecracker jailer isolation argument was requested on macOS.
    UnsupportedJailerIsolation(JailerIsolationArgument),
    /// The authenticated worker rejected or could not install its launch policy.
    WorkerPolicy,
    /// The private daemon ownership handoff failed.
    DaemonHandoff,
    /// An approved host resource could not be prepared without weakening identity.
    GrantPreparation,
    /// The private startup grant transaction failed.
    GrantProtocol,
    /// The closed launcher-vsock broker protocol failed.
    SocketBroker,
    /// The private per-VM runtime namespace failed validation or cleanup.
    RuntimeNamespace,
    /// Waiting for the embedded worker failed.
    WorkerWait(io::ErrorKind),
    /// A graceful signal could not be forwarded to the owned worker.
    SignalForward(io::ErrorKind),
    /// Production launching is unavailable on this target.
    UnsupportedPlatform,
}

impl fmt::Display for LauncherError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBundleLayout => formatter.write_str("invalid production bundle layout"),
            Self::InvalidBundleEntry => formatter.write_str("invalid production bundle entry"),
            Self::InvalidBundleSignature => {
                formatter.write_str("production bundle signature validation failed")
            }
            Self::SignalSetup(kind) => {
                write!(
                    formatter,
                    "failed to install launcher signal handling: {kind:?}"
                )
            }
            Self::WorkerSpawn(kind) => {
                write!(formatter, "failed to start sandbox worker: {kind:?}")
            }
            Self::SessionSetup(kind) => {
                write!(
                    formatter,
                    "failed to establish private worker session: {kind:?}"
                )
            }
            Self::InvalidWorkerIdentity => {
                formatter.write_str("sandbox worker identity validation failed")
            }
            Self::InvalidDaemonIdentity => {
                formatter.write_str("daemon launcher identity validation failed")
            }
            Self::SessionProtocol => formatter.write_str("private worker session failed"),
            Self::InvalidGrantInput => formatter.write_str("invalid resource grant input"),
            Self::InvalidLaunchPolicy => formatter.write_str("invalid production launch policy"),
            Self::UnsupportedJailerIsolation(argument) => write!(
                formatter,
                "unsupported Firecracker jailer isolation argument on macOS: --{}",
                argument.name()
            ),
            Self::WorkerPolicy => formatter.write_str("sandbox worker launch policy failed"),
            Self::DaemonHandoff => formatter.write_str("private daemon handoff failed"),
            Self::GrantPreparation => formatter.write_str("resource grant preparation failed"),
            Self::GrantProtocol => formatter.write_str("private resource grant failed"),
            Self::SocketBroker => formatter.write_str("private socket broker failed"),
            Self::RuntimeNamespace => {
                formatter.write_str("private worker runtime namespace failed")
            }
            Self::WorkerWait(kind) => {
                write!(formatter, "failed to wait for sandbox worker: {kind:?}")
            }
            Self::SignalForward(kind) => {
                write!(formatter, "failed to forward launcher signal: {kind:?}")
            }
            Self::UnsupportedPlatform => {
                formatter.write_str("production bundle launching requires macOS")
            }
        }
    }
}

impl std::error::Error for LauncherError {}

/// Stable production-package failure categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageError {
    /// Package command input is invalid.
    InvalidInput,
    /// A package input or fixed metadata object is missing or has an unsafe type.
    InvalidInputEntry,
    /// The optional repository-test resource tree is invalid or too large.
    InvalidTestResources,
    /// A vmnet provisioning profile is missing, malformed, mismatched, or unsafe.
    InvalidProvisioningProfile,
    /// A private staging operation failed.
    Staging(io::ErrorKind),
    /// A fixed platform signing or metadata tool failed.
    ToolFailure(&'static str),
    /// Inspection found an unexpected identity, entitlement, or runtime flag.
    InspectionFailure,
    /// The current host rejected or could not execute the vmnet authorization probe.
    AuthorizationBlocked,
    /// The final bundle already exists.
    OutputAlreadyExists,
    /// Exclusive final publication failed.
    Publication(io::ErrorKind),
    /// Production packaging is unavailable on this target.
    UnsupportedPlatform,
}

impl fmt::Display for PackageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput => formatter.write_str("invalid production bundle input"),
            Self::InvalidInputEntry => formatter.write_str("invalid production bundle input entry"),
            Self::InvalidTestResources => {
                formatter.write_str("invalid production bundle test resources")
            }
            Self::InvalidProvisioningProfile => {
                formatter.write_str("invalid production vmnet provisioning profile")
            }
            Self::Staging(kind) => {
                write!(formatter, "failed to stage production bundle: {kind:?}")
            }
            Self::ToolFailure(tool) => {
                write!(formatter, "production bundle {tool} failed")
            }
            Self::InspectionFailure => formatter.write_str("production bundle inspection failed"),
            Self::AuthorizationBlocked => {
                formatter.write_str("production vmnet authorization blocked")
            }
            Self::OutputAlreadyExists => {
                formatter.write_str("production bundle output already exists")
            }
            Self::Publication(kind) => {
                write!(formatter, "failed to publish production bundle: {kind:?}")
            }
            Self::UnsupportedPlatform => {
                formatter.write_str("production bundle packaging requires macOS")
            }
        }
    }
}

impl std::error::Error for PackageError {}
