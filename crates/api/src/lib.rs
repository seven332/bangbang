//! Firecracker-compatible API surface.

pub mod http;
pub mod route;

/// Firecracker's default maximum accepted HTTP request body size.
pub const HTTP_MAX_PAYLOAD_SIZE: usize = 51_200;

/// Maximum accepted HTTP request head size before body bytes.
///
/// This is a parser safety limit separate from Firecracker's payload limit,
/// which applies to the HTTP body declared by `Content-Length`.
pub const HTTP_MAX_REQUEST_HEAD_SIZE: usize = 16 * 1024;
