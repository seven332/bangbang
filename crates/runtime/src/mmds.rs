//! Backend-neutral MMDS control-plane input and metadata query model.

use std::fmt;
use std::net::Ipv4Addr;

use serde_json::{Map, Value};

use crate::network::NetworkInterfaceConfig;

pub const MMDS_DATA_STORE_LIMIT_BYTES: usize = 51_200;

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
}

impl Default for MmdsState {
    fn default() -> Self {
        Self::new(MMDS_DATA_STORE_LIMIT_BYTES)
    }
}

impl MmdsState {
    pub const fn new(data_store_limit_bytes: usize) -> Self {
        Self {
            config: None,
            value: None,
            data_store_limit_bytes,
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
}

fn mmds_pointer_path(path: &str) -> &str {
    path.strip_suffix('/').unwrap_or(path)
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

    fn serialized_len(value: &Value) -> usize {
        serde_json::to_vec(value)
            .expect("test JSON value should serialize")
            .len()
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
}
