//! Backend-neutral MMDS control-plane input and metadata query model.

use std::collections::HashMap;
use std::fmt;
use std::net::Ipv4Addr;
use std::str;
use std::time::Instant;

use serde_json::{Map, Value};

use crate::network::NetworkInterfaceConfig;

pub const MMDS_DATA_STORE_LIMIT_BYTES: usize = 51_200;
pub const MMDS_TOKEN_MIN_TTL_SECONDS: u32 = 1;
pub const MMDS_TOKEN_MAX_TTL_SECONDS: u32 = 21_600;
pub const MMDS_TOKEN_MAX_ACTIVE_TOKENS: usize = 1_024;

const MMDS_TOKEN_BYTES: usize = 32;
const MMDS_GUEST_ALLOW_METHODS: &str = "GET, PUT";
const MMDS_GUEST_TOKEN_PATH: &str = "/latest/api/token";
const MMDS_GUEST_X_AWS_EC2_METADATA_TOKEN_TTL_SECONDS: &str =
    "X-aws-ec2-metadata-token-ttl-seconds";
const MMDS_GUEST_X_FORWARDED_FOR: &str = "X-Forwarded-For";
const MMDS_GUEST_X_METADATA_TOKEN_TTL_SECONDS: &str = "X-metadata-token-ttl-seconds";
const MMDS_MILLISECONDS_PER_SECOND: u64 = 1_000;
const MMDS_TOKEN_GENERATION_ATTEMPTS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmdsContentInput {
    value: Value,
}

impl MmdsContentInput {
    pub fn new(value: Value) -> Self {
        Self { value }
    }

    pub fn value(&self) -> &Value {
        &self.value
    }

