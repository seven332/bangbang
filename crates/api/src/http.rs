use std::fmt;

use serde::Deserialize;

use crate::HTTP_MAX_PAYLOAD_SIZE;
use crate::route::Endpoint;

const MAX_HEADERS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiRequest {
    GetInstanceInfo,
    GetVersion,
    PutDrive(Box<DriveConfigRequest>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestError {
    GetRequestBody,
    InvalidPathMethod,
    MismatchedDriveId,
    MalformedRequest,
    PayloadTooLarge,
}

impl RequestError {
    pub fn fault_message(&self) -> &'static str {
        match self {
            Self::GetRequestBody => "GET request cannot have a body.",
            Self::InvalidPathMethod => "Invalid request method and/or path.",
            Self::MismatchedDriveId => "path drive_id must match body drive_id.",
            Self::MalformedRequest => "Malformed HTTP request.",
            Self::PayloadTooLarge => "HTTP request payload exceeds the configured limit.",
        }
    }
}

impl fmt::Display for RequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.fault_message())
    }
}

impl std::error::Error for RequestError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveConfigRequest {
    path_drive_id: String,
    body_drive_id: String,
    path_on_host: String,
    is_root_device: bool,
    is_read_only: Option<bool>,
    partuuid: Option<String>,
    cache_type: Option<DriveCacheType>,
    io_engine: Option<DriveIoEngine>,
    rate_limiter_configured: bool,
    socket: Option<String>,
}

impl DriveConfigRequest {
    pub fn path_drive_id(&self) -> &str {
        &self.path_drive_id
    }

    pub fn body_drive_id(&self) -> &str {
        &self.body_drive_id
    }

    pub fn path_on_host(&self) -> &str {
        &self.path_on_host
    }

    pub const fn is_root_device(&self) -> bool {
        self.is_root_device
    }

    pub const fn is_read_only(&self) -> Option<bool> {
        self.is_read_only
    }

    pub fn partuuid(&self) -> Option<&str> {
        self.partuuid.as_deref()
    }

    pub const fn cache_type(&self) -> Option<DriveCacheType> {
        self.cache_type
    }

    pub const fn io_engine(&self) -> Option<DriveIoEngine> {
        self.io_engine
    }

    pub const fn rate_limiter_configured(&self) -> bool {
        self.rate_limiter_configured
    }

