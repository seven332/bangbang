//! Backend-neutral MMDS control-plane input and metadata query model.

use std::fmt;
use std::net::Ipv4Addr;
use std::str;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};

pub use crate::mmds_token::{
    MMDS_TOKEN_MAX_TTL_SECONDS, MMDS_TOKEN_MIN_TTL_SECONDS, MmdsTokenAuthority, MmdsTokenError,
};
use crate::network::NetworkInterfaceConfig;

pub const MMDS_DATA_STORE_LIMIT_BYTES: usize = 51_200;
pub const MMDS_GUEST_TCP_PORT: u16 = 80;
pub const DEFAULT_MMDS_IPV4_ADDRESS: Ipv4Addr = Ipv4Addr::new(169, 254, 169, 254);
pub const DEFAULT_MMDS_MAC_ADDRESS: EthernetMacAddress =
    EthernetMacAddress::from_octets([0x06, 0x01, 0x23, 0x45, 0x67, 0x01]);

const ETHERNET_MAC_ADDRESS_LEN: usize = 6;
const MMDS_GUEST_ALLOW_METHODS: &str = "GET, PUT";
const MMDS_GUEST_INVALID_TOKEN: &str = "MMDS token not valid.";
const MMDS_GUEST_MISSING_TOKEN: &str = "No MMDS token provided. Use `X-metadata-token` or `X-aws-ec2-metadata-token` header to specify the session token.";
const MMDS_GUEST_TOKEN_PATH: &str = "/latest/api/token";
const MMDS_GUEST_X_AWS_EC2_METADATA_TOKEN: &str = "X-aws-ec2-metadata-token";
const MMDS_GUEST_X_AWS_EC2_METADATA_TOKEN_TTL_SECONDS: &str =
    "X-aws-ec2-metadata-token-ttl-seconds";
const MMDS_GUEST_X_FORWARDED_FOR: &str = "X-Forwarded-For";
const MMDS_GUEST_X_METADATA_TOKEN: &str = "X-metadata-token";
const MMDS_GUEST_X_METADATA_TOKEN_TTL_SECONDS: &str = "X-metadata-token-ttl-seconds";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EthernetMacAddress {
    octets: [u8; ETHERNET_MAC_ADDRESS_LEN],
}

impl EthernetMacAddress {
    pub const fn from_octets(octets: [u8; ETHERNET_MAC_ADDRESS_LEN]) -> Self {
        Self { octets }
    }

