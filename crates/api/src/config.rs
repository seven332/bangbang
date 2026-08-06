//! Duplicate-safe Firecracker configuration-document parsing.

use std::fmt;

use crate::http::{ApiRequest, CpuConfigRequest, RequestError, parse_request_with_limit};
use crate::json::parse_value_from_str;

/// One supported top-level Firecracker configuration-file section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSection {
    Balloon,
    BootSource,
    CpuConfig,
    Drives,
    Entropy,
    Logger,
    MachineConfig,
    MemoryHotplug,
    Metrics,
    MmdsConfig,
    NetworkInterfaces,
    Pmem,
    Serial,
    Vsock,
}

impl ConfigSection {
    /// Return the stable Firecracker configuration-file spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Balloon => "balloon",
            Self::BootSource => "boot-source",
            Self::CpuConfig => "cpu-config",
            Self::Drives => "drives",
            Self::Entropy => "entropy",
            Self::Logger => "logger",
            Self::MachineConfig => "machine-config",
            Self::MemoryHotplug => "memory-hotplug",
            Self::Metrics => "metrics",
            Self::MmdsConfig => "mmds-config",
            Self::NetworkInterfaces => "network-interfaces",
            Self::Pmem => "pmem",
            Self::Serial => "serial",
            Self::Vsock => "vsock",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        Some(match value {
            "balloon" => Self::Balloon,
            "boot-source" => Self::BootSource,
            "cpu-config" => Self::CpuConfig,
            "drives" => Self::Drives,
            "entropy" => Self::Entropy,
            "logger" => Self::Logger,
            "machine-config" => Self::MachineConfig,
            "memory-hotplug" => Self::MemoryHotplug,
            "metrics" => Self::Metrics,
            "mmds-config" => Self::MmdsConfig,
            "network-interfaces" => Self::NetworkInterfaces,
            "pmem" => Self::Pmem,
            "serial" => Self::Serial,
            "vsock" => Self::Vsock,
            _ => return None,
        })
    }
}

impl fmt::Display for ConfigSection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One strictly parsed API request and its source configuration section.
#[derive(Clone, PartialEq, Eq)]
pub struct ConfigRequest {
    section: ConfigSection,
    request: ApiRequest,
}

impl fmt::Debug for ConfigRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfigRequest")
            .field("section", &self.section)
            .field("request", &"<redacted>")
            .finish()
    }
}

impl ConfigRequest {
    /// Return the source configuration section.
    pub const fn section(&self) -> ConfigSection {
        self.section
    }

    /// Return the parsed API request.
    pub const fn request(&self) -> &ApiRequest {
        &self.request
    }

    /// Consume the wrapper into its source section and API request.
    pub fn into_parts(self) -> (ConfigSection, ApiRequest) {
        (self.section, self.request)
    }
}

/// Failure while parsing a complete Firecracker configuration document.
#[derive(Clone, PartialEq, Eq)]
pub enum ConfigDocumentError {
    Malformed,
    MissingSection(ConfigSection),
    UnknownSection(String),
    MalformedSection(ConfigSection),
    Request {
        section: ConfigSection,
        source: RequestError,
    },
}

impl ConfigDocumentError {
    /// Return an unknown section spelling for the production compatibility
    /// adapter without exposing it through generic diagnostics.
    pub fn unknown_section(&self) -> Option<&str> {
        match self {
            Self::UnknownSection(section) => Some(section),
            Self::Malformed
            | Self::MissingSection(_)
            | Self::MalformedSection(_)
            | Self::Request { .. } => None,
        }
    }
}

impl fmt::Debug for ConfigDocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed => formatter.write_str("Malformed"),
            Self::MissingSection(section) => formatter
                .debug_tuple("MissingSection")
                .field(section)
                .finish(),
            Self::UnknownSection(_) => formatter
                .debug_tuple("UnknownSection")
                .field(&"<redacted>")
                .finish(),
            Self::MalformedSection(section) => formatter
                .debug_tuple("MalformedSection")
                .field(section)
                .finish(),
            Self::Request { section, source } => formatter
                .debug_struct("Request")
                .field("section", section)
                .field("source", source)
                .finish(),
        }
    }
}

impl fmt::Display for ConfigDocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed => formatter.write_str("malformed configuration document"),
            Self::MissingSection(section) => {
                write!(
                    formatter,
                    "missing required configuration section: {section}"
                )
            }
            Self::UnknownSection(_) => formatter.write_str("unknown configuration section"),
            Self::MalformedSection(section) => {
                write!(formatter, "malformed configuration section: {section}")
            }
            Self::Request { section, source } => write!(
                formatter,
                "invalid configuration section {section}: {}",
                source.fault_message()
            ),
        }
    }
}

impl std::error::Error for ConfigDocumentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Request { source, .. } => Some(source),
            Self::Malformed
            | Self::MissingSection(_)
            | Self::UnknownSection(_)
            | Self::MalformedSection(_) => None,
        }
    }
}

