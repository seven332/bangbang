use std::fmt;

use crate::route::Endpoint;
use crate::HTTP_MAX_PAYLOAD_SIZE;

const MAX_HEADERS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiRequest {
    GetVersion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestError {
    GetRequestBody,
    InvalidPathMethod,
    MalformedRequest,
    PayloadTooLarge,
}

impl RequestError {
    pub fn fault_message(&self) -> &'static str {
        match self {
            Self::GetRequestBody => "GET request cannot have a body.",
            Self::InvalidPathMethod => "Invalid request method and/or path.",
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

pub fn handle_request_bytes(bytes: &[u8], version: &str) -> HttpResponse {
    match parse_request(bytes) {
        Ok(ApiRequest::GetVersion) => HttpResponse::version(version),
        Err(err) => HttpResponse::fault(err.fault_message()),
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

    match (method, path) {
        ("GET", "/version") => Ok(ApiRequest::GetVersion),
        _ => Err(RequestError::InvalidPathMethod),
    }
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
    let start = value
        .iter()
        .position(|&byte| !is_http_optional_whitespace(byte))
        .unwrap_or(value.len());
    let end = value
        .iter()
        .rposition(|&byte| !is_http_optional_whitespace(byte))
        .map_or(start, |index| index + 1);

    &value[start..end]
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
            ApiRequest::GetVersion => Self::Version,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VERSION: &str = "0.1.0";

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
        let request = b"GET / HTTP/1.1\r\n\r\n";

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
    fn handles_request_bytes_as_response() {
        let response =
            handle_request_bytes(b"GET /version HTTP/1.1\r\nHost: localhost\r\n\r\n", VERSION);

        assert_eq!(response.status(), StatusCode::Ok);
        assert_eq!(response.body(), r#"{"firecracker_version":"0.1.0"}"#);
    }
}