    pub const fn octets(self) -> [u8; ETHERNET_MAC_ADDRESS_LEN] {
        self.octets
    }
}

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

    pub fn effective_ipv4_address(&self) -> Ipv4Addr {
        self.ipv4_address.unwrap_or(DEFAULT_MMDS_IPV4_ADDRESS)
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MmdsGuestHttpVersion {
    Http10,
    #[default]
    Http11,
}

impl MmdsGuestHttpVersion {
    fn parse(version: &str) -> Result<Self, MmdsGuestRequestParseError> {
        match version {
            "HTTP/1.0" => Ok(Self::Http10),
            "HTTP/1.1" => Ok(Self::Http11),
            _ => Err(MmdsGuestRequestParseError::UnsupportedHttpVersion),
        }
    }

    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Http10 => "HTTP/1.0",
            Self::Http11 => "HTTP/1.1",
        }
    }
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

    pub const fn http_version(&self) -> MmdsGuestHttpVersion {
        match self {
            Self::Get(request) => request.http_version(),
            Self::TokenPut(request) => request.http_version(),
        }
    }

    pub fn parse_http(bytes: &[u8]) -> Result<Self, MmdsGuestRequestParseError> {
        Self::parse_http_with_version(bytes).map_err(MmdsGuestRequestParseFailure::into_error)
    }

    fn parse_http_with_version(bytes: &[u8]) -> Result<Self, MmdsGuestRequestParseFailure> {
        let request = str::from_utf8(bytes).map_err(|_| {
            MmdsGuestRequestParseFailure::without_version(MmdsGuestRequestParseError::InvalidUtf8)
        })?;
        let (head, body) = request
            .split_once("\r\n\r\n")
            .ok_or(MmdsGuestRequestParseError::MalformedRequest)
            .map_err(MmdsGuestRequestParseFailure::without_version)?;
        let mut lines = head.split("\r\n");
        let request_line = lines
            .next()
            .ok_or(MmdsGuestRequestParseError::MalformedRequest)
            .map_err(MmdsGuestRequestParseFailure::without_version)?;
        let (method, uri, version) = parse_guest_request_line(request_line)
            .map_err(MmdsGuestRequestParseFailure::without_version)?;
        let http_version = MmdsGuestHttpVersion::parse(version);
        let method = MmdsGuestRequestMethod::parse(method).map_err(|err| {
            if let Ok(http_version) = http_version {
                MmdsGuestRequestParseFailure::with_version(http_version, err)
            } else {
                MmdsGuestRequestParseFailure::without_version(err)
            }
        })?;
        let http_version = http_version.map_err(MmdsGuestRequestParseFailure::without_version)?;

        let uri = guest_request_uri_path(uri)
            .map_err(|err| MmdsGuestRequestParseFailure::with_version(http_version, err))?;
        let mut content_length = None;
        let mut output_format = MmdsOutputFormat::Imds;
        let mut token = MmdsGuestToken::Missing;
        let mut token_ttl = MmdsGuestTokenTtl::Missing;
        let mut forwarded_for = false;

        for line in lines {
            let (name, value) = parse_guest_request_header(line)
                .map_err(|err| MmdsGuestRequestParseFailure::with_version(http_version, err))?;
            if name.eq_ignore_ascii_case("Content-Length") {
                if content_length.is_some() {
                    return Err(MmdsGuestRequestParseFailure::with_version(
                        http_version,
                        MmdsGuestRequestParseError::DuplicateContentLength,
                    ));
                }
                content_length = Some(parse_guest_content_length(value).map_err(|err| {
                    MmdsGuestRequestParseFailure::with_version(http_version, err)
                })?);
            } else if name.eq_ignore_ascii_case("Transfer-Encoding") {
                return Err(MmdsGuestRequestParseFailure::with_version(
                    http_version,
                    MmdsGuestRequestParseError::UnsupportedTransferEncoding,
                ));
            } else if method == MmdsGuestRequestMethod::Get && name.eq_ignore_ascii_case("Accept") {
                output_format = parse_guest_accept_header(value)
                    .map_err(|err| MmdsGuestRequestParseFailure::with_version(http_version, err))?;
            } else if method == MmdsGuestRequestMethod::Get {
                if let Some(header) = MmdsGuestTokenHeader::parse_name(name) {
                    token = match token {
                        MmdsGuestToken::Missing => MmdsGuestToken::Header {
                            token_header: header,
                            token_value: value.to_string(),
                        },
                        MmdsGuestToken::Header { .. } | MmdsGuestToken::Duplicate => {
                            MmdsGuestToken::Duplicate
                        }
                    };
                }
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
            return Err(MmdsGuestRequestParseFailure::with_version(
                http_version,
                MmdsGuestRequestParseError::UnsupportedBody,
            ));
        }

        match method {
            MmdsGuestRequestMethod::Get => Ok(Self::Get(MmdsGuestGetRequest {
                http_version,
                uri: uri.to_string(),
                output_format,
                token,
            })),
            MmdsGuestRequestMethod::Put => {
                if forwarded_for {
                    return Err(MmdsGuestRequestParseFailure::with_version(
                        http_version,
                        MmdsGuestRequestParseError::UnsupportedForwardedFor,
                    ));
                }

                Ok(Self::TokenPut(MmdsGuestTokenPutRequest {
                    http_version,
                    uri: uri.to_string(),
                    token_ttl,
                }))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MmdsGuestRequestParseFailure {
    error: MmdsGuestRequestParseError,
    http_version: Option<MmdsGuestHttpVersion>,
}

impl MmdsGuestRequestParseFailure {
    const fn without_version(error: MmdsGuestRequestParseError) -> Self {
        Self {
            error,
            http_version: None,
        }
    }

    const fn with_version(
        http_version: MmdsGuestHttpVersion,
        error: MmdsGuestRequestParseError,
    ) -> Self {
        Self {
            error,
            http_version: Some(http_version),
        }
    }

    const fn into_error(self) -> MmdsGuestRequestParseError {
        self.error
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmdsGuestGetRequest {
    http_version: MmdsGuestHttpVersion,
    uri: String,
    output_format: MmdsOutputFormat,
    token: MmdsGuestToken,
}

impl MmdsGuestGetRequest {
    pub const fn http_version(&self) -> MmdsGuestHttpVersion {
        self.http_version
    }

    pub fn uri(&self) -> &str {
        &self.uri
    }

    pub const fn output_format(&self) -> MmdsOutputFormat {
        self.output_format
    }

    pub fn token(&self) -> &MmdsGuestToken {
        &self.token
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmdsGuestTokenPutRequest {
    http_version: MmdsGuestHttpVersion,
    uri: String,
    token_ttl: MmdsGuestTokenTtl,
}

impl MmdsGuestTokenPutRequest {
    pub const fn http_version(&self) -> MmdsGuestHttpVersion {
        self.http_version
    }

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

#[derive(Clone, PartialEq, Eq)]
pub enum MmdsGuestToken {
    Missing,
    Header {
        token_header: MmdsGuestTokenHeader,
        token_value: String,
    },
    Duplicate,
}

impl fmt::Debug for MmdsGuestToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => f.write_str("Missing"),
            Self::Header { token_header, .. } => f
                .debug_struct("Header")
                .field("token_header", token_header)
                .field("token_value", &"[REDACTED]")
                .finish(),
            Self::Duplicate => f.write_str("Duplicate"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmdsGuestTokenHeader {
    Metadata,
    AwsEc2Metadata,
}

impl MmdsGuestTokenHeader {
    fn parse_name(name: &str) -> Option<Self> {
        if name.eq_ignore_ascii_case(MMDS_GUEST_X_METADATA_TOKEN) {
            return Some(Self::Metadata);
        }
        if name.eq_ignore_ascii_case(MMDS_GUEST_X_AWS_EC2_METADATA_TOKEN) {
            return Some(Self::AwsEc2Metadata);
        }

        None
    }
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
    MissingToken,
    InvalidToken,
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
            Self::MissingToken => f.write_str(MMDS_GUEST_MISSING_TOKEN),
            Self::InvalidToken => f.write_str(MMDS_GUEST_INVALID_TOKEN),
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
    Unauthorized,
    NotFound,
    MethodNotAllowed,
    NotImplemented,
}

impl MmdsGuestStatus {
    pub const fn as_u16(&self) -> u16 {
        match self {
            Self::Ok => 200,
            Self::BadRequest => 400,
            Self::Unauthorized => 401,
            Self::NotFound => 404,
            Self::MethodNotAllowed => 405,
            Self::NotImplemented => 501,
        }
    }

    pub const fn reason_phrase(&self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::BadRequest => "Bad Request",
            Self::Unauthorized => "Unauthorized",
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

#[derive(Clone, PartialEq, Eq)]
pub struct MmdsGuestResponse {
    http_version: MmdsGuestHttpVersion,
    status: MmdsGuestStatus,
    content_type: MmdsGuestContentType,
    allow: Option<&'static str>,
    custom_headers: Vec<(&'static str, String)>,
    body: String,
}

impl fmt::Debug for MmdsGuestResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MmdsGuestResponse")
            .field("http_version", &self.http_version)
            .field("status", &self.status)
            .field("content_type", &self.content_type)
            .field("allow", &self.allow)
            .field("custom_headers", &"[REDACTED]")
            .field("body", &"[REDACTED]")
            .field("body_len", &self.body.len())
            .finish()
    }
}

impl MmdsGuestResponse {
    fn new(status: MmdsGuestStatus, content_type: MmdsGuestContentType, body: String) -> Self {
        Self {
            http_version: MmdsGuestHttpVersion::default(),
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

    fn with_http_version(mut self, http_version: MmdsGuestHttpVersion) -> Self {
        self.http_version = http_version;
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
            "{} {} {}\r\nContent-Type: {}\r\n",
            self.http_version.as_str(),
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

pub struct MmdsState {
    config: Option<MmdsConfig>,
    data_store_present: bool,
    value: Option<Value>,
    data_store_limit_bytes: usize,
    token_authority: MmdsTokenAuthority,
}

impl fmt::Debug for MmdsState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MmdsState")
            .field("config", &self.config)
            .field("data_store_present", &self.data_store_present)
            .field("value", &"[REDACTED]")
            .field("data_store_limit_bytes", &self.data_store_limit_bytes)
            .field("token_authority", &self.token_authority)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct MmdsStateHandle {
    state: Arc<Mutex<MmdsState>>,
}

impl Default for MmdsStateHandle {
    fn default() -> Self {
        Self::new(MmdsState::default())
    }
}

impl MmdsStateHandle {
    pub fn new(state: MmdsState) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }

    pub fn with<R>(&self, f: impl FnOnce(&MmdsState) -> R) -> Result<R, MmdsStateLockError> {
        let state = self.state.lock().map_err(|_| MmdsStateLockError)?;
        Ok(f(&state))
    }

    pub fn with_mut<R>(
        &self,
        f: impl FnOnce(&mut MmdsState) -> R,
    ) -> Result<R, MmdsStateLockError> {
        let mut state = self.state.lock().map_err(|_| MmdsStateLockError)?;
        Ok(f(&mut state))
    }

    pub fn config(&self) -> Result<Option<MmdsConfig>, MmdsStateLockError> {
        self.with(|state| state.config().cloned())
    }

    #[doc(hidden)]
    pub fn shares_state_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MmdsStateLockError;

impl fmt::Display for MmdsStateLockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MMDS state lock is poisoned")
    }
}

impl std::error::Error for MmdsStateLockError {}

impl Default for MmdsState {
    fn default() -> Self {
        Self::new(MMDS_DATA_STORE_LIMIT_BYTES)
    }
}

impl MmdsState {
    pub fn new(data_store_limit_bytes: usize) -> Self {
        Self::with_instance_id(data_store_limit_bytes, "anonymous")
    }

    pub fn with_instance_id(data_store_limit_bytes: usize, instance_id: impl AsRef<str>) -> Self {
        Self {
            config: None,
            data_store_present: false,
            value: None,
            data_store_limit_bytes,
            token_authority: MmdsTokenAuthority::new(instance_id),
        }
    }

    pub const fn data_store_limit_bytes(&self) -> usize {
        self.data_store_limit_bytes
    }

    pub fn config(&self) -> Option<&MmdsConfig> {
        self.config.as_ref()
    }

    pub const fn data_store_present(&self) -> bool {
        self.data_store_present
    }

    #[cfg(test)]
    pub(crate) fn token_authority_is_bound_to_instance_id(&self, instance_id: &str) -> bool {
        self.token_authority.is_bound_to_instance_id(instance_id)
    }

    pub(crate) fn ensure_data_store_present(&mut self) {
        self.data_store_present = true;
    }

    pub fn put_config(
        &mut self,
        input: MmdsConfigInput,
        configured_network_interfaces: &[NetworkInterfaceConfig],
    ) -> Result<(), MmdsConfigError> {
        let config = input.validate(configured_network_interfaces)?;
        self.ensure_data_store_present();
        self.config = Some(config);
        Ok(())
    }

    pub fn get_data(&self) -> Result<Value, MmdsDataStoreError> {
        self.value
            .as_ref()
            .cloned()
            .ok_or(MmdsDataStoreError::NotInitialized)
    }

    pub fn get_data_or_null(&self) -> Value {
        self.value.as_ref().cloned().unwrap_or(Value::Null)
    }

    pub(crate) fn get_or_create_data_store_value(&mut self) -> Value {
        self.ensure_data_store_present();
        self.get_data_or_null()
    }

    pub(crate) fn get_existing_data_store_value(&self) -> Result<Value, MmdsDataStoreError> {
        if !self.data_store_present {
            return Err(MmdsDataStoreError::NotInitialized);
        }

        Ok(self.get_data_or_null())
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
        match MmdsGuestRequest::parse_http_with_version(request_bytes) {
            Ok(MmdsGuestRequest::Get(request)) => self
                .guest_get_http_response(&request)
                .with_http_version(request.http_version()),
            Ok(MmdsGuestRequest::TokenPut(request)) => self
                .guest_token_put_response(&request)
                .with_http_version(request.http_version()),
            Err(failure) => {
                let response = guest_request_parse_error_response(failure.error);
                if let Some(http_version) = failure.http_version {
                    response.with_http_version(http_version)
                } else {
                    response
                }
            }
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
        self.ensure_data_store_present();
        self.value = Some(value);
        Ok(())
    }

    pub(crate) fn put_existing_data_store(
        &mut self,
        input: MmdsContentInput,
    ) -> Result<(), MmdsDataStoreError> {
        if !self.data_store_present {
            return Err(MmdsDataStoreError::NotInitialized);
        }

        self.put_data(input)
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

    pub(crate) fn patch_existing_data_store(
        &mut self,
        input: MmdsContentInput,
    ) -> Result<(), MmdsDataStoreError> {
        if !self.data_store_present {
            return Err(MmdsDataStoreError::NotInitialized);
        }

        self.patch_data(input)
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

    fn guest_mmds_version(&self) -> MmdsVersion {
        self.config
            .as_ref()
            .map_or(MmdsVersion::V1, MmdsConfig::version)
    }

    fn guest_get_http_response(&self, request: &MmdsGuestGetRequest) -> MmdsGuestResponse {
        if self.guest_mmds_version() == MmdsVersion::V2 {
            match request.token() {
                MmdsGuestToken::Missing => {
                    return guest_request_parse_error_response(
                        MmdsGuestRequestParseError::MissingToken,
                    );
                }
                MmdsGuestToken::Header { token_value, .. }
                    if self.is_guest_token_valid(token_value) => {}
                MmdsGuestToken::Header { .. } | MmdsGuestToken::Duplicate => {
                    return guest_request_parse_error_response(
                        MmdsGuestRequestParseError::InvalidToken,
                    );
                }
            }
        }

        self.guest_get_response(request.uri(), request.output_format())
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
        MmdsGuestRequestParseError::MissingToken | MmdsGuestRequestParseError::InvalidToken => {
            MmdsGuestStatus::Unauthorized
        }
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
    use base64::Engine as _;

    use crate::network::NetworkInterfaceConfigInput;

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

    fn enable_mmds_v1(state: &mut MmdsState) {
        state.config = Some(MmdsConfig {
            network_interfaces: vec!["eth0".to_string()],
            version: MmdsVersion::V1,
            ipv4_address: None,
            imds_compat: false,
        });
    }

    fn enable_mmds_v2(state: &mut MmdsState) {
        state.config = Some(MmdsConfig {
            network_interfaces: vec!["eth0".to_string()],
            version: MmdsVersion::V2,
            ipv4_address: None,
            imds_compat: false,
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
        assert_eq!(request.token(), &MmdsGuestToken::Missing);
    }

    fn assert_guest_token_get_request(
        bytes: &[u8],
        expected_uri: &str,
        expected_token_header: MmdsGuestTokenHeader,
        expected_token_value: &str,
    ) {
        let request =
            MmdsGuestRequest::parse_http(bytes).expect("test MMDS guest HTTP request should parse");
        let MmdsGuestRequest::Get(request) = request else {
            panic!("test MMDS guest HTTP request should be GET");
        };

        assert_eq!(request.uri(), expected_uri);
        assert_eq!(
            request.token(),
            &MmdsGuestToken::Header {
                token_header: expected_token_header,
                token_value: expected_token_value.to_string(),
            }
        );
    }

    fn assert_guest_token_get_duplicate(bytes: &[u8]) {
        let request =
            MmdsGuestRequest::parse_http(bytes).expect("test MMDS guest HTTP request should parse");
        let MmdsGuestRequest::Get(request) = request else {
            panic!("test MMDS guest HTTP request should be GET");
        };

        assert_eq!(request.token(), &MmdsGuestToken::Duplicate);
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
        assert_eq!(token.len(), 48);
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(token)
                .expect("test MMDS token should use standard Base64")
                .len(),
            36
        );
    }

    #[test]
    fn mmds_state_guest_token_delegates_to_token_authority() {
        let mut state = MmdsState {
            token_authority: MmdsTokenAuthority::with_manual_clock("state-instance", 1_000),
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
    fn mmds_state_guest_tokens_are_isolated_between_states() {
        let mut first = MmdsState {
            token_authority: MmdsTokenAuthority::with_manual_clock("first-instance", 1_000),
            ..MmdsState::default()
        };
        let mut second = MmdsState {
            token_authority: MmdsTokenAuthority::with_manual_clock("second-instance", 1_000),
            ..MmdsState::default()
        };
        let first_token = first
            .generate_guest_token(60)
            .expect("first state should generate a guest token");
        let second_token = second
            .generate_guest_token(60)
            .expect("second state should generate a guest token");

        assert!(first.is_guest_token_valid(&first_token));
        assert!(second.is_guest_token_valid(&second_token));
        assert!(!first.is_guest_token_valid(&second_token));
        assert!(!second.is_guest_token_valid(&first_token));
    }

    #[test]
    fn mmds_state_debug_redacts_instance_metadata_and_token_state() {
        let instance_id = "private-mmds-instance";
        let metadata_secret = "private-metadata-value";
        let mut state = MmdsState::with_instance_id(MMDS_DATA_STORE_LIMIT_BYTES, instance_id);
        state
            .put_data(MmdsContentInput::new(serde_json::json!({
                "secret": metadata_secret,
            })))
            .expect("test metadata should store");
        let token = state
            .generate_guest_token(60)
            .expect("test token should generate");
        let debug = format!("{state:?}");

        assert!(!debug.contains(instance_id));
        assert!(!debug.contains(metadata_secret));
        assert!(!debug.contains(&token));
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn mmds_guest_debug_surfaces_redact_token_and_response_values() {
        let token_value = "private-token-value-that-must-never-appear";
        let token = MmdsGuestToken::Header {
            token_header: MmdsGuestTokenHeader::Metadata,
            token_value: token_value.to_string(),
        };
        let request = MmdsGuestRequest::Get(MmdsGuestGetRequest {
            http_version: MmdsGuestHttpVersion::Http11,
            uri: "/meta-data/hostname".to_string(),
            output_format: MmdsOutputFormat::Imds,
            token: token.clone(),
        });
        let response = MmdsGuestResponse::new(
            MmdsGuestStatus::Ok,
            MmdsGuestContentType::PlainText,
            token_value.to_string(),
        )
        .with_custom_header("X-test-private", token_value);

        for debug in [
            format!("{token:?}"),
            format!("{request:?}"),
            format!("{response:?}"),
        ] {
            assert!(!debug.contains(token_value));
            assert!(debug.contains("[REDACTED]"));
        }
    }

    #[test]
    fn mmds_state_handle_shares_mutations() {
        let handle = MmdsStateHandle::default();
        let cloned = handle.clone();
        let value = query_value();

        handle
            .with_mut(|state| state.put_data(MmdsContentInput::new(value.clone())))
            .expect("MMDS handle should lock")
            .expect("MMDS data should store");

        assert_eq!(
            cloned
                .with(MmdsState::get_data)
                .expect("cloned MMDS handle should lock"),
            Ok(value)
        );
    }

    #[test]
    fn mmds_state_handle_serializes_concurrent_token_generation() {
        const THREADS: usize = 8;
        const TOKENS_PER_THREAD: usize = 16;

        let state = MmdsState {
            token_authority: MmdsTokenAuthority::with_manual_clock("concurrent-instance", 1_000),
            ..MmdsState::default()
        };
        let handle = MmdsStateHandle::new(state);
        let tokens = std::thread::scope(|scope| {
            let workers = (0..THREADS)
                .map(|_| {
                    let handle = handle.clone();
                    scope.spawn(move || {
                        (0..TOKENS_PER_THREAD)
                            .map(|_| {
                                handle
                                    .with_mut(|state| state.generate_guest_token(60))
                                    .expect("MMDS state should lock")
                                    .expect("token should generate")
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect::<Vec<_>>();

            workers
                .into_iter()
                .flat_map(|worker| worker.join().expect("token worker should not panic"))
                .collect::<Vec<_>>()
        });
        let unique_tokens = tokens.iter().collect::<std::collections::HashSet<_>>();

        assert_eq!(tokens.len(), THREADS * TOKENS_PER_THREAD);
        assert_eq!(unique_tokens.len(), tokens.len());
        assert_eq!(
            handle.with(|state| tokens.iter().all(|token| state.is_guest_token_valid(token))),
            Ok(true)
        );
    }

    #[test]
    fn get_or_create_data_store_value_marks_store_present_without_data() {
        let mut state = MmdsState::default();

        assert_eq!(state.get_or_create_data_store_value(), Value::Null);
        assert!(state.data_store_present());
        assert_eq!(state.get_data(), Err(MmdsDataStoreError::NotInitialized));
        assert_eq!(state.get_existing_data_store_value(), Ok(Value::Null));
    }

    #[test]
    fn existing_data_store_operations_require_store_presence() {
        let mut state = MmdsState::default();
        let value = query_value();

        assert_eq!(
            state.get_existing_data_store_value(),
            Err(MmdsDataStoreError::NotInitialized)
        );
        assert_eq!(
            state.put_existing_data_store(MmdsContentInput::new(value)),
            Err(MmdsDataStoreError::NotInitialized)
        );
        assert_eq!(
            state.patch_existing_data_store(MmdsContentInput::new(serde_json::json!({}))),
            Err(MmdsDataStoreError::NotInitialized)
        );
        assert!(!state.data_store_present());
    }

    #[test]
    fn mmds_config_effective_ipv4_address_uses_default_or_configured_value() {
        let mut state = MmdsState::default();
        enable_mmds_v1(&mut state);
        assert_eq!(
            state
                .config()
                .expect("MMDS config should be present")
                .effective_ipv4_address(),
            DEFAULT_MMDS_IPV4_ADDRESS
        );

        state
            .put_config(
                MmdsConfigInput::new(vec!["eth0".to_string()])
                    .with_ipv4_address(Ipv4Addr::new(169, 254, 169, 253)),
                &[NetworkInterfaceConfigInput::new("eth0", "eth0", "tap0")
                    .validate()
                    .expect("network config should validate")],
            )
            .expect("MMDS config should store");
        assert_eq!(
            state
                .config()
                .expect("MMDS config should be present")
                .effective_ipv4_address(),
            Ipv4Addr::new(169, 254, 169, 253)
        );
        assert!(state.data_store_present());
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
    fn get_data_or_null_returns_public_default_without_initializing() {
        let state = MmdsState::default();

        assert_eq!(state.get_data_or_null(), Value::Null);
        assert_eq!(state.get_data(), Err(MmdsDataStoreError::NotInitialized));
        assert!(!state.data_store_present());
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
        assert_eq!(MmdsGuestStatus::Unauthorized.as_u16(), 401);
        assert_eq!(MmdsGuestStatus::NotFound.as_u16(), 404);
        assert_eq!(MmdsGuestStatus::MethodNotAllowed.as_u16(), 405);
        assert_eq!(MmdsGuestStatus::NotImplemented.as_u16(), 501);
    }

    #[test]
    fn guest_status_reason_phrases_match_http_values() {
        assert_eq!(MmdsGuestStatus::Ok.reason_phrase(), "OK");
        assert_eq!(MmdsGuestStatus::BadRequest.reason_phrase(), "Bad Request");
        assert_eq!(
            MmdsGuestStatus::Unauthorized.reason_phrase(),
            "Unauthorized"
        );
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
    fn mmds_guest_request_preserves_supported_http_versions() {
        let request = MmdsGuestRequest::parse_http(
            b"GET /meta-data/hostname HTTP/1.0\r\nAccept: application/json\r\n\r\n",
        )
        .expect("HTTP/1.0 GET request should parse");
        assert_eq!(request.http_version(), MmdsGuestHttpVersion::Http10);
        let MmdsGuestRequest::Get(request) = request else {
            panic!("test MMDS guest HTTP request should be GET");
        };
        assert_eq!(request.http_version(), MmdsGuestHttpVersion::Http10);

        let request = MmdsGuestRequest::parse_http(
            b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 60\r\n\r\n",
        )
        .expect("HTTP/1.1 token PUT request should parse");
        assert_eq!(request.http_version(), MmdsGuestHttpVersion::Http11);
        let MmdsGuestRequest::TokenPut(request) = request else {
            panic!("test MMDS guest HTTP request should be token PUT");
        };
        assert_eq!(request.http_version(), MmdsGuestHttpVersion::Http11);
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
            MmdsGuestRequest::parse_http(b"POST /meta-data/hostname HTTP/2\r\n\r\n"),
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
    fn mmds_guest_request_parses_get_token_headers() {
        assert_guest_token_get_request(
            b"GET /meta-data/hostname HTTP/1.1\r\nX-metadata-token: token-1\r\n\r\n",
            "/meta-data/hostname",
            MmdsGuestTokenHeader::Metadata,
            "token-1",
        );
        assert_guest_token_get_request(
            b"GET /meta-data/hostname HTTP/1.1\r\nX-aws-ec2-metadata-token: token-2\r\n\r\n",
            "/meta-data/hostname",
            MmdsGuestTokenHeader::AwsEc2Metadata,
            "token-2",
        );
        assert_guest_token_get_request(
            b"GET http://169.254.169.254/meta-data/hostname HTTP/1.1\r\nx-MeTaDaTa-ToKeN: token-3\r\n\r\n",
            "/meta-data/hostname",
            MmdsGuestTokenHeader::Metadata,
            "token-3",
        );
    }

    #[test]
    fn mmds_guest_request_records_duplicate_get_token_headers() {
        assert_guest_token_get_duplicate(
            b"GET /meta-data/hostname HTTP/1.1\r\nX-metadata-token: token-1\r\nX-aws-ec2-metadata-token: token-1\r\n\r\n",
        );
        assert_guest_token_get_duplicate(
            b"GET /meta-data/hostname HTTP/1.1\r\nX-metadata-token: token-1\r\nX-metadata-token: token-2\r\n\r\n",
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
    fn mmds_guest_http_response_bytes_preserve_http_10_get_success() {
        let mut state = initialized_query_state();

        assert_eq!(
            state.guest_http_response_bytes(
                b"GET /meta-data/hostname HTTP/1.0\r\nAccept: */*\r\n\r\n"
            ),
            b"HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 10\r\n\r\ndemo.local"
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
    fn mmds_guest_http_response_bytes_preserve_http_10_token_put_success() {
        let mut state = initialized_query_state();
        let bytes = state.guest_http_response_bytes(
            b"PUT /latest/api/token HTTP/1.0\r\nX-aws-ec2-metadata-token-ttl-seconds: +60\r\n\r\n",
        );
        let response = String::from_utf8(bytes).expect("token response should be UTF-8");
        let (head, token) = response
            .split_once("\r\n\r\n")
            .expect("token response should include header terminator");

        assert_eq!(
            head,
            "HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\nX-aws-ec2-metadata-token-ttl-seconds: 60\r\nContent-Length: 48"
        );
        assert_mmds_token_shape(token);
        assert!(state.is_guest_token_valid(token));
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
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nX-aws-ec2-metadata-token-ttl-seconds: 60\r\nContent-Length: 48"
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
                b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 0\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "Invalid MMDS token TTL: 0. Please provide a value between 1 and 21600.",
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
    fn mmds_guest_http_response_bytes_preserve_http_10_errors_with_supported_version() {
        let mut state = MmdsState::default();
        assert_eq!(
            state.guest_http_response_bytes(
                b"GET /meta-data/hostname HTTP/1.0\r\nAccept: application/json\r\n\r\n"
            ),
            b"HTTP/1.0 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: 39\r\n\r\nThe MMDS data store is not initialized."
                .to_vec()
        );

        let mut state = initialized_query_state();
        assert_eq!(
            state.guest_http_response_bytes(b"POST /meta-data/hostname HTTP/1.0\r\n\r\n"),
            b"HTTP/1.0 405 Method Not Allowed\r\nContent-Type: text/plain\r\nAllow: GET, PUT\r\nContent-Length: 48\r\n\r\nMMDS guest HTTP request method is not supported."
                .to_vec()
        );

        let mut state = initialized_query_state();
        assert_eq!(
            state.guest_http_response_bytes(
                b"GET /meta-data/hostname HTTP/1.0\r\nAccept: application/xml\r\n\r\n"
            ),
            b"HTTP/1.0 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: 55\r\n\r\nMMDS guest HTTP request Accept header is not supported."
                .to_vec()
        );

        let mut state = initialized_query_state();
        assert_eq!(
            state.guest_http_response_bytes(b"PUT /latest/api/token HTTP/1.0\r\n\r\n"),
            b"HTTP/1.0 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: 152\r\n\r\nToken time to live value not found. Use `X-metadata-token-ttl-seconds` or `X-aws-ec2-metadata-token-ttl-seconds` header to specify the token's lifetime."
                .to_vec()
        );

        let mut state = initialized_query_state();
        enable_mmds_v2(&mut state);
        let expected = format!(
            "HTTP/1.0 401 Unauthorized\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
            MMDS_GUEST_MISSING_TOKEN.len(),
            MMDS_GUEST_MISSING_TOKEN
        )
        .into_bytes();
        assert_eq!(
            state.guest_http_response_bytes(b"GET /meta-data/hostname HTTP/1.0\r\n\r\n"),
            expected
        );
    }

    #[test]
    fn mmds_guest_http_response_token_put_errors_do_not_create_tokens() {
        for request in [
            b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: application/json\r\n\r\n".as_slice(),
            b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 1\r\nX-aws-ec2-metadata-token-ttl-seconds: 1\r\n\r\n",
            b"PUT /wrong HTTP/1.1\r\nX-metadata-token-ttl-seconds: 1\r\n\r\n",
        ] {
            let mut state = initialized_query_state();
            state.token_authority =
                MmdsTokenAuthority::with_manual_clock("failed-put-instance", 1_000);

            assert_ne!(
                state.guest_http_response(request).status(),
                MmdsGuestStatus::Ok
            );
            let token = state
                .generate_guest_token(1)
                .expect("failed token PUT should not initialize token state");
            assert!(state.is_guest_token_valid(&token));
        }
    }

    #[test]
    fn mmds_guest_http_response_default_get_does_not_enforce_tokens() {
        let mut state = initialized_query_state();
        let response = state.guest_http_response(
            b"GET /meta-data/hostname HTTP/1.1\r\nAccept: application/json\r\n\r\n",
        );

        assert_eq!(response.status(), MmdsGuestStatus::Ok);
        assert_eq!(response.body(), r#""demo.local""#);
    }

    #[test]
    fn mmds_guest_http_response_v1_get_does_not_enforce_tokens() {
        for request in [
            b"GET /meta-data/hostname HTTP/1.1\r\nAccept: application/json\r\n\r\n".as_slice(),
            b"GET /meta-data/hostname HTTP/1.1\r\nAccept: application/json\r\nX-metadata-token: unknown\r\n\r\n",
            b"GET /meta-data/hostname HTTP/1.1\r\nAccept: application/json\r\nX-metadata-token: unknown\r\nX-aws-ec2-metadata-token: duplicate\r\n\r\n",
        ] {
            let mut state = initialized_query_state();
            enable_mmds_v1(&mut state);

            assert_guest_response(
                state.guest_http_response(request),
                MmdsGuestStatus::Ok,
                MmdsGuestContentType::ApplicationJson,
                r#""demo.local""#,
            );
        }
    }

    #[test]
    fn mmds_guest_http_response_v2_requires_token() {
        let mut state = initialized_query_state();
        enable_mmds_v2(&mut state);

        assert_guest_response(
            state.guest_http_response(
                b"GET /meta-data/hostname HTTP/1.1\r\nAccept: application/json\r\n\r\n",
            ),
            MmdsGuestStatus::Unauthorized,
            MmdsGuestContentType::PlainText,
            MMDS_GUEST_MISSING_TOKEN,
        );
    }

    #[test]
    fn mmds_guest_http_response_v2_rejects_invalid_tokens() {
        for request in [
            b"GET /meta-data/hostname HTTP/1.1\r\nX-metadata-token: unknown\r\n\r\n".as_slice(),
            b"GET /meta-data/hostname HTTP/1.1\r\nX-aws-ec2-metadata-token: \r\n\r\n",
            b"GET /meta-data/hostname HTTP/1.1\r\nX-metadata-token: unknown\r\nX-aws-ec2-metadata-token: unknown\r\n\r\n",
        ] {
            let mut state = initialized_query_state();
            enable_mmds_v2(&mut state);

            assert_guest_response(
                state.guest_http_response(request),
                MmdsGuestStatus::Unauthorized,
                MmdsGuestContentType::PlainText,
                MMDS_GUEST_INVALID_TOKEN,
            );
        }
    }

    #[test]
    fn mmds_guest_http_response_v2_rejects_expired_token() {
        let mut state = initialized_query_state();
        enable_mmds_v2(&mut state);
        state.token_authority =
            MmdsTokenAuthority::with_manual_clock("expired-token-instance", 1_000);
        let token = state
            .generate_guest_token(1)
            .expect("test token generation should succeed");
        state.token_authority.set_now_millis(2_000);
        let request =
            format!("GET /meta-data/hostname HTTP/1.1\r\nX-metadata-token: {token}\r\n\r\n");

        assert_guest_response(
            state.guest_http_response(request.as_bytes()),
            MmdsGuestStatus::Unauthorized,
            MmdsGuestContentType::PlainText,
            MMDS_GUEST_INVALID_TOKEN,
        );
    }

    #[test]
    fn mmds_guest_http_response_v2_accepts_valid_tokens() {
        let mut state = initialized_query_state();
        enable_mmds_v2(&mut state);
        state.token_authority =
            MmdsTokenAuthority::with_manual_clock("valid-token-instance", 1_000);
        let token = state
            .generate_guest_token(1)
            .expect("test token generation should succeed");
        let request = format!(
            "GET /meta-data/hostname HTTP/1.1\r\nAccept: application/json\r\nX-aws-ec2-metadata-token: {token}\r\n\r\n"
        );

        assert_guest_response(
            state.guest_http_response(request.as_bytes()),
            MmdsGuestStatus::Ok,
            MmdsGuestContentType::ApplicationJson,
            r#""demo.local""#,
        );
    }

    #[test]
    fn mmds_guest_http_response_v2_token_errors_do_not_mutate_state() {
        let requests = [
            b"GET /meta-data/hostname HTTP/1.1\r\n\r\n".as_slice(),
            b"GET /meta-data/hostname HTTP/1.1\r\nX-metadata-token: unknown\r\n\r\n",
            b"GET /meta-data/hostname HTTP/1.1\r\nX-metadata-token: unknown\r\nX-aws-ec2-metadata-token: duplicate\r\n\r\n",
        ];

        for request in requests {
            let mut state = initialized_query_state();
            enable_mmds_v2(&mut state);
            state.token_authority =
                MmdsTokenAuthority::with_manual_clock("token-error-instance", 1_000);
            let token = state
                .generate_guest_token(1)
                .expect("test token generation should succeed");
            let original = state.get_data().expect("data store should be initialized");

            assert_eq!(
                state.guest_http_response(request).status(),
                MmdsGuestStatus::Unauthorized
            );
            assert_eq!(state.get_data(), Ok(original));
            assert!(state.is_guest_token_valid(&token));
        }
    }

    #[test]
    fn mmds_guest_http_response_v2_authenticates_before_data_lookup() {
        let mut state = MmdsState::default();
        enable_mmds_v2(&mut state);

        assert_guest_response(
            state.guest_http_response(b"GET /meta-data/hostname HTTP/1.1\r\n\r\n"),
            MmdsGuestStatus::Unauthorized,
            MmdsGuestContentType::PlainText,
            MMDS_GUEST_MISSING_TOKEN,
        );

        let token = state
            .generate_guest_token(1)
            .expect("test token generation should succeed");
        let request =
            format!("GET /meta-data/hostname HTTP/1.1\r\nX-metadata-token: {token}\r\n\r\n");

        assert_guest_response(
            state.guest_http_response(request.as_bytes()),
            MmdsGuestStatus::BadRequest,
            MmdsGuestContentType::PlainText,
            "The MMDS data store is not initialized.",
        );
    }

    #[test]
    fn mmds_guest_http_response_bytes_serialize_missing_v2_token() {
        let mut state = initialized_query_state();
        enable_mmds_v2(&mut state);
        let expected = format!(
            "HTTP/1.1 401 Unauthorized\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
            MMDS_GUEST_MISSING_TOKEN.len(),
            MMDS_GUEST_MISSING_TOKEN
        )
        .into_bytes();

        assert_eq!(
            state.guest_http_response_bytes(b"GET /meta-data/hostname HTTP/1.1\r\n\r\n"),
            expected
        );
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
    fn mmds_guest_http_response_bytes_keep_default_version_for_unsupported_version_error() {
        let mut state = initialized_query_state();

        assert_eq!(
            state.guest_http_response_bytes(b"GET /meta-data/hostname HTTP/2\r\n\r\n"),
            b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: 49\r\n\r\nMMDS guest HTTP request version is not supported."
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
            MmdsGuestRequestParseError::MissingToken.to_string(),
            MMDS_GUEST_MISSING_TOKEN
        );
        assert_eq!(
            MmdsGuestRequestParseError::InvalidToken.to_string(),
            MMDS_GUEST_INVALID_TOKEN
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
