//! Backend-neutral MMDS control-plane input model.

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MmdsDataStoreError {
    NotInitialized,
    DataStoreLimitExceeded {
        limit_bytes: usize,
        size_bytes: usize,
    },
    Serialization,
}

impl fmt::Display for MmdsDataStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInitialized => f.write_str("The MMDS data store is not initialized."),
            Self::DataStoreLimitExceeded {
                limit_bytes,
                size_bytes,
            } => write!(
                f,
                "The MMDS data store size limit was exceeded: {size_bytes} bytes > {limit_bytes} bytes"
            ),
            Self::Serialization => f.write_str("The MMDS data store could not be serialized."),
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

    pub fn put_data(&mut self, input: MmdsContentInput) -> Result<(), MmdsDataStoreError> {
        let value = input.into_value();
        self.ensure_within_limit(&value)?;
        self.value = Some(value);
        Ok(())
    }

    pub fn patch_data(&mut self, input: MmdsContentInput) -> Result<(), MmdsDataStoreError> {
        let value = self
            .value
            .as_ref()
            .ok_or(MmdsDataStoreError::NotInitialized)?;
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
