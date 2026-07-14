//! Private launcher-worker session protocol and macOS ownership primitives.

mod codec;
#[cfg(target_os = "macos")]
pub mod macos;
mod state;

pub use codec::{
    CancelSignal, Frame, FrameDecoder, Message, ProtocolError, Readiness, Role, SessionId,
    TerminalCategory, encode_frame,
};
pub use state::{LauncherLifecycle, LauncherState, WorkerLifecycle, WorkerState};

/// Fixed descriptor used only by the private production-bundle bootstrap.
pub const SESSION_FD: libc::c_int = 3;

/// Private environment marker installed by the production launcher.
pub const SESSION_ENV_KEY: &str = "BANGBANG_INTERNAL_SESSION_V1";

/// Exact value required for [`SESSION_ENV_KEY`].
pub const SESSION_ENV_VALUE: &str = "1";
