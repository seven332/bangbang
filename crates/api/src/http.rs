use std::fmt;

use serde::Serialize;

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
        #[derive(Serialize)]
        struct VersionResponse<'a> {
            firecracker_version: &'a str,
        }

        let body = serde_json::to_string(&VersionResponse {
            firecracker_version: version,
        })
        .expect("serializing version response should not fail");

        Self {
            status: StatusCode::Ok,
            body,
        }
    }

    pub fn fault(message: &str) -> Self {
        #[derive(Serialize)]
        struct FaultMessage<'a> {
            fault_message: &'a str,
        }

        let body = serde_json::to_string(&FaultMessage {
            fault_message: message,
        })
        .expect("serializing fault response should not fail");

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

    let (method, path, header_len, content_length) = parse_request_head(bytes)?;
    let body = bytes
        .get(header_len..)
        .ok_or(RequestError::MalformedRequest)?;

    if body.len() != content_length {
        return Err(RequestError::MalformedRequest);
    }

    if method == "GET" && content_length > 0 {
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
    let content_length = content_length(request.headers)?;

    let total_len = header_len
        .checked_add(content_length)
        .ok_or(RequestError::PayloadTooLarge)?;

    if total_len > HTTP_MAX_PAYLOAD_SIZE {
        return Err(RequestError::PayloadTooLarge);
    }

    Ok(Some(total_len))
}

fn parse_request_head(bytes: &[u8]) -> Result<(&str, &str, usize, usize), RequestError> {
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
    let content_length = content_length(request.headers)?;

    Ok((method, path, header_len, content_length))
}

fn content_length(headers: &[httparse::Header<'_>]) -> Result<usize, RequestError> {
    let mut content_length = None;

    for header in headers {
        if !header.name.eq_ignore_ascii_case("Content-Length") {
            continue;
        }

        if content_length.is_some() {
            return Err(RequestError::MalformedRequest);
        }

        let value =
            std::str::from_utf8(header.value).map_err(|_| RequestError::MalformedRequest)?;
        let parsed = value
            .trim()
            .parse::<usize>()
            .map_err(|_| RequestError::MalformedRequest)?;
        content_length = Some(parsed);
    }

    Ok(content_length.unwrap_or(0))
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
