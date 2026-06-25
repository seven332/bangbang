//! Firecracker-compatible API surface.

pub mod http;
pub mod route;

/// Firecracker's default maximum accepted HTTP request size.
pub const HTTP_MAX_PAYLOAD_SIZE: usize = 51_200;
