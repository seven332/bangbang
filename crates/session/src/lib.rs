//! Private launcher-worker session protocol and macOS ownership primitives.

mod codec;
mod grant;
#[cfg(target_os = "macos")]
pub mod macos;
mod state;

pub use codec::{
    CancelSignal, Frame, FrameDecoder, MAX_VMNET_ACTIVE_INTERFACES, MAX_VMNET_BRIDGE_NAME_BYTES,
    MAX_VMNET_BRIDGE_NAMES, Message, ProtocolError, Readiness, Role, SessionId, TerminalCategory,
    VmnetAuthority, VmnetAuthorityError, WorkerPolicy, encode_frame,
};
pub use grant::{
    BatchId, GRANT_HEADER_BYTES, GrantAccess, GrantFrame, GrantId, GrantObjectKind, GrantRecord,
    MAX_BATCH_BOOKMARK_BYTES, MAX_BOOKMARK_BYTES, MAX_GRANT_DATAGRAM_BYTES, MAX_GRANT_ID_BYTES,
    MAX_GRANT_RECORDS, MAX_GRANTS, MAX_SNAPSHOT_OUTPUT_CHILD_BYTES, MAX_SOCKET_CHILD_BYTES,
    ObjectIdentity, ResourceRole, SnapshotOutputChild, SocketChild, decode_grant_frame,
    encode_grant_frame,
};
pub use state::{LauncherLifecycle, LauncherState, WorkerLifecycle, WorkerState};

/// Fixed descriptor used only by the private production-bundle bootstrap.
pub const SESSION_FD: libc::c_int = 3;

/// Fixed descriptor used only for startup grant datagrams.
pub const GRANT_FD: libc::c_int = 4;

/// Fixed descriptor used only by the private launcher-vsock broker.
pub const SOCKET_BROKER_FD: libc::c_int = 5;

/// Private environment marker installed by the production launcher.
pub const SESSION_ENV_KEY: &str = "BANGBANG_INTERNAL_SESSION_V4";

/// Exact value required for [`SESSION_ENV_KEY`].
pub const SESSION_ENV_VALUE: &str = "4";