    pub fn socket(&self) -> Option<&str> {
        self.socket.as_deref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum DriveCacheType {
    Unsafe,
    Writeback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum DriveIoEngine {
    Sync,
    Async,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DriveConfigRequestBody {
    drive_id: String,
    path_on_host: String,
    is_root_device: bool,
    #[serde(default)]
    is_read_only: Option<bool>,
    #[serde(default)]
    partuuid: Option<String>,
    #[serde(default)]
    cache_type: Option<DriveCacheType>,
    #[serde(default, rename = "io_engine")]
    io_engine: Option<DriveIoEngine>,
    #[serde(default)]
    rate_limiter: Option<serde_json::Value>,
    #[serde(default)]
    socket: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusCode {
    Ok,
    BadRequest,
}

impl StatusCode {
    pub const fn as_u16(self) -> u16 {
        match self {
            Self::Ok => 200,
            Self::BadRequest => 400,
        }
    }

    const fn reason_phrase(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::BadRequest => "Bad Request",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    status: StatusCode,
    body: String,
}

impl HttpResponse {
    pub fn instance_info(id: &str, state: &str, vmm_version: &str, app_name: &str) -> Self {
        let body = serde_json::json!({
            "app_name": app_name,
            "id": id,
            "state": state,
            "vmm_version": vmm_version,
        })
        .to_string();

        Self {
            status: StatusCode::Ok,
            body,
        }
    }

    pub fn version(version: &str) -> Self {
        let body = serde_json::json!({ "firecracker_version": version }).to_string();

        Self {
            status: StatusCode::Ok,
            body,
        }
    }

    pub fn fault(message: &str) -> Self {
        let body = serde_json::json!({ "fault_message": message }).to_string();

        Self {
            status: StatusCode::BadRequest,
            body,
        }
    }

    pub const fn status(&self) -> StatusCode {
        self.status
    }

    pub fn body(&self) -> &str {
        &self.body
    }

    pub fn to_http_bytes(&self) -> Vec<u8> {
        format!(
            "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            self.status.as_u16(),
            self.status.reason_phrase(),
            self.body.len(),
            self.body
        )
        .into_bytes()
    }
}

pub fn parse_request(bytes: &[u8]) -> Result<ApiRequest, RequestError> {
    if bytes.len() > HTTP_MAX_PAYLOAD_SIZE {
        return Err(RequestError::PayloadTooLarge);
    }

    let (method, path, header_len, request_body) = parse_request_head(bytes)?;
    let body = bytes
        .get(header_len..)
        .ok_or(RequestError::MalformedRequest)?;

    if request_body.has_unsupported_encoding() {
        return Err(RequestError::MalformedRequest);
    }

    checked_request_len(header_len, request_body.content_length())?;

    if body.len() != request_body.content_length() {
        return Err(RequestError::MalformedRequest);
    }

    if method == "GET" && request_body.has_content() {
        return Err(RequestError::GetRequestBody);
    }

    if method == "PUT"
        && let Some(path_drive_id) = drive_path_id(path)
    {
        return parse_drive_config_request(path_drive_id, body);
    }

    match (method, path) {
        ("GET", "/") => Ok(ApiRequest::GetInstanceInfo),
        ("GET", "/version") => Ok(ApiRequest::GetVersion),
        _ => Err(RequestError::InvalidPathMethod),
    }
}

fn drive_path_id(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/drives/")?;
    if rest.is_empty()
        || rest.contains('/')
        || !rest
            .bytes()
            .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
    {
        return None;
    }

    Some(rest)
}

fn parse_drive_config_request(
    path_drive_id: &str,
    body: &[u8],
) -> Result<ApiRequest, RequestError> {
    let body = serde_json::from_slice::<DriveConfigRequestBody>(body)
        .map_err(|_| RequestError::MalformedRequest)?;
    if path_drive_id != body.drive_id {
        return Err(RequestError::MismatchedDriveId);
    }

    Ok(ApiRequest::PutDrive(Box::new(DriveConfigRequest {
        path_drive_id: path_drive_id.to_string(),
        body_drive_id: body.drive_id,
        path_on_host: body.path_on_host,
        is_root_device: body.is_root_device,
        is_read_only: body.is_read_only,
        partuuid: body.partuuid,
        cache_type: body.cache_type,
        io_engine: body.io_engine,
        rate_limiter_configured: body.rate_limiter.is_some(),
        socket: body.socket,
    })))
}

pub fn request_total_len(bytes: &[u8]) -> Result<Option<usize>, RequestError> {
    if bytes.len() > HTTP_MAX_PAYLOAD_SIZE {
        return Err(RequestError::PayloadTooLarge);
    }

    let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut request = httparse::Request::new(&mut headers);
    let status = request
        .parse(bytes)
        .map_err(|_| RequestError::MalformedRequest)?;
    let header_len = match status {
        httparse::Status::Complete(header_len) => header_len,
        httparse::Status::Partial => return Ok(None),
    };
    let body = request_body(request.headers)?;

    if body.has_unsupported_encoding() {
        return Err(RequestError::MalformedRequest);
    }

    Ok(Some(checked_request_len(
        header_len,
        body.content_length(),
    )?))
}

fn parse_request_head(bytes: &[u8]) -> Result<(&str, &str, usize, RequestBody), RequestError> {
    let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut request = httparse::Request::new(&mut headers);

    let status = request
        .parse(bytes)
        .map_err(|_| RequestError::MalformedRequest)?;
    let header_len = match status {
        httparse::Status::Complete(header_len) => header_len,
        httparse::Status::Partial => return Err(RequestError::MalformedRequest),
    };

    let method = request.method.ok_or(RequestError::MalformedRequest)?;
    let path = request.path.ok_or(RequestError::MalformedRequest)?;
    let body = request_body(request.headers)?;

    Ok((method, path, header_len, body))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RequestBody {
    content_length: usize,
    transfer_encoding: bool,
}

impl RequestBody {
    const fn content_length(self) -> usize {
        self.content_length
    }

    const fn has_unsupported_encoding(self) -> bool {
        self.transfer_encoding
    }

    const fn has_content(self) -> bool {
        self.content_length > 0
    }
}

fn request_body(headers: &[httparse::Header<'_>]) -> Result<RequestBody, RequestError> {
    let mut content_length = None;
    let mut transfer_encoding = false;

    for header in headers {
        if header.name.eq_ignore_ascii_case("Content-Length") {
            if content_length.is_some() {
                return Err(RequestError::MalformedRequest);
            }

            content_length = Some(parse_content_length(header.value)?);
        } else if header.name.eq_ignore_ascii_case("Transfer-Encoding") {
            transfer_encoding = true;
        }
    }

    Ok(RequestBody {
        content_length: content_length.unwrap_or(0),
        transfer_encoding,
    })
}

fn parse_content_length(value: &[u8]) -> Result<usize, RequestError> {
    let value = trim_http_optional_whitespace(value);
    if value.is_empty() {
        return Err(RequestError::MalformedRequest);
    }

    let mut parsed = 0usize;
    for byte in value {
        if !byte.is_ascii_digit() {
            return Err(RequestError::MalformedRequest);
        }

        parsed = parsed
            .checked_mul(10)
            .and_then(|parsed| parsed.checked_add(usize::from(byte - b'0')))
            .ok_or(RequestError::PayloadTooLarge)?;
    }

    Ok(parsed)
}

fn trim_http_optional_whitespace(value: &[u8]) -> &[u8] {
    let mut value = value;

    while let Some((&byte, rest)) = value.split_first() {
        if !is_http_optional_whitespace(byte) {
            break;
        }
        value = rest;
    }

    while let Some((&byte, rest)) = value.split_last() {
        if !is_http_optional_whitespace(byte) {
            break;
        }
        value = rest;
    }

    value
}

const fn is_http_optional_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t')
}

fn checked_request_len(header_len: usize, content_length: usize) -> Result<usize, RequestError> {
    let total_len = header_len
        .checked_add(content_length)
        .ok_or(RequestError::PayloadTooLarge)?;

    if total_len > HTTP_MAX_PAYLOAD_SIZE {
        return Err(RequestError::PayloadTooLarge);
    }

    Ok(total_len)
}

impl From<ApiRequest> for Endpoint {
    fn from(request: ApiRequest) -> Self {
        match request {
            ApiRequest::GetInstanceInfo => Self::DescribeInstance,
            ApiRequest::GetVersion => Self::Version,
            ApiRequest::PutDrive(_) => Self::Drive,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VERSION: &str = "0.1.0";

    fn request_with_body(method: &str, path: &str, body: &str) -> Vec<u8> {
        format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    #[test]
    fn parses_get_instance_info() {
        let request = b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n";

        assert_eq!(parse_request(request), Ok(ApiRequest::GetInstanceInfo));
        assert_eq!(request_total_len(request), Ok(Some(request.len())));
    }

    #[test]
    fn rejects_get_instance_info_with_body() {
        let request =
            b"GET / HTTP/1.1\r\nContent-Length: 2\r\nContent-Type: application/json\r\n\r\n{}";

        assert_eq!(parse_request(request), Err(RequestError::GetRequestBody));
    }

    #[test]
    fn parses_get_instance_info_with_zero_content_length() {
        let request = b"GET / HTTP/1.1\r\nContent-Length:\t0 \r\n\r\n";

        assert_eq!(parse_request(request), Ok(ApiRequest::GetInstanceInfo));
        assert_eq!(request_total_len(request), Ok(Some(request.len())));
    }

    #[test]
    fn parses_get_version() {
        let request = b"GET /version HTTP/1.1\r\nHost: localhost\r\n\r\n";

        assert_eq!(parse_request(request), Ok(ApiRequest::GetVersion));
        assert_eq!(request_total_len(request), Ok(Some(request.len())));
    }

    #[test]
    fn rejects_get_version_with_body() {
        let request =
            b"GET /version HTTP/1.1\r\nContent-Length: 2\r\nContent-Type: application/json\r\n\r\n{}";

        assert_eq!(parse_request(request), Err(RequestError::GetRequestBody));
    }

    #[test]
    fn parses_get_version_with_zero_content_length() {
        let request = b"GET /version HTTP/1.1\r\nContent-Length:\t0 \r\n\r\n";

        assert_eq!(parse_request(request), Ok(ApiRequest::GetVersion));
        assert_eq!(request_total_len(request), Ok(Some(request.len())));
    }

    #[test]
    fn parses_put_drive_with_minimal_body() {
        let body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/rootfs.ext4",
            "is_root_device": true
        }"#;
        let request = request_with_body("PUT", "/drives/rootfs", body);

        let parsed = parse_request(&request).expect("drive request should parse");

        let ApiRequest::PutDrive(config) = parsed else {
            panic!("expected drive request");
        };
        assert_eq!(config.path_drive_id(), "rootfs");
        assert_eq!(config.body_drive_id(), "rootfs");
        assert_eq!(config.path_on_host(), "/tmp/rootfs.ext4");
        assert!(config.is_root_device());
        assert_eq!(config.is_read_only(), None);
        assert_eq!(config.partuuid(), None);
        assert_eq!(config.cache_type(), None);
        assert_eq!(config.io_engine(), None);
        assert!(!config.rate_limiter_configured());
        assert_eq!(config.socket(), None);
        assert_eq!(request_total_len(&request), Ok(Some(request.len())));
    }

    #[test]
    fn parses_put_drive_with_complete_body() {
        let body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/rootfs.ext4",
            "is_root_device": true,
            "is_read_only": true,
            "partuuid": "0eaa91a0-01",
            "cache_type": "Unsafe",
            "io_engine": "Sync",
            "rate_limiter": {
                "bandwidth": {
                    "size": 0,
                    "one_time_burst": 0,
                    "refill_time": 0
                }
            },
            "socket": "/tmp/vhost.sock"
        }"#;
        let request = request_with_body("PUT", "/drives/rootfs", body);

        let parsed = parse_request(&request).expect("drive request should parse");

        let ApiRequest::PutDrive(config) = parsed else {
            panic!("expected drive request");
        };
        assert_eq!(config.path_drive_id(), "rootfs");
        assert_eq!(config.body_drive_id(), "rootfs");
        assert_eq!(config.path_on_host(), "/tmp/rootfs.ext4");
        assert!(config.is_root_device());
        assert_eq!(config.is_read_only(), Some(true));
        assert_eq!(config.partuuid(), Some("0eaa91a0-01"));
        assert_eq!(config.cache_type(), Some(DriveCacheType::Unsafe));
        assert_eq!(config.io_engine(), Some(DriveIoEngine::Sync));
        assert!(config.rate_limiter_configured());
        assert_eq!(config.socket(), Some("/tmp/vhost.sock"));
    }

    #[test]
    fn parses_put_drive_with_deferred_field_nulls() {
        let body = r#"{
            "drive_id": "data",
            "path_on_host": "/tmp/data.ext4",
            "is_root_device": false,
            "rate_limiter": null,
            "socket": null
        }"#;
        let request = request_with_body("PUT", "/drives/data", body);

        let parsed = parse_request(&request).expect("drive request should parse");

        let ApiRequest::PutDrive(config) = parsed else {
            panic!("expected drive request");
        };
        assert!(!config.rate_limiter_configured());
        assert_eq!(config.socket(), None);
    }

    #[test]
    fn parses_put_drive_with_deferred_cache_and_io_values() {
        let body = r#"{
            "drive_id": "data",
            "path_on_host": "/tmp/data.ext4",
            "is_root_device": false,
            "cache_type": "Writeback",
            "io_engine": "Async"
        }"#;
        let request = request_with_body("PUT", "/drives/data", body);

        let parsed = parse_request(&request).expect("drive request should parse");

        let ApiRequest::PutDrive(config) = parsed else {
            panic!("expected drive request");
        };
        assert_eq!(config.cache_type(), Some(DriveCacheType::Writeback));
        assert_eq!(config.io_engine(), Some(DriveIoEngine::Async));
    }

    #[test]
    fn rejects_put_drive_mismatched_body_id() {
        let body = r#"{
            "drive_id": "scratch",
            "path_on_host": "/tmp/rootfs.ext4",
            "is_root_device": true
        }"#;
        let request = request_with_body("PUT", "/drives/rootfs", body);

        assert_eq!(
            parse_request(&request),
            Err(RequestError::MismatchedDriveId)
        );
    }

    #[test]
    fn rejects_put_drive_without_path_id() {
        let body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/rootfs.ext4",
            "is_root_device": true
        }"#;

        assert_eq!(
            parse_request(&request_with_body("PUT", "/drives", body)),
            Err(RequestError::InvalidPathMethod)
        );
        assert_eq!(
            parse_request(&request_with_body("PUT", "/drives/", body)),
            Err(RequestError::InvalidPathMethod)
        );
    }

    #[test]
    fn rejects_put_drive_extra_path_segment() {
        let body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/rootfs.ext4",
            "is_root_device": true
        }"#;

        assert_eq!(
            parse_request(&request_with_body("PUT", "/drives/rootfs/extra", body)),
            Err(RequestError::InvalidPathMethod)
        );
    }

    #[test]
    fn rejects_put_drive_invalid_path_id() {
        let body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/rootfs.ext4",
            "is_root_device": true
        }"#;

        assert_eq!(
            parse_request(&request_with_body("PUT", "/drives/root-fs", body)),
            Err(RequestError::InvalidPathMethod)
        );
        assert_eq!(
            parse_request(&request_with_body("PUT", "/drives/rootfs?debug=true", body)),
            Err(RequestError::InvalidPathMethod)
        );
    }

    #[test]
    fn rejects_put_drive_with_empty_body() {
        let request = b"PUT /drives/rootfs HTTP/1.1\r\nContent-Length: 0\r\n\r\n";

        assert_eq!(parse_request(request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_put_drive_with_malformed_json() {
        let request = request_with_body("PUT", "/drives/rootfs", "{");

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_put_drive_missing_required_field() {
        let body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/rootfs.ext4"
        }"#;
        let request = request_with_body("PUT", "/drives/rootfs", body);

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_put_drive_invalid_field_type() {
        let body = r#"{
            "drive_id": 1000,
            "path_on_host": "/tmp/rootfs.ext4",
            "is_root_device": true
        }"#;
        let request = request_with_body("PUT", "/drives/rootfs", body);

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_put_drive_unknown_field() {
        let body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/rootfs.ext4",
            "is_root_device": true,
            "unknown": true
        }"#;
        let request = request_with_body("PUT", "/drives/rootfs", body);

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_put_drive_unknown_cache_value() {
        let body = r#"{
            "drive_id": "rootfs",
            "path_on_host": "/tmp/rootfs.ext4",
            "is_root_device": true,
            "cache_type": "Unknown"
        }"#;
        let request = request_with_body("PUT", "/drives/rootfs", body);

        assert_eq!(parse_request(&request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_get_drive_with_body() {
        let request = request_with_body("GET", "/drives/rootfs", "{}");

        assert_eq!(parse_request(&request), Err(RequestError::GetRequestBody));
    }

    #[test]
    fn rejects_get_version_with_transfer_encoding_body() {
        let request = b"GET /version HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n";

        assert_eq!(parse_request(request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn total_len_rejects_unsupported_transfer_encoding() {
        let request = b"GET /version HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n";

        assert_eq!(
            request_total_len(request),
            Err(RequestError::MalformedRequest)
        );
    }

    #[test]
    fn rejects_unsupported_method() {
        let request = b"PUT /version HTTP/1.1\r\nContent-Length: 0\r\n\r\n";

        assert_eq!(parse_request(request), Err(RequestError::InvalidPathMethod));
    }

    #[test]
    fn rejects_unsupported_path() {
        let request = b"GET /unknown HTTP/1.1\r\n\r\n";

        assert_eq!(parse_request(request), Err(RequestError::InvalidPathMethod));
    }

    #[test]
    fn rejects_malformed_request() {
        let request = b"not-http\r\n\r\n";

        assert_eq!(parse_request(request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_incomplete_body() {
        let request = b"GET /version HTTP/1.1\r\nContent-Length: 2\r\n\r\n{";

        assert_eq!(parse_request(request), Err(RequestError::MalformedRequest));
    }

    #[test]
    fn rejects_non_digit_content_length() {
        let request = b"GET /version HTTP/1.1\r\nContent-Length: +0\r\n\r\n";

        assert_eq!(parse_request(request), Err(RequestError::MalformedRequest));
        assert_eq!(
            request_total_len(request),
            Err(RequestError::MalformedRequest)
        );
    }

    #[test]
    fn rejects_duplicate_content_length() {
        let request = b"GET /version HTTP/1.1\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n";

        assert_eq!(parse_request(request), Err(RequestError::MalformedRequest));
        assert_eq!(
            request_total_len(request),
            Err(RequestError::MalformedRequest)
        );
    }

    #[test]
    fn rejects_declared_content_length_over_payload_limit() {
        let request = format!(
            "GET /version HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            HTTP_MAX_PAYLOAD_SIZE + 1
        );

        assert_eq!(
            parse_request(request.as_bytes()),
            Err(RequestError::PayloadTooLarge)
        );
        assert_eq!(
            request_total_len(request.as_bytes()),
            Err(RequestError::PayloadTooLarge)
        );
    }

    #[test]
    fn rejects_declared_content_length_over_usize() {
        let request =
            b"GET /version HTTP/1.1\r\nContent-Length: 999999999999999999999999999999\r\n\r\n";

        assert_eq!(parse_request(request), Err(RequestError::PayloadTooLarge));
        assert_eq!(
            request_total_len(request),
            Err(RequestError::PayloadTooLarge)
        );
    }

    #[test]
    fn rejects_request_over_payload_limit() {
        let request = vec![b'a'; HTTP_MAX_PAYLOAD_SIZE + 1];

        assert_eq!(parse_request(&request), Err(RequestError::PayloadTooLarge));
    }

    #[test]
    fn response_body_contains_firecracker_version() {
        let response = HttpResponse::version(VERSION);

        assert_eq!(response.status(), StatusCode::Ok);
        assert_eq!(response.body(), r#"{"firecracker_version":"0.1.0"}"#);
    }

    #[test]
    fn response_body_contains_instance_info() {
        let response = HttpResponse::instance_info("demo-1", "Not started", VERSION, "bangbang");
        let body: serde_json::Value =
            serde_json::from_str(response.body()).expect("body should be JSON");

        assert_eq!(response.status(), StatusCode::Ok);
        assert_eq!(
            body,
            serde_json::json!({
                "app_name": "bangbang",
                "id": "demo-1",
                "state": "Not started",
                "vmm_version": "0.1.0",
            })
        );
    }

    #[test]
    fn fault_body_contains_fault_message() {
        let response = HttpResponse::fault("message");

        assert_eq!(response.status(), StatusCode::BadRequest);
        assert_eq!(response.body(), r#"{"fault_message":"message"}"#);
    }

    #[test]
    fn response_bytes_include_http_headers() {
        let response = HttpResponse::version(VERSION);
        let bytes = response.to_http_bytes();
        let text = std::str::from_utf8(&bytes).expect("response should be utf-8");

        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("Content-Type: application/json\r\n"));
        assert!(text.contains(&format!("Content-Length: {}\r\n", response.body().len())));
        assert!(text.ends_with(r#"{"firecracker_version":"0.1.0"}"#));
    }

    #[test]
    fn api_request_converts_to_endpoint() {
        assert_eq!(
            Endpoint::from(ApiRequest::GetInstanceInfo),
            Endpoint::DescribeInstance
        );
        assert_eq!(Endpoint::from(ApiRequest::GetVersion), Endpoint::Version);
        let request = parse_request(&request_with_body(
            "PUT",
            "/drives/rootfs",
            r#"{
                "drive_id": "rootfs",
                "path_on_host": "/tmp/rootfs.ext4",
                "is_root_device": true
            }"#,
        ))
        .expect("drive request should parse");

        assert_eq!(Endpoint::from(request), Endpoint::Drive);
    }
}
