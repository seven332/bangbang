//! Strict portable vhost-user frontend protocol primitives.
//!
//! The crate implements only the reviewed Firecracker v1.16 block frontend
//! request subset over an already connected Unix stream. It does not open or
//! authorize socket paths, implement a backend, or activate a guest device.

#[cfg(not(unix))]
compile_error!("bangbang-vhost-user requires Unix descriptor and socket semantics");

mod error;
mod frontend;
mod message;
mod notifier;
mod transport;

pub use error::{VhostUserError, VhostUserNotifierError};
pub use frontend::{
    SUPPORTED_PROTOCOL_FEATURES, SUPPORTED_VIRTIO_FEATURES, VHOST_USER_F_PROTOCOL_FEATURES,
    VHOST_USER_PROTOCOL_F_CONFIG, VHOST_USER_PROTOCOL_F_REPLY_ACK, VIRTIO_BLK_F_FLUSH,
    VIRTIO_BLK_F_RO, VIRTIO_F_EVENT_IDX, VIRTIO_F_VERSION_1, VhostUserConfig, VhostUserConfigFlags,
    VhostUserFrontend, VhostUserFrontendOptions, VhostUserFrontendState, VhostUserMemoryRegion,
    VhostUserVringAddress,
};
pub use notifier::{
    BackendCallEndpoint, BackendKickEndpoint, CallDrainOutcome, CallNotifier, KickNotifier,
    KickSignalOutcome, create_call_notifier, create_kick_notifier,
};