    pub fn into_value(self) -> Value {
        self.value
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmdsConfigInput {
    network_interfaces: Vec<String>,
    version: MmdsVersion,
    ipv4_address: Option<Ipv4Addr>,
    imds_compat: bool,
}

impl MmdsConfigInput {
    pub fn new(network_interfaces: impl Into<Vec<String>>) -> Self {
        Self {
            network_interfaces: network_interfaces.into(),
            version: MmdsVersion::V1,
            ipv4_address: None,
            imds_compat: false,
        }
    }

    pub fn network_interfaces(&self) -> &[String] {
        &self.network_interfaces
    }

    pub const fn version(&self) -> MmdsVersion {
        self.version
    }

    pub const fn ipv4_address(&self) -> Option<Ipv4Addr> {
        self.ipv4_address
    }

    pub const fn imds_compat(&self) -> bool {
        self.imds_compat
    }

    pub const fn with_version(mut self, version: MmdsVersion) -> Self {
        self.version = version;
        self
    }

    pub const fn with_ipv4_address(mut self, ipv4_address: Ipv4Addr) -> Self {
        self.ipv4_address = Some(ipv4_address);
        self
    }

    pub const fn with_imds_compat(mut self, imds_compat: bool) -> Self {
        self.imds_compat = imds_compat;
        self
    }

    pub fn validate(
        self,
        configured_network_interfaces: &[NetworkInterfaceConfig],
    ) -> Result<MmdsConfig, MmdsConfigError> {
        if self.network_interfaces.is_empty() {
            return Err(MmdsConfigError::EmptyNetworkInterfaceList);
        }

        if let Some(ipv4_address) = self.ipv4_address
            && !is_valid_link_local_ipv4(ipv4_address)
        {
            return Err(MmdsConfigError::InvalidIpv4Address(ipv4_address));
        }

        for iface_id in &self.network_interfaces {
            if !configured_network_interfaces
                .iter()
                .any(|config| config.iface_id() == iface_id)
            {
                return Err(MmdsConfigError::UnknownNetworkInterfaceId {
                    iface_id: iface_id.clone(),
                });
            }
        }

        Ok(MmdsConfig {
            network_interfaces: self.network_interfaces,
            version: self.version,
            ipv4_address: self.ipv4_address,
            imds_compat: self.imds_compat,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmdsVersion {
    V1,
    V2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmdsConfig {
    network_interfaces: Vec<String>,
    version: MmdsVersion,
    ipv4_address: Option<Ipv4Addr>,
    imds_compat: bool,
}

impl MmdsConfig {
    pub fn network_interfaces(&self) -> &[String] {
        &self.network_interfaces
    }

    pub const fn version(&self) -> MmdsVersion {
        self.version
    }

    pub const fn ipv4_address(&self) -> Option<Ipv4Addr> {
        self.ipv4_address
    }

    pub const fn imds_compat(&self) -> bool {
        self.imds_compat
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MmdsConfigError {
    EmptyNetworkInterfaceList,
    InvalidIpv4Address(Ipv4Addr),
    UnknownNetworkInterfaceId { iface_id: String },
}

impl fmt::Display for MmdsConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyNetworkInterfaceList => {
                f.write_str("MMDS network_interfaces must not be empty")
            }
            Self::InvalidIpv4Address(ipv4_address) => {
                write!(
                    f,
                    "MMDS ipv4_address must be a usable RFC 3927 link-local address: {ipv4_address}"
                )
            }
            Self::UnknownNetworkInterfaceId { iface_id } => {
                write!(f, "MMDS network interface id is not configured: {iface_id}")
            }
        }
    }
}

impl std::error::Error for MmdsConfigError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmdsOutputFormat {
    Json,
    Imds,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MmdsGuestRequest {
    Get(MmdsGuestGetRequest),
    TokenPut(MmdsGuestTokenPutRequest),
}

impl MmdsGuestRequest {
    pub fn uri(&self) -> &str {
        match self {
            Self::Get(request) => request.uri(),
            Self::TokenPut(request) => request.uri(),
        }
    }

    pub fn parse_http(bytes: &[u8]) -> Result<Self, MmdsGuestRequestParseError> {
        let request = str::from_utf8(bytes).map_err(|_| MmdsGuestRequestParseError::InvalidUtf8)?;
        let (head, body) = request
            .split_once("\r\n\r\n")
            .ok_or(MmdsGuestRequestParseError::MalformedRequest)?;
        let mut lines = head.split("\r\n");
        let request_line = lines
            .next()
            .ok_or(MmdsGuestRequestParseError::MalformedRequest)?;
        let (method, uri, version) = parse_guest_request_line(request_line)?;
        let method = MmdsGuestRequestMethod::parse(method)?;
        if version != "HTTP/1.0" && version != "HTTP/1.1" {
            return Err(MmdsGuestRequestParseError::UnsupportedHttpVersion);
        }

        let uri = guest_request_uri_path(uri)?;
        let mut content_length = None;
        let mut output_format = MmdsOutputFormat::Imds;
        let mut token_ttl = MmdsGuestTokenTtl::Missing;
        let mut forwarded_for = false;

        for line in lines {
            let (name, value) = parse_guest_request_header(line)?;
            if name.eq_ignore_ascii_case("Content-Length") {
                if content_length.is_some() {
                    return Err(MmdsGuestRequestParseError::DuplicateContentLength);
                }
                content_length = Some(parse_guest_content_length(value)?);
            } else if name.eq_ignore_ascii_case("Transfer-Encoding") {
                return Err(MmdsGuestRequestParseError::UnsupportedTransferEncoding);
            } else if method == MmdsGuestRequestMethod::Get && name.eq_ignore_ascii_case("Accept") {
                output_format = parse_guest_accept_header(value)?;
            } else if method == MmdsGuestRequestMethod::Put {
                if name.eq_ignore_ascii_case(MMDS_GUEST_X_FORWARDED_FOR) {
                    forwarded_for = true;
                } else if let Some(header) = MmdsGuestTokenTtlHeader::parse_name(name) {
                    token_ttl = match token_ttl {
                        MmdsGuestTokenTtl::Missing => MmdsGuestTokenTtl::Header {
                            ttl_header: header,
                            ttl_value: value.to_string(),
                        },
                        MmdsGuestTokenTtl::Header { .. } | MmdsGuestTokenTtl::Duplicate => {
                            MmdsGuestTokenTtl::Duplicate
                        }
                    };
                }
            }
        }

        let content_length = content_length.unwrap_or(0);
        if content_length != 0 || !body.is_empty() {
            return Err(MmdsGuestRequestParseError::UnsupportedBody);
        }

        match method {
            MmdsGuestRequestMethod::Get => Ok(Self::Get(MmdsGuestGetRequest {
                uri: uri.to_string(),
                output_format,
            })),
            MmdsGuestRequestMethod::Put => {
                if forwarded_for {
                    return Err(MmdsGuestRequestParseError::UnsupportedForwardedFor);
                }

                Ok(Self::TokenPut(MmdsGuestTokenPutRequest {
                    uri: uri.to_string(),
                    token_ttl,
                }))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmdsGuestGetRequest {
    uri: String,
    output_format: MmdsOutputFormat,
}

impl MmdsGuestGetRequest {
    pub fn uri(&self) -> &str {
        &self.uri
    }

    pub const fn output_format(&self) -> MmdsOutputFormat {
        self.output_format
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmdsGuestTokenPutRequest {
    uri: String,
    token_ttl: MmdsGuestTokenTtl,
}

impl MmdsGuestTokenPutRequest {
    pub fn uri(&self) -> &str {
        &self.uri
    }

    pub fn token_ttl(&self) -> &MmdsGuestTokenTtl {
        &self.token_ttl
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MmdsGuestTokenTtl {
    Missing,
    Header {
        ttl_header: MmdsGuestTokenTtlHeader,
        ttl_value: String,
    },
    Duplicate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmdsGuestTokenTtlHeader {
    Metadata,
    AwsEc2Metadata,
}

impl MmdsGuestTokenTtlHeader {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Metadata => MMDS_GUEST_X_METADATA_TOKEN_TTL_SECONDS,
            Self::AwsEc2Metadata => MMDS_GUEST_X_AWS_EC2_METADATA_TOKEN_TTL_SECONDS,
        }
    }

    fn parse_name(name: &str) -> Option<Self> {
        if name.eq_ignore_ascii_case(MMDS_GUEST_X_METADATA_TOKEN_TTL_SECONDS) {
            return Some(Self::Metadata);
        }
        if name.eq_ignore_ascii_case(MMDS_GUEST_X_AWS_EC2_METADATA_TOKEN_TTL_SECONDS) {
            return Some(Self::AwsEc2Metadata);
        }

        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MmdsGuestRequestMethod {
    Get,
    Put,
}

impl MmdsGuestRequestMethod {
    fn parse(method: &str) -> Result<Self, MmdsGuestRequestParseError> {
        match method {
            "GET" => Ok(Self::Get),
            "PUT" => Ok(Self::Put),
            _ => Err(MmdsGuestRequestParseError::UnsupportedMethod),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmdsGuestRequestParseError {
    InvalidUtf8,
    MalformedRequest,
    UnsupportedMethod,
    UnsupportedHttpVersion,
    InvalidUri,
    MalformedHeader,
    DuplicateContentLength,
    InvalidContentLength,
    UnsupportedTransferEncoding,
    UnsupportedBody,
    UnsupportedAccept,
    MissingTokenTtl,
    InvalidTokenTtl,
    DuplicateTokenTtl,
    UnsupportedForwardedFor,
}

impl fmt::Display for MmdsGuestRequestParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUtf8 => f.write_str("MMDS guest HTTP request is not valid UTF-8."),
            Self::MalformedRequest => f.write_str("MMDS guest HTTP request is malformed."),
            Self::UnsupportedMethod => {
                f.write_str("MMDS guest HTTP request method is not supported.")
            }
            Self::UnsupportedHttpVersion => {
                f.write_str("MMDS guest HTTP request version is not supported.")
            }
            Self::InvalidUri => f.write_str("Invalid URI."),
            Self::MalformedHeader => f.write_str("MMDS guest HTTP request header is malformed."),
            Self::DuplicateContentLength => {
                f.write_str("MMDS guest HTTP request has duplicate Content-Length headers.")
            }
            Self::InvalidContentLength => {
                f.write_str("MMDS guest HTTP request Content-Length is invalid.")
            }
            Self::UnsupportedTransferEncoding => {
                f.write_str("MMDS guest HTTP request Transfer-Encoding is not supported.")
            }
            Self::UnsupportedBody => f.write_str("MMDS guest HTTP request body is not supported."),
            Self::UnsupportedAccept => {
                f.write_str("MMDS guest HTTP request Accept header is not supported.")
            }
            Self::MissingTokenTtl => f.write_str(
                "Token time to live value not found. Use `X-metadata-token-ttl-seconds` or `X-aws-ec2-metadata-token-ttl-seconds` header to specify the token's lifetime.",
            ),
            Self::InvalidTokenTtl => {
                f.write_str("MMDS guest token TTL header value is invalid.")
            }
            Self::DuplicateTokenTtl => {
                f.write_str("MMDS guest token TTL header is duplicated.")
            }
            Self::UnsupportedForwardedFor => {
                f.write_str("MMDS guest token PUT request does not support X-Forwarded-For.")
            }
        }
    }
}

impl std::error::Error for MmdsGuestRequestParseError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmdsGuestStatus {
    Ok,
    BadRequest,
    NotFound,
    MethodNotAllowed,
    NotImplemented,
}

impl MmdsGuestStatus {
    pub const fn as_u16(&self) -> u16 {
        match self {
            Self::Ok => 200,
            Self::BadRequest => 400,
            Self::NotFound => 404,
            Self::MethodNotAllowed => 405,
            Self::NotImplemented => 501,
        }
    }

    pub const fn reason_phrase(&self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::BadRequest => "Bad Request",
            Self::NotFound => "Not Found",
            Self::MethodNotAllowed => "Method Not Allowed",
            Self::NotImplemented => "Not Implemented",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmdsGuestContentType {
    ApplicationJson,
    PlainText,
}

impl MmdsGuestContentType {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::ApplicationJson => "application/json",
            Self::PlainText => "text/plain",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmdsGuestResponse {
    status: MmdsGuestStatus,
    content_type: MmdsGuestContentType,
    allow: Option<&'static str>,
    custom_headers: Vec<(&'static str, String)>,
    body: String,
}

impl MmdsGuestResponse {
    fn new(status: MmdsGuestStatus, content_type: MmdsGuestContentType, body: String) -> Self {
        Self {
            status,
            content_type,
            allow: None,
            custom_headers: Vec::new(),
            body,
        }
    }

    fn with_allow_header(mut self, allow: &'static str) -> Self {
        self.allow = Some(allow);
        self
    }

    fn with_custom_header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.custom_headers.push((name, value.into()));
        self
    }

    pub const fn status(&self) -> MmdsGuestStatus {
        self.status
    }

    pub const fn content_type(&self) -> MmdsGuestContentType {
        self.content_type
    }

    pub fn body(&self) -> &str {
        &self.body
    }

    pub fn to_http_bytes(&self) -> Vec<u8> {
        let mut response = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\n",
            self.status.as_u16(),
            self.status.reason_phrase(),
            self.content_type.as_str(),
        );
        if let Some(allow) = self.allow {
            response.push_str("Allow: ");
            response.push_str(allow);
            response.push_str("\r\n");
        }
        for (name, value) in &self.custom_headers {
            response.push_str(name);
            response.push_str(": ");
            response.push_str(value);
            response.push_str("\r\n");
        }
        response.push_str("Content-Length: ");
        response.push_str(&self.body.len().to_string());
        response.push_str("\r\n\r\n");
        response.push_str(&self.body);
        response.into_bytes()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MmdsTokenError {
    InvalidTtl { ttl_seconds: u32 },
    ActiveTokenLimitExceeded { limit: usize },
    RandomnessUnavailable,
    TokenCollision,
}

impl fmt::Display for MmdsTokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTtl { ttl_seconds } => write!(
                f,
                "Invalid MMDS token TTL: {ttl_seconds}. Please provide a value between {MMDS_TOKEN_MIN_TTL_SECONDS} and {MMDS_TOKEN_MAX_TTL_SECONDS}."
            ),
            Self::ActiveTokenLimitExceeded { limit } => {
                write!(f, "The MMDS active token limit was exceeded: {limit}.")
            }
            Self::RandomnessUnavailable => f.write_str("MMDS token randomness is unavailable."),
            Self::TokenCollision => f.write_str("MMDS token generation collided repeatedly."),
        }
    }
}

impl std::error::Error for MmdsTokenError {}

#[derive(Debug, Clone, Copy)]
enum MmdsTokenClock {
    System {
        origin: Instant,
    },
    #[cfg(test)]
    Manual {
        now_millis: u64,
    },
}

impl Default for MmdsTokenClock {
    fn default() -> Self {
        Self::System {
            origin: Instant::now(),
        }
    }
}

impl MmdsTokenClock {
    fn now_millis(&self) -> u64 {
        match self {
            Self::System { origin } => {
                u64::try_from(origin.elapsed().as_millis()).unwrap_or(u64::MAX)
            }
            #[cfg(test)]
            Self::Manual { now_millis } => *now_millis,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MmdsTokenAuthority {
    tokens: HashMap<String, u64>,
    max_active_tokens: usize,
    clock: MmdsTokenClock,
}

impl PartialEq for MmdsTokenAuthority {
    fn eq(&self, other: &Self) -> bool {
        self.tokens == other.tokens && self.max_active_tokens == other.max_active_tokens
    }
}

impl Eq for MmdsTokenAuthority {}

impl Default for MmdsTokenAuthority {
    fn default() -> Self {
        Self::new(MMDS_TOKEN_MAX_ACTIVE_TOKENS)
    }
}

impl MmdsTokenAuthority {
    pub fn new(max_active_tokens: usize) -> Self {
        Self {
            tokens: HashMap::new(),
            max_active_tokens,
            clock: MmdsTokenClock::default(),
        }
    }

    pub fn generate_token(&mut self, ttl_seconds: u32) -> Result<String, MmdsTokenError> {
        self.validate_ttl(ttl_seconds)?;

        let now_millis = self.clock.now_millis();
        self.remove_expired_tokens(now_millis);
        if self.tokens.len() >= self.max_active_tokens {
            return Err(MmdsTokenError::ActiveTokenLimitExceeded {
                limit: self.max_active_tokens,
            });
        }

        let expiry_millis = token_expiry_millis(now_millis, ttl_seconds);
        for _ in 0..MMDS_TOKEN_GENERATION_ATTEMPTS {
            let token = generate_opaque_token()?;
            if self.tokens.contains_key(&token) {
                continue;
            }

            self.tokens.insert(token.clone(), expiry_millis);
            return Ok(token);
        }

        Err(MmdsTokenError::TokenCollision)
    }

    pub fn is_valid(&self, token: &str) -> bool {
        if token.is_empty() {
            return false;
        }

        self.tokens
            .get(token)
            .is_some_and(|expiry_millis| *expiry_millis > self.clock.now_millis())
    }

    fn validate_ttl(&self, ttl_seconds: u32) -> Result<(), MmdsTokenError> {
        if (MMDS_TOKEN_MIN_TTL_SECONDS..=MMDS_TOKEN_MAX_TTL_SECONDS).contains(&ttl_seconds) {
            return Ok(());
        }

        Err(MmdsTokenError::InvalidTtl { ttl_seconds })
    }

    fn remove_expired_tokens(&mut self, now_millis: u64) {
        self.tokens
            .retain(|_, expiry_millis| *expiry_millis > now_millis);
    }

    #[cfg(test)]
    fn with_manual_clock(max_active_tokens: usize, now_millis: u64) -> Self {
        Self {
            tokens: HashMap::new(),
            max_active_tokens,
            clock: MmdsTokenClock::Manual { now_millis },
        }
    }

    #[cfg(test)]
    fn set_now_millis(&mut self, now_millis: u64) {
        self.clock = MmdsTokenClock::Manual { now_millis };
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MmdsDataStoreError {
    InvalidObject,
    NotFound,
    NotInitialized,
    DataStoreLimitExceeded {
        limit_bytes: usize,
        size_bytes: usize,
    },
    Serialization,
    UnsupportedValueType,
}

impl fmt::Display for MmdsDataStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidObject => {
                f.write_str("The MMDS data store request body must be a JSON object.")
            }
            Self::NotFound => f.write_str("The MMDS resource does not exist."),
            Self::NotInitialized => f.write_str("The MMDS data store is not initialized."),
            Self::DataStoreLimitExceeded {
                limit_bytes,
                size_bytes,
            } => write!(
                f,
                "The MMDS data store size limit was exceeded: {size_bytes} bytes > {limit_bytes} bytes"
            ),
            Self::Serialization => f.write_str("The MMDS data store could not be serialized."),
            Self::UnsupportedValueType => {
                f.write_str("Cannot retrieve value. The value has an unsupported type.")
            }
        }
    }
}

impl std::error::Error for MmdsDataStoreError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmdsState {
    config: Option<MmdsConfig>,
    value: Option<Value>,
    data_store_limit_bytes: usize,
    token_authority: MmdsTokenAuthority,
}

impl Default for MmdsState {
    fn default() -> Self {
        Self::new(MMDS_DATA_STORE_LIMIT_BYTES)
    }
}

impl MmdsState {
    pub fn new(data_store_limit_bytes: usize) -> Self {
        Self {
            config: None,
            value: None,
            data_store_limit_bytes,
            token_authority: MmdsTokenAuthority::default(),
        }
    }

    pub const fn data_store_limit_bytes(&self) -> usize {
        self.data_store_limit_bytes
    }

    pub fn config(&self) -> Option<&MmdsConfig> {
        self.config.as_ref()
    }

    pub fn put_config(
        &mut self,
        input: MmdsConfigInput,
        configured_network_interfaces: &[NetworkInterfaceConfig],
    ) -> Result<(), MmdsConfigError> {
        self.config = Some(input.validate(configured_network_interfaces)?);
        Ok(())
    }

    pub fn get_data(&self) -> Result<Value, MmdsDataStoreError> {
        self.value
            .as_ref()
            .cloned()
            .ok_or(MmdsDataStoreError::NotInitialized)
    }

    pub fn query_data(
        &self,
        path: &str,
        output_format: MmdsOutputFormat,
    ) -> Result<String, MmdsDataStoreError> {
        let value = self
            .value
            .as_ref()
            .ok_or(MmdsDataStoreError::NotInitialized)?;
        let pointer_path = mmds_pointer_path(path);
        let query_value = value
            .pointer(pointer_path)
            .ok_or(MmdsDataStoreError::NotFound)?;

        if self.config.as_ref().is_some_and(MmdsConfig::imds_compat) {
            return format_imds(query_value);
        }

        match output_format {
            MmdsOutputFormat::Json => Ok(query_value.to_string()),
            MmdsOutputFormat::Imds => format_imds(query_value),
        }
    }

    pub fn guest_get_response(
        &self,
        uri: &str,
        output_format: MmdsOutputFormat,
    ) -> MmdsGuestResponse {
        if uri.is_empty() {
            return MmdsGuestResponse::new(
                MmdsGuestStatus::BadRequest,
                MmdsGuestContentType::PlainText,
                "Invalid URI.".to_string(),
            );
        }

        let query_path = sanitize_guest_uri(uri);
        match self.query_data(&query_path, output_format) {
            Ok(body) => MmdsGuestResponse::new(
                MmdsGuestStatus::Ok,
                self.guest_success_content_type(output_format),
                body,
            ),
            Err(err) => guest_error_response(uri, err),
        }
    }

    pub fn guest_http_response(&mut self, request_bytes: &[u8]) -> MmdsGuestResponse {
        match MmdsGuestRequest::parse_http(request_bytes) {
            Ok(MmdsGuestRequest::Get(request)) => {
                self.guest_get_response(request.uri(), request.output_format())
            }
            Ok(MmdsGuestRequest::TokenPut(request)) => self.guest_token_put_response(&request),
            Err(err) => guest_request_parse_error_response(err),
        }
    }

    pub fn guest_http_response_bytes(&mut self, request_bytes: &[u8]) -> Vec<u8> {
        self.guest_http_response(request_bytes).to_http_bytes()
    }

    pub fn generate_guest_token(&mut self, ttl_seconds: u32) -> Result<String, MmdsTokenError> {
        self.token_authority.generate_token(ttl_seconds)
    }

    pub fn is_guest_token_valid(&self, token: &str) -> bool {
        self.token_authority.is_valid(token)
    }

    pub fn put_data(&mut self, input: MmdsContentInput) -> Result<(), MmdsDataStoreError> {
        let value = input.into_value();
        validate_object(&value)?;
        self.ensure_within_limit(&value)?;
        self.value = Some(value);
        Ok(())
    }

    pub fn patch_data(&mut self, input: MmdsContentInput) -> Result<(), MmdsDataStoreError> {
        let value = self
            .value
            .as_ref()
            .ok_or(MmdsDataStoreError::NotInitialized)?;
        validate_object(input.value())?;
        let mut patched = value.clone();
        json_merge_patch(&mut patched, input.value());
        self.ensure_within_limit(&patched)?;
        self.value = Some(patched);
        Ok(())
    }

    fn ensure_within_limit(&self, value: &Value) -> Result<(), MmdsDataStoreError> {
        let size_bytes = serde_json::to_vec(value)
            .map_err(|_| MmdsDataStoreError::Serialization)?
            .len();
        if size_bytes > self.data_store_limit_bytes {
            return Err(MmdsDataStoreError::DataStoreLimitExceeded {
                limit_bytes: self.data_store_limit_bytes,
                size_bytes,
            });
        }

        Ok(())
    }

    fn guest_success_content_type(&self, output_format: MmdsOutputFormat) -> MmdsGuestContentType {
        if self.config.as_ref().is_some_and(MmdsConfig::imds_compat) {
            return MmdsGuestContentType::PlainText;
        }

        match output_format {
            MmdsOutputFormat::Json => MmdsGuestContentType::ApplicationJson,
            MmdsOutputFormat::Imds => MmdsGuestContentType::PlainText,
        }
    }

    fn guest_token_put_response(
        &mut self,
        request: &MmdsGuestTokenPutRequest,
    ) -> MmdsGuestResponse {
        if sanitize_guest_uri(request.uri()) != MMDS_GUEST_TOKEN_PATH {
            return MmdsGuestResponse::new(
                MmdsGuestStatus::NotFound,
                MmdsGuestContentType::PlainText,
                format!("Resource not found: {}.", request.uri()),
            );
        }

        let (ttl_header, ttl_value) = match request.token_ttl() {
            MmdsGuestTokenTtl::Missing => {
                return guest_request_parse_error_response(
                    MmdsGuestRequestParseError::MissingTokenTtl,
                );
            }
            MmdsGuestTokenTtl::Header {
                ttl_header,
                ttl_value,
            } => (*ttl_header, ttl_value.as_str()),
            MmdsGuestTokenTtl::Duplicate => {
                return guest_request_parse_error_response(
                    MmdsGuestRequestParseError::DuplicateTokenTtl,
                );
            }
        };
        let ttl_seconds = match parse_guest_token_ttl(ttl_value) {
            Ok(ttl_seconds) => ttl_seconds,
            Err(err) => {
                return guest_request_parse_error_response(err);
            }
        };

        match self.generate_guest_token(ttl_seconds) {
            Ok(token) => {
                MmdsGuestResponse::new(MmdsGuestStatus::Ok, MmdsGuestContentType::PlainText, token)
                    .with_custom_header(ttl_header.name(), ttl_seconds.to_string())
            }
            Err(err) => MmdsGuestResponse::new(
                MmdsGuestStatus::BadRequest,
                MmdsGuestContentType::PlainText,
                err.to_string(),
            ),
        }
    }
}

fn token_expiry_millis(now_millis: u64, ttl_seconds: u32) -> u64 {
    now_millis.saturating_add(u64::from(ttl_seconds) * MMDS_MILLISECONDS_PER_SECOND)
}

fn generate_opaque_token() -> Result<String, MmdsTokenError> {
    let mut bytes = [0_u8; MMDS_TOKEN_BYTES];
    getrandom::fill(&mut bytes).map_err(|_| MmdsTokenError::RandomnessUnavailable)?;

    Ok(hex_encode(&bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(hex_digit(byte >> 4));
        output.push(hex_digit(byte & 0x0f));
    }

    output
}

fn hex_digit(nibble: u8) -> char {
    match nibble {
        0..=9 => char::from(b'0' + nibble),
        10..=15 => char::from(b'a' + (nibble - 10)),
        _ => '?',
    }
}

fn mmds_pointer_path(path: &str) -> &str {
    path.strip_suffix('/').unwrap_or(path)
}

fn parse_guest_request_line(
    request_line: &str,
) -> Result<(&str, &str, &str), MmdsGuestRequestParseError> {
    let mut parts = request_line.split_ascii_whitespace();
    let method = parts
        .next()
        .ok_or(MmdsGuestRequestParseError::MalformedRequest)?;
    let uri = parts
        .next()
        .ok_or(MmdsGuestRequestParseError::MalformedRequest)?;
    let version = parts
        .next()
        .ok_or(MmdsGuestRequestParseError::MalformedRequest)?;
    if parts.next().is_some() {
        return Err(MmdsGuestRequestParseError::MalformedRequest);
    }

    Ok((method, uri, version))
}

fn guest_request_uri_path(uri: &str) -> Result<&str, MmdsGuestRequestParseError> {
    if uri.is_empty() {
        return Err(MmdsGuestRequestParseError::InvalidUri);
    }
    if uri.starts_with('/') {
        return Ok(uri);
    }
    if let Some(rest) = uri.strip_prefix("http://") {
        let Some(path_start) = rest.find('/') else {
            return Err(MmdsGuestRequestParseError::InvalidUri);
        };
        if path_start == 0 {
            return Err(MmdsGuestRequestParseError::InvalidUri);
        }
        let path = rest
            .get(path_start..)
            .ok_or(MmdsGuestRequestParseError::InvalidUri)?;
        if path.is_empty() {
            return Err(MmdsGuestRequestParseError::InvalidUri);
        }
        return Ok(path);
    }

    Err(MmdsGuestRequestParseError::InvalidUri)
}

fn parse_guest_request_header(line: &str) -> Result<(&str, &str), MmdsGuestRequestParseError> {
    let (name, value) = line
        .split_once(':')
        .ok_or(MmdsGuestRequestParseError::MalformedHeader)?;
    if !is_http_token(name) {
        return Err(MmdsGuestRequestParseError::MalformedHeader);
    }

    Ok((name, trim_http_optional_whitespace(value)))
}

fn is_http_token(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            matches!(
                byte,
                b'!' | b'#'
                    | b'$'
                    | b'%'
                    | b'&'
                    | b'\''
                    | b'*'
                    | b'+'
                    | b'-'
                    | b'.'
                    | b'^'
                    | b'_'
                    | b'`'
                    | b'|'
                    | b'~'
                    | b'0'..=b'9'
                    | b'A'..=b'Z'
                    | b'a'..=b'z'
            )
        })
}

fn parse_guest_content_length(value: &str) -> Result<usize, MmdsGuestRequestParseError> {
    if value.is_empty() {
        return Err(MmdsGuestRequestParseError::InvalidContentLength);
    }

    let mut parsed = 0usize;
    for byte in value.bytes() {
        if !byte.is_ascii_digit() {
            return Err(MmdsGuestRequestParseError::InvalidContentLength);
        }

        parsed = parsed
            .checked_mul(10)
            .and_then(|parsed| parsed.checked_add(usize::from(byte - b'0')))
            .ok_or(MmdsGuestRequestParseError::InvalidContentLength)?;
    }

    Ok(parsed)
}

fn parse_guest_token_ttl(value: &str) -> Result<u32, MmdsGuestRequestParseError> {
    value
        .parse::<u32>()
        .map_err(|_| MmdsGuestRequestParseError::InvalidTokenTtl)
}

fn parse_guest_accept_header(value: &str) -> Result<MmdsOutputFormat, MmdsGuestRequestParseError> {
    if value.is_empty() || value == "*/*" || value.eq_ignore_ascii_case("text/plain") {
        return Ok(MmdsOutputFormat::Imds);
    }
    if value.eq_ignore_ascii_case("application/json") {
        return Ok(MmdsOutputFormat::Json);
    }

    Err(MmdsGuestRequestParseError::UnsupportedAccept)
}

fn trim_http_optional_whitespace(value: &str) -> &str {
    value.trim_matches(|character| matches!(character, ' ' | '\t'))
}

fn sanitize_guest_uri(uri: &str) -> String {
    let mut sanitized = String::with_capacity(uri.len());
    let mut last_was_slash = false;

    for character in uri.chars() {
        if character == '/' {
            if !last_was_slash {
                sanitized.push(character);
            }
            last_was_slash = true;
        } else {
            sanitized.push(character);
            last_was_slash = false;
        }
    }

    sanitized
}

fn guest_error_response(uri: &str, err: MmdsDataStoreError) -> MmdsGuestResponse {
    let (status, body) = match err {
        MmdsDataStoreError::NotFound => (
            MmdsGuestStatus::NotFound,
            format!("Resource not found: {uri}."),
        ),
        MmdsDataStoreError::UnsupportedValueType => {
            (MmdsGuestStatus::NotImplemented, err.to_string())
        }
        MmdsDataStoreError::InvalidObject
        | MmdsDataStoreError::NotInitialized
        | MmdsDataStoreError::DataStoreLimitExceeded { .. }
        | MmdsDataStoreError::Serialization => (MmdsGuestStatus::BadRequest, err.to_string()),
    };

    MmdsGuestResponse::new(status, MmdsGuestContentType::PlainText, body)
}

fn guest_request_parse_error_response(err: MmdsGuestRequestParseError) -> MmdsGuestResponse {
    let status = match err {
        MmdsGuestRequestParseError::UnsupportedMethod => MmdsGuestStatus::MethodNotAllowed,
        MmdsGuestRequestParseError::InvalidUtf8
        | MmdsGuestRequestParseError::MalformedRequest
        | MmdsGuestRequestParseError::UnsupportedHttpVersion
        | MmdsGuestRequestParseError::InvalidUri
        | MmdsGuestRequestParseError::MalformedHeader
        | MmdsGuestRequestParseError::DuplicateContentLength
        | MmdsGuestRequestParseError::InvalidContentLength
        | MmdsGuestRequestParseError::UnsupportedTransferEncoding
        | MmdsGuestRequestParseError::UnsupportedBody
        | MmdsGuestRequestParseError::UnsupportedAccept
        | MmdsGuestRequestParseError::MissingTokenTtl
        | MmdsGuestRequestParseError::InvalidTokenTtl
        | MmdsGuestRequestParseError::DuplicateTokenTtl
        | MmdsGuestRequestParseError::UnsupportedForwardedFor => MmdsGuestStatus::BadRequest,
    };

    let response = MmdsGuestResponse::new(status, MmdsGuestContentType::PlainText, err.to_string());
    if status == MmdsGuestStatus::MethodNotAllowed {
        return response.with_allow_header(MMDS_GUEST_ALLOW_METHODS);
    }

    response
}

fn format_imds(value: &Value) -> Result<String, MmdsDataStoreError> {
    if let Some(map) = value.as_object() {
        let entries = map
            .iter()
            .map(|(key, value)| {
                if value.is_object() {
                    format!("{key}/")
                } else {
                    key.clone()
                }
            })
            .collect::<Vec<_>>();
        return Ok(entries.join("\n"));
    }

    value
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or(MmdsDataStoreError::UnsupportedValueType)
}

fn validate_object(value: &Value) -> Result<(), MmdsDataStoreError> {
    if value.is_object() {
        Ok(())
    } else {
        Err(MmdsDataStoreError::InvalidObject)
    }
}

fn json_merge_patch(target: &mut Value, patch: &Value) {
    let Some(patch) = patch.as_object() else {
        *target = patch.clone();
        return;
    };

    if !target.is_object() {
        *target = Value::Object(Map::new());
    }

    let Some(target) = target.as_object_mut() else {
        return;
    };

    for (key, value) in patch {
        if value.is_null() {
            target.remove(key);
        } else {
            json_merge_patch(target.entry(key.clone()).or_insert(Value::Null), value);
        }
    }
}

fn is_valid_link_local_ipv4(ipv4_address: Ipv4Addr) -> bool {
    matches!(ipv4_address.octets(), [169, 254, 1..=254, _])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query_value() -> Value {
        serde_json::json!({
            "age": 43,
            "member": false,
            "meta-data": {
                "ami-id": "ami-123",
                "hostname": "demo.local",
            },
            "nothing": null,
            "phones": [
                "+401234567",
                "+441234567",
            ],
            "user-data": "hello",
        })
    }

    fn initialized_query_state() -> MmdsState {
        let mut state = MmdsState::default();
        state
            .put_data(MmdsContentInput::new(query_value()))
            .expect("test MMDS value should initialize");
        state
    }

    fn enable_imds_compat(state: &mut MmdsState) {
        state.config = Some(MmdsConfig {
            network_interfaces: vec!["eth0".to_string()],
            version: MmdsVersion::V1,
            ipv4_address: None,
            imds_compat: true,
        });
    }

    fn assert_json_value(output: &str, expected: Value) {
        let value = serde_json::from_str::<Value>(output).expect("query output should be JSON");
        assert_eq!(value, expected);
    }

    fn assert_guest_response(
        response: MmdsGuestResponse,
        status: MmdsGuestStatus,
        content_type: MmdsGuestContentType,
        body: &str,
    ) {
        assert_eq!(response.status(), status);
        assert_eq!(response.content_type(), content_type);
        assert_eq!(response.body(), body);
    }

    fn assert_guest_http_response(
        bytes: &[u8],
        status: MmdsGuestStatus,
        content_type: MmdsGuestContentType,
        body: &str,
    ) {
        let mut state = initialized_query_state();
        assert_guest_response(state.guest_http_response(bytes), status, content_type, body);
    }

    fn assert_guest_request(
        bytes: &[u8],
        expected_uri: &str,
        expected_output_format: MmdsOutputFormat,
    ) {
        let request =
            MmdsGuestRequest::parse_http(bytes).expect("test MMDS guest HTTP request should parse");
        let MmdsGuestRequest::Get(request) = request else {
            panic!("test MMDS guest HTTP request should be GET");
        };

        assert_eq!(request.uri(), expected_uri);
        assert_eq!(request.output_format(), expected_output_format);
    }

    fn assert_guest_token_put_request(
        bytes: &[u8],
        expected_uri: &str,
        expected_ttl_header: MmdsGuestTokenTtlHeader,
        expected_ttl_value: &str,
    ) {
        let request =
            MmdsGuestRequest::parse_http(bytes).expect("test MMDS guest HTTP request should parse");
        let MmdsGuestRequest::TokenPut(request) = request else {
            panic!("test MMDS guest HTTP request should be token PUT");
        };

        assert_eq!(request.uri(), expected_uri);
        assert_eq!(
            request.token_ttl(),
            &MmdsGuestTokenTtl::Header {
                ttl_header: expected_ttl_header,
                ttl_value: expected_ttl_value.to_string(),
            }
        );
    }

    fn assert_guest_token_put_duplicate_ttl(bytes: &[u8]) {
        let request =
            MmdsGuestRequest::parse_http(bytes).expect("test MMDS guest HTTP request should parse");
        let MmdsGuestRequest::TokenPut(request) = request else {
            panic!("test MMDS guest HTTP request should be token PUT");
        };

        assert_eq!(request.token_ttl(), &MmdsGuestTokenTtl::Duplicate);
    }

    fn serialized_len(value: &Value) -> usize {
        serde_json::to_vec(value)
            .expect("test JSON value should serialize")
            .len()
    }

    fn assert_mmds_token_shape(token: &str) {
        assert_eq!(token.len(), MMDS_TOKEN_BYTES * 2);
        assert!(
            token
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
    }

    #[test]
    fn mmds_token_authority_accepts_ttl_boundaries() {
        let mut authority = MmdsTokenAuthority::with_manual_clock(2, 1_000);

        let min_token = authority
            .generate_token(MMDS_TOKEN_MIN_TTL_SECONDS)
            .expect("minimum token TTL should be accepted");
        let max_token = authority
            .generate_token(MMDS_TOKEN_MAX_TTL_SECONDS)
            .expect("maximum token TTL should be accepted");

        assert_mmds_token_shape(&min_token);
        assert_mmds_token_shape(&max_token);
        assert!(authority.is_valid(&min_token));
        assert!(authority.is_valid(&max_token));
    }

    #[test]
    fn mmds_token_authority_rejects_invalid_ttl_values() {
        let mut authority = MmdsTokenAuthority::with_manual_clock(2, 1_000);

        assert_eq!(
            authority.generate_token(0),
            Err(MmdsTokenError::InvalidTtl { ttl_seconds: 0 })
        );
        assert_eq!(
            authority.generate_token(MMDS_TOKEN_MAX_TTL_SECONDS + 1),
            Err(MmdsTokenError::InvalidTtl {
                ttl_seconds: MMDS_TOKEN_MAX_TTL_SECONDS + 1,
            })
        );
        assert!(authority.tokens.is_empty());
    }

    #[test]
    fn mmds_token_errors_display_deterministic_messages() {
        assert_eq!(
            MmdsTokenError::InvalidTtl { ttl_seconds: 0 }.to_string(),
            "Invalid MMDS token TTL: 0. Please provide a value between 1 and 21600."
        );
        assert_eq!(
            MmdsTokenError::ActiveTokenLimitExceeded { limit: 1 }.to_string(),
            "The MMDS active token limit was exceeded: 1."
        );
        assert_eq!(
            MmdsTokenError::RandomnessUnavailable.to_string(),
            "MMDS token randomness is unavailable."
        );
        assert_eq!(
            MmdsTokenError::TokenCollision.to_string(),
            "MMDS token generation collided repeatedly."
        );
    }

    #[test]
    fn mmds_token_authority_rejects_unknown_empty_and_expired_tokens() {
        let mut authority = MmdsTokenAuthority::with_manual_clock(1, 1_000);
        let token = authority
            .generate_token(1)
            .expect("token generation should succeed");

        assert!(authority.is_valid(&token));
        assert!(!authority.is_valid(""));
        assert!(!authority.is_valid("not-a-generated-token"));

        authority.set_now_millis(1_999);
        assert!(authority.is_valid(&token));

        authority.set_now_millis(2_000);
        assert!(!authority.is_valid(&token));
    }

    #[test]
    fn mmds_token_authority_cleans_expired_tokens_before_capacity_check() {
        let mut authority = MmdsTokenAuthority::with_manual_clock(1, 1_000);
        let first = authority
            .generate_token(1)
            .expect("first token generation should succeed");
        assert_eq!(authority.tokens.len(), 1);

        authority.set_now_millis(2_000);
        assert!(!authority.is_valid(&first));

        let second = authority
            .generate_token(1)
            .expect("expired token should be cleaned before capacity check");

        assert!(authority.is_valid(&second));
        assert_eq!(authority.tokens.len(), 1);
    }

    #[test]
    fn mmds_token_authority_reports_capacity_exhaustion() {
        let mut authority = MmdsTokenAuthority::with_manual_clock(1, 1_000);
        authority
            .generate_token(1)
            .expect("first token generation should succeed");

        assert_eq!(
            authority.generate_token(1),
            Err(MmdsTokenError::ActiveTokenLimitExceeded { limit: 1 })
        );
        assert_eq!(authority.tokens.len(), 1);
    }

    #[test]
    fn mmds_state_guest_token_delegates_to_token_authority() {
        let mut state = MmdsState {
            token_authority: MmdsTokenAuthority::with_manual_clock(1, 1_000),
            ..MmdsState::default()
        };
        let token = state
            .generate_guest_token(1)
            .expect("state token generation should succeed");

        assert!(state.is_guest_token_valid(&token));

        state.token_authority.set_now_millis(2_000);
        assert!(!state.is_guest_token_valid(&token));
    }

    #[test]
    fn mmds_state_equality_ignores_token_clock_origin() {
        assert_eq!(MmdsState::default(), MmdsState::default());
    }

    #[test]
    fn put_data_accepts_exact_data_store_limit() {
        let value = serde_json::json!({"a": ""});
        let mut state = MmdsState::new(serialized_len(&value));

        state
            .put_data(MmdsContentInput::new(value.clone()))
            .expect("exact-limit MMDS value should be accepted");

        assert_eq!(state.get_data(), Ok(value));
    }

    #[test]
    fn put_data_rejects_one_byte_over_data_store_limit_without_initializing() {
        let value = serde_json::json!({"a": ""});
        let limit_bytes = serialized_len(&value) - 1;
        let mut state = MmdsState::new(limit_bytes);

        assert_eq!(
            state.put_data(MmdsContentInput::new(value.clone())),
            Err(MmdsDataStoreError::DataStoreLimitExceeded {
                limit_bytes,
                size_bytes: serialized_len(&value),
            })
        );
        assert_eq!(state.get_data(), Err(MmdsDataStoreError::NotInitialized));
    }

    #[test]
    fn patch_data_accepts_exact_data_store_limit() {
        let original = serde_json::json!({"a": ""});
        let patch = serde_json::json!({"b": ""});
        let patched = serde_json::json!({"a": "", "b": ""});
        let mut state = MmdsState::new(serialized_len(&patched));

        state
            .put_data(MmdsContentInput::new(original))
            .expect("initial MMDS value should fit");
        state
            .patch_data(MmdsContentInput::new(patch))
            .expect("exact-limit patched MMDS value should be accepted");

        assert_eq!(state.get_data(), Ok(patched));
    }

    #[test]
    fn patch_data_rejects_one_byte_over_data_store_limit_without_mutating() {
        let original = serde_json::json!({"a": ""});
        let patch = serde_json::json!({"b": ""});
        let patched = serde_json::json!({"a": "", "b": ""});
        let limit_bytes = serialized_len(&patched) - 1;
        let mut state = MmdsState::new(limit_bytes);

        state
            .put_data(MmdsContentInput::new(original.clone()))
            .expect("initial MMDS value should fit");
        assert_eq!(
            state.patch_data(MmdsContentInput::new(patch)),
            Err(MmdsDataStoreError::DataStoreLimitExceeded {
                limit_bytes,
                size_bytes: serialized_len(&patched),
            })
        );
        assert_eq!(state.get_data(), Ok(original));
    }

    #[test]
    fn query_data_requires_initialized_data_store() {
        let state = MmdsState::default();

        assert_eq!(
            state.query_data("/", MmdsOutputFormat::Json),
            Err(MmdsDataStoreError::NotInitialized)
        );
    }

    #[test]
    fn query_data_returns_root_object_json() {
        let state = initialized_query_state();
        let output = state
            .query_data("/", MmdsOutputFormat::Json)
            .expect("root JSON query should succeed");

        assert_json_value(&output, query_value());
    }

    #[test]
    fn query_data_lists_root_object_as_imds() {
        let state = initialized_query_state();

        assert_eq!(
            state.query_data("/", MmdsOutputFormat::Imds),
            Ok("age\nmember\nmeta-data/\nnothing\nphones\nuser-data".to_string())
        );
    }

    #[test]
    fn query_data_lists_nested_object_and_formats_string_leaf_as_imds() {
        let state = initialized_query_state();

        assert_eq!(
            state.query_data("/meta-data", MmdsOutputFormat::Imds),
            Ok("ami-id\nhostname".to_string())
        );
        assert_eq!(
            state.query_data("/meta-data/hostname", MmdsOutputFormat::Imds),
            Ok("demo.local".to_string())
        );
    }

    #[test]
    fn query_data_ignores_trailing_slash_for_lookup() {
        let state = initialized_query_state();

        assert_eq!(
            state.query_data("/meta-data/", MmdsOutputFormat::Imds),
            Ok("ami-id\nhostname".to_string())
        );
        assert_eq!(
            state.query_data("/phones/", MmdsOutputFormat::Json),
            Ok(r#"["+401234567","+441234567"]"#.to_string())
        );
    }

    #[test]
    fn query_data_returns_json_for_arrays_and_scalars() {
        let state = initialized_query_state();

        assert_eq!(
            state.query_data("/phones", MmdsOutputFormat::Json),
            Ok(r#"["+401234567","+441234567"]"#.to_string())
        );
        assert_eq!(
            state.query_data("/phones/0", MmdsOutputFormat::Json),
            Ok(r#""+401234567""#.to_string())
        );
        assert_eq!(
            state.query_data("/age", MmdsOutputFormat::Json),
            Ok("43".to_string())
        );
        assert_eq!(
            state.query_data("/member", MmdsOutputFormat::Json),
            Ok("false".to_string())
        );
        assert_eq!(
            state.query_data("/nothing", MmdsOutputFormat::Json),
            Ok("null".to_string())
        );
    }

    #[test]
    fn query_data_uses_json_pointer_escaping() {
        let mut state = MmdsState::default();
        state
            .put_data(MmdsContentInput::new(serde_json::json!({
                "with/slash": {
                    "tilde~key": "escaped",
                },
            })))
            .expect("test MMDS value should initialize");

        assert_eq!(
            state.query_data("/with~1slash/tilde~0key", MmdsOutputFormat::Json),
            Ok(r#""escaped""#.to_string())
        );
    }

    #[test]
    fn query_data_rejects_missing_path() {
        let state = initialized_query_state();

        assert_eq!(
            state.query_data("/meta-data/missing", MmdsOutputFormat::Json),
            Err(MmdsDataStoreError::NotFound)
        );
    }

    #[test]
    fn query_data_rejects_unsupported_imds_value_types() {
        let state = initialized_query_state();

        for path in ["/age", "/member", "/nothing", "/phones"] {
            assert_eq!(
                state.query_data(path, MmdsOutputFormat::Imds),
                Err(MmdsDataStoreError::UnsupportedValueType)
            );
        }
    }

    #[test]
    fn query_data_error_messages_match_firecracker_shape() {
        assert_eq!(
            MmdsDataStoreError::NotFound.to_string(),
            "The MMDS resource does not exist."
        );
        assert_eq!(
            MmdsDataStoreError::UnsupportedValueType.to_string(),
            "Cannot retrieve value. The value has an unsupported type."
        );
    }

    #[test]
    fn query_data_imds_compat_forces_imds_formatting() {
        let mut state = initialized_query_state();
        enable_imds_compat(&mut state);

        assert_eq!(
            state.query_data("/meta-data", MmdsOutputFormat::Json),
            Ok("ami-id\nhostname".to_string())
        );
        assert_eq!(
            state.query_data("/age", MmdsOutputFormat::Json),
            Err(MmdsDataStoreError::UnsupportedValueType)
        );
    }

    #[test]
    fn query_data_does_not_mutate_data_store() {
        let state = initialized_query_state();
        let original = state.get_data().expect("data store should be initialized");

        assert_eq!(
            state.query_data("/meta-data", MmdsOutputFormat::Imds),
            Ok("ami-id\nhostname".to_string())
        );
        assert_eq!(state.get_data(), Ok(original));
    }

    #[test]
    fn guest_status_codes_match_http_values() {
        assert_eq!(MmdsGuestStatus::Ok.as_u16(), 200);
        assert_eq!(MmdsGuestStatus::BadRequest.as_u16(), 400);
        assert_eq!(MmdsGuestStatus::NotFound.as_u16(), 404);
        assert_eq!(MmdsGuestStatus::MethodNotAllowed.as_u16(), 405);
        assert_eq!(MmdsGuestStatus::NotImplemented.as_u16(), 501);
    }

    #[test]
    fn guest_status_reason_phrases_match_http_values() {
        assert_eq!(MmdsGuestStatus::Ok.reason_phrase(), "OK");
        assert_eq!(MmdsGuestStatus::BadRequest.reason_phrase(), "Bad Request");
        assert_eq!(MmdsGuestStatus::NotFound.reason_phrase(), "Not Found");
        assert_eq!(
            MmdsGuestStatus::NotImplemented.reason_phrase(),
            "Not Implemented"
        );
        assert_eq!(
            MmdsGuestStatus::MethodNotAllowed.reason_phrase(),
            "Method Not Allowed"
        );
    }

    #[test]
    fn guest_content_type_names_match_http_values() {
        assert_eq!(
            MmdsGuestContentType::ApplicationJson.as_str(),
            "application/json"
        );
        assert_eq!(MmdsGuestContentType::PlainText.as_str(), "text/plain");
    }

    #[test]
    fn mmds_guest_request_parses_get_without_accept_as_imds() {
        assert_guest_request(
            b"GET /latest/meta-data/hostname HTTP/1.1\r\nHost: 169.254.169.254\r\n\r\n",
            "/latest/meta-data/hostname",
            MmdsOutputFormat::Imds,
        );
    }

    #[test]
    fn mmds_guest_request_parses_absolute_form_uri_path() {
        assert_guest_request(
            b"GET http://169.254.169.254/latest/meta-data/hostname HTTP/1.0\r\n\r\n",
            "/latest/meta-data/hostname",
            MmdsOutputFormat::Imds,
        );
    }

    #[test]
    fn mmds_guest_request_parses_application_json_accept() {
        assert_guest_request(
            b"GET /meta-data/hostname HTTP/1.1\r\nAccept: application/json\r\n\r\n",
            "/meta-data/hostname",
            MmdsOutputFormat::Json,
        );
    }

    #[test]
    fn mmds_guest_request_parses_imds_accept_variants() {
        for request in [
            b"GET /meta-data/hostname HTTP/1.1\r\nAccept:\r\n\r\n".as_slice(),
            b"GET /meta-data/hostname HTTP/1.1\r\nAccept: */*\r\n\r\n",
            b"GET /meta-data/hostname HTTP/1.1\r\nAccept:\ttext/plain \r\n\r\n",
        ] {
            assert_guest_request(request, "/meta-data/hostname", MmdsOutputFormat::Imds);
        }
    }

    #[test]
    fn mmds_guest_request_accepts_zero_content_length() {
        assert_guest_request(
            b"GET /meta-data/hostname HTTP/1.1\r\nContent-Length:\t0 \r\n\r\n",
            "/meta-data/hostname",
            MmdsOutputFormat::Imds,
        );
    }

    #[test]
    fn mmds_guest_request_rejects_invalid_utf8() {
        let request = b"GET /meta-data/host\xffname HTTP/1.1\r\n\r\n";

        assert_eq!(
            MmdsGuestRequest::parse_http(request),
            Err(MmdsGuestRequestParseError::InvalidUtf8)
        );
    }

    #[test]
    fn mmds_guest_request_rejects_malformed_request_line() {
        for request in [
            b"GET /meta-data/hostname HTTP/1.1 extra\r\n\r\n".as_slice(),
            b"GET /meta-data/hostname\r\n\r\n",
            b"\r\n\r\n",
        ] {
            assert_eq!(
                MmdsGuestRequest::parse_http(request),
                Err(MmdsGuestRequestParseError::MalformedRequest)
            );
        }
    }

    #[test]
    fn mmds_guest_request_rejects_unsupported_method_and_version() {
        assert_eq!(
            MmdsGuestRequest::parse_http(b"POST /meta-data/hostname HTTP/1.1\r\n\r\n"),
            Err(MmdsGuestRequestParseError::UnsupportedMethod)
        );
        assert_eq!(
            MmdsGuestRequest::parse_http(b"GET /meta-data/hostname HTTP/2\r\n\r\n"),
            Err(MmdsGuestRequestParseError::UnsupportedHttpVersion)
        );
    }

    #[test]
    fn mmds_guest_request_rejects_invalid_uri() {
        for request in [
            b"GET http://169.254.169.254 HTTP/1.1\r\n\r\n".as_slice(),
            b"GET http:///meta-data/hostname HTTP/1.1\r\n\r\n",
            b"GET http:// HTTP/1.1\r\n\r\n",
            b"GET * HTTP/1.1\r\n\r\n",
        ] {
            assert_eq!(
                MmdsGuestRequest::parse_http(request),
                Err(MmdsGuestRequestParseError::InvalidUri)
            );
        }
    }

    #[test]
    fn mmds_guest_request_rejects_malformed_headers() {
        for request in [
            b"GET /meta-data/hostname HTTP/1.1\r\nAccept application/json\r\n\r\n".as_slice(),
            b"GET /meta-data/hostname HTTP/1.1\r\nBad Header: value\r\n\r\n",
            b"GET /meta-data/hostname HTTP/1.1\r\n: value\r\n\r\n",
            b"GET /meta-data/hostname HTTP/1.1\r\nBad\x7fHeader: value\r\n\r\n",
        ] {
            assert_eq!(
                MmdsGuestRequest::parse_http(request),
                Err(MmdsGuestRequestParseError::MalformedHeader)
            );
        }
    }

    #[test]
    fn mmds_guest_request_rejects_body_framing() {
        assert_eq!(
            MmdsGuestRequest::parse_http(
                b"GET /meta-data/hostname HTTP/1.1\r\nContent-Length: 0\r\n\r\nbody"
            ),
            Err(MmdsGuestRequestParseError::UnsupportedBody)
        );
        assert_eq!(
            MmdsGuestRequest::parse_http(
                b"GET /meta-data/hostname HTTP/1.1\r\nContent-Length: 4\r\n\r\nbody"
            ),
            Err(MmdsGuestRequestParseError::UnsupportedBody)
        );
        assert_eq!(
            MmdsGuestRequest::parse_http(
                b"GET /meta-data/hostname HTTP/1.1\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n",
            ),
            Err(MmdsGuestRequestParseError::DuplicateContentLength)
        );
        assert_eq!(
            MmdsGuestRequest::parse_http(
                b"GET /meta-data/hostname HTTP/1.1\r\nContent-Length: +0\r\n\r\n"
            ),
            Err(MmdsGuestRequestParseError::InvalidContentLength)
        );
        assert_eq!(
            MmdsGuestRequest::parse_http(
                b"GET /meta-data/hostname HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n"
            ),
            Err(MmdsGuestRequestParseError::UnsupportedTransferEncoding)
        );
    }

    #[test]
    fn mmds_guest_request_rejects_unsupported_accept_header() {
        assert_eq!(
            MmdsGuestRequest::parse_http(
                b"GET /meta-data/hostname HTTP/1.1\r\nAccept: application/xml\r\n\r\n"
            ),
            Err(MmdsGuestRequestParseError::UnsupportedAccept)
        );
    }

    #[test]
    fn mmds_guest_request_parses_token_put_ttl_headers() {
        assert_guest_token_put_request(
            b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 60\r\n\r\n",
            "/latest/api/token",
            MmdsGuestTokenTtlHeader::Metadata,
            "60",
        );
        assert_guest_token_put_request(
            b"PUT /latest/api/token HTTP/1.1\r\nX-aws-ec2-metadata-token-ttl-seconds: 21600\r\n\r\n",
            "/latest/api/token",
            MmdsGuestTokenTtlHeader::AwsEc2Metadata,
            "21600",
        );
        assert_guest_token_put_request(
            b"PUT http://169.254.169.254/latest/api/token HTTP/1.1\r\nx-MeTaDaTa-ToKeN-TtL-SeCoNdS: 1\r\n\r\n",
            "/latest/api/token",
            MmdsGuestTokenTtlHeader::Metadata,
            "1",
        );
        assert_guest_token_put_request(
            b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: application/json\r\n\r\n",
            "/latest/api/token",
            MmdsGuestTokenTtlHeader::Metadata,
            "application/json",
        );
        assert_guest_token_put_duplicate_ttl(
            b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 1\r\nX-aws-ec2-metadata-token-ttl-seconds: 1\r\n\r\n",
        );
    }

    #[test]
    fn mmds_guest_request_rejects_forwarded_for_token_put_header() {
        assert_eq!(
            MmdsGuestRequest::parse_http(
                b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 1\r\nX-Forwarded-For: 127.0.0.1\r\n\r\n",
            ),
            Err(MmdsGuestRequestParseError::UnsupportedForwardedFor)
        );
    }

    #[test]
    fn mmds_guest_request_rejects_token_put_body() {
        assert_eq!(
            MmdsGuestRequest::parse_http(
                b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 60\r\nContent-Length: 4\r\n\r\nbody",
            ),
            Err(MmdsGuestRequestParseError::UnsupportedBody)
        );
    }

    #[test]
    fn mmds_guest_request_feeds_guest_get_response_path() {
        let state = initialized_query_state();
        let request = MmdsGuestRequest::parse_http(
            b"GET /meta-data/hostname HTTP/1.1\r\nAccept: application/json\r\n\r\n",
        )
        .expect("test MMDS guest HTTP request should parse");
        let MmdsGuestRequest::Get(request) = request else {
            panic!("test MMDS guest HTTP request should be GET");
        };
        let response = state.guest_get_response(request.uri(), request.output_format());

        assert_eq!(response.status(), MmdsGuestStatus::Ok);
        assert_eq!(
            response.content_type(),
            MmdsGuestContentType::ApplicationJson
        );
        assert_eq!(response.body(), r#""demo.local""#);
    }

    #[test]
    fn mmds_guest_http_response_returns_json_success() {
        let mut state = initialized_query_state();
        let response = state.guest_http_response(
            b"GET /meta-data/hostname HTTP/1.1\r\nAccept: application/json\r\n\r\n",
        );

        assert_eq!(response.status(), MmdsGuestStatus::Ok);
        assert_eq!(
            response.content_type(),
            MmdsGuestContentType::ApplicationJson
        );
        assert_eq!(response.body(), r#""demo.local""#);
    }

    #[test]
    fn mmds_guest_http_response_bytes_return_imds_success() {
        let mut state = initialized_query_state();

        assert_eq!(
            state.guest_http_response_bytes(
                b"GET /meta-data/hostname HTTP/1.1\r\nAccept: */*\r\n\r\n"
            ),
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 10\r\n\r\ndemo.local"
                .to_vec()
        );
    }

    #[test]
    fn mmds_guest_http_response_generates_token_for_put() {
        let mut state = initialized_query_state();
        let response = state.guest_http_response(
            b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 60\r\n\r\n",
        );

        assert_eq!(response.status(), MmdsGuestStatus::Ok);
        assert_eq!(response.content_type(), MmdsGuestContentType::PlainText);
        assert_mmds_token_shape(response.body());
        assert!(state.is_guest_token_valid(response.body()));
    }

    #[test]
    fn mmds_guest_http_response_bytes_include_token_ttl_header() {
        let mut state = initialized_query_state();
        let bytes = state.guest_http_response_bytes(
            b"PUT /latest/api/token HTTP/1.1\r\nX-aws-ec2-metadata-token-ttl-seconds: +60\r\n\r\n",
        );
        let response = String::from_utf8(bytes).expect("token response should be UTF-8");
        let (head, token) = response
            .split_once("\r\n\r\n")
            .expect("token response should include header terminator");

        assert_eq!(
            head,
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nX-aws-ec2-metadata-token-ttl-seconds: 60\r\nContent-Length: 64"
        );
        assert_mmds_token_shape(token);
        assert!(state.is_guest_token_valid(token));
    }

    #[test]
    fn mmds_guest_http_response_maps_token_put_errors() {
        for (request, status, body) in [
            (
                b"PUT /latest/api/token HTTP/1.1\r\n\r\n".as_slice(),
                MmdsGuestStatus::BadRequest,
                "Token time to live value not found. Use `X-metadata-token-ttl-seconds` or `X-aws-ec2-metadata-token-ttl-seconds` header to specify the token's lifetime.",
            ),
            (
                b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: application/json\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest token TTL header value is invalid.",
            ),
            (
                b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: \r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest token TTL header value is invalid.",
            ),
            (
                b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 4294967296\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest token TTL header value is invalid.",
            ),
            (
                b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 1\r\nX-aws-ec2-metadata-token-ttl-seconds: 1\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest token TTL header is duplicated.",
            ),
            (
                b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 21601\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "Invalid MMDS token TTL: 21601. Please provide a value between 1 and 21600.",
            ),
            (
                b"PUT /wrong HTTP/1.1\r\nX-metadata-token-ttl-seconds: 60\r\n\r\n",
                MmdsGuestStatus::NotFound,
                "Resource not found: /wrong.",
            ),
            (
                b"PUT /wrong HTTP/1.1\r\n\r\n",
                MmdsGuestStatus::NotFound,
                "Resource not found: /wrong.",
            ),
            (
                b"PUT /wrong HTTP/1.1\r\nX-metadata-token-ttl-seconds: application/json\r\n\r\n",
                MmdsGuestStatus::NotFound,
                "Resource not found: /wrong.",
            ),
            (
                b"PUT /wrong HTTP/1.1\r\nX-metadata-token-ttl-seconds: 1\r\nX-aws-ec2-metadata-token-ttl-seconds: 1\r\n\r\n",
                MmdsGuestStatus::NotFound,
                "Resource not found: /wrong.",
            ),
            (
                b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 60\r\nX-Forwarded-For: 127.0.0.1\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest token PUT request does not support X-Forwarded-For.",
            ),
            (
                b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 60\r\nContent-Length: 4\r\n\r\nbody",
                MmdsGuestStatus::BadRequest,
                "MMDS guest HTTP request body is not supported.",
            ),
        ] {
            let mut state = initialized_query_state();
            assert_guest_response(
                state.guest_http_response(request),
                status,
                MmdsGuestContentType::PlainText,
                body,
            );
        }
    }

    #[test]
    fn mmds_guest_http_response_token_put_errors_do_not_create_tokens() {
        for request in [
            b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: application/json\r\n\r\n".as_slice(),
            b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 1\r\nX-aws-ec2-metadata-token-ttl-seconds: 1\r\n\r\n",
            b"PUT /wrong HTTP/1.1\r\nX-metadata-token-ttl-seconds: 1\r\n\r\n",
        ] {
            let mut state = initialized_query_state();
            state.token_authority = MmdsTokenAuthority::with_manual_clock(1, 1_000);

            assert_ne!(
                state.guest_http_response(request).status(),
                MmdsGuestStatus::Ok
            );
            let token = state
                .generate_guest_token(1)
                .expect("failed token PUT should not consume token capacity");
            assert!(state.is_guest_token_valid(&token));
        }
    }

    #[test]
    fn mmds_guest_http_response_get_does_not_enforce_tokens_yet() {
        let mut state = initialized_query_state();
        let response = state.guest_http_response(
            b"GET /meta-data/hostname HTTP/1.1\r\nAccept: application/json\r\n\r\n",
        );

        assert_eq!(response.status(), MmdsGuestStatus::Ok);
        assert_eq!(response.body(), r#""demo.local""#);
    }

    #[test]
    fn mmds_guest_http_response_maps_uninitialized_store() {
        let mut state = MmdsState::default();

        assert_guest_response(
            state.guest_http_response(
                b"GET /meta-data/hostname HTTP/1.1\r\nAccept: application/json\r\n\r\n",
            ),
            MmdsGuestStatus::BadRequest,
            MmdsGuestContentType::PlainText,
            "The MMDS data store is not initialized.",
        );
    }

    #[test]
    fn mmds_guest_http_response_maps_parse_errors() {
        for (request, status, body) in [
            (
                b"GET /meta-data/host\xffname HTTP/1.1\r\n\r\n".as_slice(),
                MmdsGuestStatus::BadRequest,
                "MMDS guest HTTP request is not valid UTF-8.",
            ),
            (
                b"GET /meta-data/hostname HTTP/1.1 extra\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest HTTP request is malformed.",
            ),
            (
                b"POST /meta-data/hostname HTTP/1.1\r\n\r\n",
                MmdsGuestStatus::MethodNotAllowed,
                "MMDS guest HTTP request method is not supported.",
            ),
            (
                b"GET /meta-data/hostname HTTP/2\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest HTTP request version is not supported.",
            ),
            (
                b"GET * HTTP/1.1\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "Invalid URI.",
            ),
            (
                b"GET /meta-data/hostname HTTP/1.1\r\nBad Header: value\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest HTTP request header is malformed.",
            ),
            (
                b"GET /meta-data/hostname HTTP/1.1\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest HTTP request has duplicate Content-Length headers.",
            ),
            (
                b"GET /meta-data/hostname HTTP/1.1\r\nContent-Length: +0\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest HTTP request Content-Length is invalid.",
            ),
            (
                b"GET /meta-data/hostname HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest HTTP request Transfer-Encoding is not supported.",
            ),
            (
                b"GET /meta-data/hostname HTTP/1.1\r\nContent-Length: 4\r\n\r\nbody",
                MmdsGuestStatus::BadRequest,
                "MMDS guest HTTP request body is not supported.",
            ),
            (
                b"GET /meta-data/hostname HTTP/1.1\r\nAccept: application/xml\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest HTTP request Accept header is not supported.",
            ),
            (
                b"PUT /latest/api/token HTTP/1.1\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "Token time to live value not found. Use `X-metadata-token-ttl-seconds` or `X-aws-ec2-metadata-token-ttl-seconds` header to specify the token's lifetime.",
            ),
            (
                b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: abc\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest token TTL header value is invalid.",
            ),
            (
                b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 1\r\nX-aws-ec2-metadata-token-ttl-seconds: 1\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest token TTL header is duplicated.",
            ),
            (
                b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 1\r\nX-Forwarded-For: 127.0.0.1\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest token PUT request does not support X-Forwarded-For.",
            ),
        ] {
            assert_guest_http_response(
                request,
                status,
                MmdsGuestContentType::PlainText,
                body,
            );
        }
    }

    #[test]
    fn mmds_guest_http_response_bytes_serialize_parse_error() {
        let mut state = initialized_query_state();

        assert_eq!(
            state.guest_http_response_bytes(b"GET /meta-data/hostname\r\n\r\n"),
            b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: 37\r\n\r\nMMDS guest HTTP request is malformed."
                .to_vec()
        );
    }

    #[test]
    fn mmds_guest_http_response_bytes_serialize_method_not_allowed() {
        let mut state = initialized_query_state();

        assert_eq!(
            state.guest_http_response_bytes(b"POST /meta-data/hostname HTTP/1.1\r\n\r\n"),
            b"HTTP/1.1 405 Method Not Allowed\r\nContent-Type: text/plain\r\nAllow: GET, PUT\r\nContent-Length: 48\r\n\r\nMMDS guest HTTP request method is not supported."
                .to_vec()
        );
    }

    #[test]
    fn mmds_guest_http_response_parse_error_does_not_mutate_data_store() {
        let mut state = initialized_query_state();
        let original = state.get_data().expect("data store should be initialized");

        assert_guest_response(
            state.guest_http_response(
                b"GET /meta-data/hostname HTTP/1.1\r\nContent-Length: 4\r\n\r\nbody",
            ),
            MmdsGuestStatus::BadRequest,
            MmdsGuestContentType::PlainText,
            "MMDS guest HTTP request body is not supported.",
        );
        assert_eq!(state.get_data(), Ok(original));
    }

    #[test]
    fn mmds_guest_request_parse_errors_display_deterministic_messages() {
        assert_eq!(
            MmdsGuestRequestParseError::InvalidUtf8.to_string(),
            "MMDS guest HTTP request is not valid UTF-8."
        );
        assert_eq!(
            MmdsGuestRequestParseError::MalformedRequest.to_string(),
            "MMDS guest HTTP request is malformed."
        );
        assert_eq!(
            MmdsGuestRequestParseError::UnsupportedMethod.to_string(),
            "MMDS guest HTTP request method is not supported."
        );
        assert_eq!(
            MmdsGuestRequestParseError::UnsupportedHttpVersion.to_string(),
            "MMDS guest HTTP request version is not supported."
        );
        assert_eq!(
            MmdsGuestRequestParseError::InvalidUri.to_string(),
            "Invalid URI."
        );
        assert_eq!(
            MmdsGuestRequestParseError::MalformedHeader.to_string(),
            "MMDS guest HTTP request header is malformed."
        );
        assert_eq!(
            MmdsGuestRequestParseError::DuplicateContentLength.to_string(),
            "MMDS guest HTTP request has duplicate Content-Length headers."
        );
        assert_eq!(
            MmdsGuestRequestParseError::InvalidContentLength.to_string(),
            "MMDS guest HTTP request Content-Length is invalid."
        );
        assert_eq!(
            MmdsGuestRequestParseError::UnsupportedTransferEncoding.to_string(),
            "MMDS guest HTTP request Transfer-Encoding is not supported."
        );
        assert_eq!(
            MmdsGuestRequestParseError::UnsupportedBody.to_string(),
            "MMDS guest HTTP request body is not supported."
        );
        assert_eq!(
            MmdsGuestRequestParseError::UnsupportedAccept.to_string(),
            "MMDS guest HTTP request Accept header is not supported."
        );
        assert_eq!(
            MmdsGuestRequestParseError::MissingTokenTtl.to_string(),
            "Token time to live value not found. Use `X-metadata-token-ttl-seconds` or `X-aws-ec2-metadata-token-ttl-seconds` header to specify the token's lifetime."
        );
        assert_eq!(
            MmdsGuestRequestParseError::InvalidTokenTtl.to_string(),
            "MMDS guest token TTL header value is invalid."
        );
        assert_eq!(
            MmdsGuestRequestParseError::DuplicateTokenTtl.to_string(),
            "MMDS guest token TTL header is duplicated."
        );
        assert_eq!(
            MmdsGuestRequestParseError::UnsupportedForwardedFor.to_string(),
            "MMDS guest token PUT request does not support X-Forwarded-For."
        );
    }

    #[test]
    fn guest_get_response_returns_json_body() {
        let state = initialized_query_state();
        let response = state.guest_get_response("/", MmdsOutputFormat::Json);

        assert_eq!(response.status(), MmdsGuestStatus::Ok);
        assert_eq!(
            response.content_type(),
            MmdsGuestContentType::ApplicationJson
        );
        assert_json_value(response.body(), query_value());
    }

    #[test]
    fn guest_get_response_returns_imds_body() {
        let state = initialized_query_state();

        assert_guest_response(
            state.guest_get_response("/", MmdsOutputFormat::Imds),
            MmdsGuestStatus::Ok,
            MmdsGuestContentType::PlainText,
            "age\nmember\nmeta-data/\nnothing\nphones\nuser-data",
        );
    }

    #[test]
    fn guest_get_response_imds_compat_forces_plain_text_response() {
        let mut state = initialized_query_state();
        enable_imds_compat(&mut state);

        assert_guest_response(
            state.guest_get_response("/meta-data", MmdsOutputFormat::Json),
            MmdsGuestStatus::Ok,
            MmdsGuestContentType::PlainText,
            "ami-id\nhostname",
        );
    }

    #[test]
    fn guest_get_response_rejects_empty_uri() {
        let state = initialized_query_state();

        assert_guest_response(
            state.guest_get_response("", MmdsOutputFormat::Json),
            MmdsGuestStatus::BadRequest,
            MmdsGuestContentType::PlainText,
            "Invalid URI.",
        );
    }

    #[test]
    fn guest_get_response_uses_original_uri_in_missing_path_body() {
        let state = initialized_query_state();

        assert_guest_response(
            state.guest_get_response("//meta-data//missing", MmdsOutputFormat::Json),
            MmdsGuestStatus::NotFound,
            MmdsGuestContentType::PlainText,
            "Resource not found: //meta-data//missing.",
        );
    }

    #[test]
    fn guest_get_response_maps_unsupported_imds_value_type() {
        let state = initialized_query_state();

        assert_guest_response(
            state.guest_get_response("/age", MmdsOutputFormat::Imds),
            MmdsGuestStatus::NotImplemented,
            MmdsGuestContentType::PlainText,
            "Cannot retrieve value. The value has an unsupported type.",
        );
    }

    #[test]
    fn guest_get_response_sanitizes_repeated_slashes_for_lookup() {
        let state = initialized_query_state();

        assert_guest_response(
            state.guest_get_response("//meta-data//hostname", MmdsOutputFormat::Imds),
            MmdsGuestStatus::Ok,
            MmdsGuestContentType::PlainText,
            "demo.local",
        );
    }

    #[test]
    fn guest_get_response_sanitizes_slash_only_uri_to_root() {
        let state = initialized_query_state();
        let response = state.guest_get_response("////", MmdsOutputFormat::Json);

        assert_eq!(response.status(), MmdsGuestStatus::Ok);
        assert_eq!(
            response.content_type(),
            MmdsGuestContentType::ApplicationJson
        );
        assert_json_value(response.body(), query_value());
    }

    #[test]
    fn guest_get_response_maps_uninitialized_store() {
        let state = MmdsState::default();

        assert_guest_response(
            state.guest_get_response("/", MmdsOutputFormat::Json),
            MmdsGuestStatus::BadRequest,
            MmdsGuestContentType::PlainText,
            "The MMDS data store is not initialized.",
        );
    }

    #[test]
    fn guest_get_response_does_not_mutate_data_store() {
        let state = initialized_query_state();
        let original = state.get_data().expect("data store should be initialized");

        assert_guest_response(
            state.guest_get_response("/meta-data", MmdsOutputFormat::Imds),
            MmdsGuestStatus::Ok,
            MmdsGuestContentType::PlainText,
            "ami-id\nhostname",
        );
        assert_eq!(state.get_data(), Ok(original));
    }

    #[test]
    fn guest_response_http_bytes_serialize_json_success() {
        let state = initialized_query_state();
        let response = state.guest_get_response("/meta-data/hostname", MmdsOutputFormat::Json);

        assert_eq!(
            response.to_http_bytes(),
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 12\r\n\r\n\"demo.local\""
                .to_vec()
        );
    }

    #[test]
    fn guest_response_http_bytes_serialize_imds_success() {
        let state = initialized_query_state();
        let response = state.guest_get_response("/meta-data/hostname", MmdsOutputFormat::Imds);

        assert_eq!(
            response.to_http_bytes(),
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 10\r\n\r\ndemo.local"
                .to_vec()
        );
    }

    #[test]
    fn guest_response_http_bytes_serialize_not_found_error() {
        let state = initialized_query_state();
        let response = state.guest_get_response("/missing", MmdsOutputFormat::Json);

        assert_eq!(
            response.to_http_bytes(),
            b"HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: 29\r\n\r\nResource not found: /missing."
                .to_vec()
        );
    }

    #[test]
    fn guest_response_http_bytes_serialize_not_implemented_error() {
        let state = initialized_query_state();
        let response = state.guest_get_response("/age", MmdsOutputFormat::Imds);

        assert_eq!(
            response.to_http_bytes(),
            b"HTTP/1.1 501 Not Implemented\r\nContent-Type: text/plain\r\nContent-Length: 57\r\n\r\nCannot retrieve value. The value has an unsupported type."
                .to_vec()
        );
    }

    #[test]
    fn guest_response_http_bytes_use_body_byte_length() {
        let response = MmdsGuestResponse::new(
            MmdsGuestStatus::Ok,
            MmdsGuestContentType::PlainText,
            "héllo".to_string(),
        );

        assert_eq!(
            response.to_http_bytes(),
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 6\r\n\r\nh\xc3\xa9llo"
                .to_vec()
        );
    }

    #[test]
    fn guest_response_http_bytes_allow_empty_body() {
        let response = MmdsGuestResponse::new(
            MmdsGuestStatus::Ok,
            MmdsGuestContentType::PlainText,
            String::new(),
        );

        assert_eq!(
            response.to_http_bytes(),
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 0\r\n\r\n".to_vec()
        );
    }

    #[test]
    fn guest_response_http_bytes_do_not_mutate_response_or_data_store() {
        let state = initialized_query_state();
        let original = state.get_data().expect("data store should be initialized");
        let response = state.guest_get_response("/meta-data/hostname", MmdsOutputFormat::Imds);
        let first_bytes = response.to_http_bytes();

        assert_eq!(response.status(), MmdsGuestStatus::Ok);
        assert_eq!(response.content_type(), MmdsGuestContentType::PlainText);
        assert_eq!(response.body(), "demo.local");
        assert_eq!(response.to_http_bytes(), first_bytes);
        assert_eq!(state.get_data(), Ok(original));
    }
}
