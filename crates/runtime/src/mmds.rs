//! Backend-neutral MMDS control-plane input model.

use std::net::Ipv4Addr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmdsContentInput {
    json: String,
}

impl MmdsContentInput {
    pub fn new(json: impl Into<String>) -> Self {
        Self { json: json.into() }
    }

    pub fn json(&self) -> &str {
        &self.json
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmdsVersion {
    V1,
    V2,
}