/// Value-free failure while parsing arbitrary duplicate-safe JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonDocumentError;

impl fmt::Display for JsonDocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("malformed JSON document")
    }
}

impl std::error::Error for JsonDocumentError {}

/// Parse JSON while rejecting duplicate keys at every object depth.
pub fn parse_json_document_without_duplicate_keys(
    contents: &str,
) -> Result<serde_json::Value, JsonDocumentError> {
    parse_value_from_str(contents).map_err(|_| JsonDocumentError)
}

/// Value-free failure while parsing a standalone custom CPU template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CpuConfigDocumentError {
    Malformed,
    Request(RequestError),
}

impl fmt::Display for CpuConfigDocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed => formatter.write_str("malformed CPU configuration document"),
            Self::Request(source) => write!(
                formatter,
                "invalid CPU configuration document: {}",
                source.fault_message()
            ),
        }
    }
}

impl std::error::Error for CpuConfigDocumentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Request(source) => Some(source),
            Self::Malformed => None,
        }
    }
}

/// Parse a complete Firecracker configuration document into strict API
/// requests in production startup order.
pub fn parse_config_document(contents: &str) -> Result<Vec<ConfigRequest>, ConfigDocumentError> {
    let value = parse_value_from_str(contents).map_err(|_| ConfigDocumentError::Malformed)?;
    let object = value.as_object().ok_or(ConfigDocumentError::Malformed)?;

    validate_sections(object)?;

    let mut requests = Vec::new();
    if let Some(machine_config) = object.get(ConfigSection::MachineConfig.as_str()) {
        requests.push(section_request(
            ConfigSection::MachineConfig,
            "PUT",
            "/machine-config".to_string(),
            machine_config,
        )?);
    }

    let boot_source = object.get(ConfigSection::BootSource.as_str()).ok_or(
        ConfigDocumentError::MissingSection(ConfigSection::BootSource),
    )?;
    requests.push(section_request(
        ConfigSection::BootSource,
        "PUT",
        "/boot-source".to_string(),
        boot_source,
    )?);

    if let Some(drives) = object.get(ConfigSection::Drives.as_str()) {
        for drive in section_array(ConfigSection::Drives, drives)? {
            let drive_id = section_string_field(ConfigSection::Drives, drive, "drive_id")?;
            requests.push(section_request(
                ConfigSection::Drives,
                "PUT",
                format!("/drives/{drive_id}"),
                drive,
            )?);
        }
    }

    if let Some(pmem_devices) = object.get(ConfigSection::Pmem.as_str()) {
        for pmem in section_array(ConfigSection::Pmem, pmem_devices)? {
            let pmem_id = section_string_field(ConfigSection::Pmem, pmem, "id")?;
            requests.push(section_request(
                ConfigSection::Pmem,
                "PUT",
                format!("/pmem/{pmem_id}"),
                pmem,
            )?);
        }
    }

    if let Some(network_interfaces) = object.get(ConfigSection::NetworkInterfaces.as_str()) {
        for network_interface in
            section_array(ConfigSection::NetworkInterfaces, network_interfaces)?
        {
            let iface_id = section_string_field(
                ConfigSection::NetworkInterfaces,
                network_interface,
                "iface_id",
            )?;
            requests.push(section_request(
                ConfigSection::NetworkInterfaces,
                "PUT",
                format!("/network-interfaces/{iface_id}"),
                network_interface,
            )?);
        }
    }

    push_optional_request(
        object,
        &mut requests,
        ConfigSection::MmdsConfig,
        "/mmds/config",
    )?;
    push_optional_request(object, &mut requests, ConfigSection::Vsock, "/vsock")?;
    push_optional_request(object, &mut requests, ConfigSection::Entropy, "/entropy")?;
    push_optional_request(
        object,
        &mut requests,
        ConfigSection::MemoryHotplug,
        "/hotplug/memory",
    )?;
    push_optional_request(object, &mut requests, ConfigSection::Balloon, "/balloon")?;
    push_optional_request(
        object,
        &mut requests,
        ConfigSection::CpuConfig,
        "/cpu-config",
    )?;
    push_optional_request(object, &mut requests, ConfigSection::Metrics, "/metrics")?;
    push_optional_request(object, &mut requests, ConfigSection::Logger, "/logger")?;
    push_optional_request(object, &mut requests, ConfigSection::Serial, "/serial")?;

    Ok(requests)
}

/// Parse one standalone duplicate-safe custom CPU-template document through
/// the strict `/cpu-config` request shape.
pub fn parse_cpu_config_document(
    contents: &str,
) -> Result<CpuConfigRequest, CpuConfigDocumentError> {
    let value = parse_value_from_str(contents).map_err(|_| CpuConfigDocumentError::Malformed)?;
    let request = section_request(
        ConfigSection::CpuConfig,
        "PUT",
        "/cpu-config".to_string(),
        &value,
    )
    .map_err(|error| match error {
        ConfigDocumentError::Request { source, .. } => CpuConfigDocumentError::Request(source),
        ConfigDocumentError::Malformed
        | ConfigDocumentError::MissingSection(_)
        | ConfigDocumentError::UnknownSection(_)
        | ConfigDocumentError::MalformedSection(_) => CpuConfigDocumentError::Malformed,
    })?;

    match request.request {
        ApiRequest::PutCpuConfig(config) => Ok(*config),
        _ => Err(CpuConfigDocumentError::Malformed),
    }
}

fn validate_sections(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), ConfigDocumentError> {
    for section in object.keys() {
        if ConfigSection::from_str(section).is_none() {
            return Err(ConfigDocumentError::UnknownSection(section.clone()));
        }
    }
    Ok(())
}

fn section_array(
    section: ConfigSection,
    value: &serde_json::Value,
) -> Result<&[serde_json::Value], ConfigDocumentError> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or(ConfigDocumentError::MalformedSection(section))
}

fn section_string_field<'value>(
    section: ConfigSection,
    value: &'value serde_json::Value,
    field: &str,
) -> Result<&'value str, ConfigDocumentError> {
    value
        .as_object()
        .and_then(|object| object.get(field))
        .and_then(serde_json::Value::as_str)
        .ok_or(ConfigDocumentError::MalformedSection(section))
}

fn push_optional_request(
    object: &serde_json::Map<String, serde_json::Value>,
    requests: &mut Vec<ConfigRequest>,
    section: ConfigSection,
    path: &str,
) -> Result<(), ConfigDocumentError> {
    if let Some(body) = object.get(section.as_str()) {
        requests.push(section_request(section, "PUT", path.to_string(), body)?);
    }
    Ok(())
}

fn section_request(
    section: ConfigSection,
    method: &str,
    path: String,
    body: &serde_json::Value,
) -> Result<ConfigRequest, ConfigDocumentError> {
    let body =
        serde_json::to_vec(body).map_err(|_| ConfigDocumentError::MalformedSection(section))?;
    let header = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let mut request = header.into_bytes();
    request.extend_from_slice(&body);

    parse_request_with_limit(&request, usize::MAX)
        .map(|request| ConfigRequest { section, request })
        .map_err(|source| ConfigDocumentError::Request { section, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOOT_SOURCE: &str = r#"{"boot-source":{"kernel_image_path":"kernel"}}"#;

    #[test]
    fn parses_minimal_document() {
        let requests = parse_config_document(BOOT_SOURCE).expect("minimal config should parse");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].section(), ConfigSection::BootSource);
        assert!(matches!(
            requests[0].request(),
            ApiRequest::PutBootSource(_)
        ));
        assert!(!format!("{:?}", requests[0]).contains("kernel"));
    }

    #[test]
    fn rejects_duplicate_keys_at_every_depth() {
        for contents in [
            r#"{"boot-source":{"kernel_image_path":"a"},"boot-source":{"kernel_image_path":"b"}}"#,
            r#"{"boot-source":{"kernel_image_path":"a","kernel_image_path":"b"}}"#,
            r#"{"boot-source":{"kernel_image_path":"a"},"drives":[{"drive_id":"d","drive_id":"e","path_on_host":"p","is_root_device":true,"is_read_only":true}]}"#,
        ] {
            assert_eq!(
                parse_config_document(contents),
                Err(ConfigDocumentError::Malformed)
            );
        }
    }

    #[test]
    fn rejects_missing_and_unknown_sections_in_existing_order() {
        assert_eq!(
            parse_config_document("{}"),
            Err(ConfigDocumentError::MissingSection(
                ConfigSection::BootSource
            ))
        );
        let error = parse_config_document(r#"{"unknown":{},"boot-source":{}}"#)
            .expect_err("unknown section should fail before the required section is parsed");
        assert_eq!(error.unknown_section(), Some("unknown"));
        assert_eq!(format!("{error:?}"), "UnknownSection(\"<redacted>\")");
        assert_eq!(error.to_string(), "unknown configuration section");
    }

    #[test]
    fn standalone_cpu_document_is_duplicate_safe_and_strict() {
        let parsed = parse_cpu_config_document(
            r#"{"reg_modifiers":[{"addr":"0x6030000000100000","bitmap":"0b1"}]}"#,
        )
        .expect("strict CPU document should parse");
        assert_eq!(parsed.reg_modifiers().len(), 1);

        assert_eq!(
            parse_cpu_config_document(
                r#"{"reg_modifiers":[],"reg_modifiers":[],"kvm_capabilities":[],"vcpu_features":[]}"#,
            ),
            Err(CpuConfigDocumentError::Malformed)
        );
        assert!(matches!(
            parse_cpu_config_document(r#"{"unknown":[]}"#),
            Err(CpuConfigDocumentError::Request(
                RequestError::MalformedRequest
            ))
        ));
    }
}
