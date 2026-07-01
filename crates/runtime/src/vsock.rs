//! Backend-neutral vsock configuration model.

use std::fmt;
use std::path::{Path, PathBuf};

pub const MIN_GUEST_CID: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VsockConfigInput {
    vsock_id: Option<String>,
    guest_cid: u32,
    uds_path: String,
}

impl VsockConfigInput {
    pub fn new(guest_cid: u32, uds_path: impl Into<String>) -> Self {
        Self {
            vsock_id: None,
            guest_cid,
            uds_path: uds_path.into(),
        }
    }

    pub fn with_vsock_id(mut self, vsock_id: impl Into<String>) -> Self {
        self.vsock_id = Some(vsock_id.into());
        self
    }

    pub fn vsock_id(&self) -> Option<&str> {
        self.vsock_id.as_deref()
    }

    pub const fn guest_cid(&self) -> u32 {
        self.guest_cid
    }

    pub fn uds_path(&self) -> &str {
        &self.uds_path
    }

    pub fn validate(self) -> Result<VsockConfig, VsockConfigError> {
        VsockConfig::try_from(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VsockConfig {
    vsock_id: Option<String>,
    guest_cid: u32,
    uds_path: PathBuf,
}

impl VsockConfig {
    pub fn vsock_id(&self) -> Option<&str> {
        self.vsock_id.as_deref()
    }

    pub const fn guest_cid(&self) -> u32 {
        self.guest_cid
    }

    pub fn uds_path(&self) -> &Path {
        &self.uds_path
    }
}

impl TryFrom<VsockConfigInput> for VsockConfig {
    type Error = VsockConfigError;

    fn try_from(input: VsockConfigInput) -> Result<Self, Self::Error> {
        if input.guest_cid < MIN_GUEST_CID {
            return Err(VsockConfigError::GuestCidTooSmall {
                guest_cid: input.guest_cid,
                min: MIN_GUEST_CID,
            });
        }

        if let Some(vsock_id) = input.vsock_id.as_deref() {
            if vsock_id.is_empty() {
                return Err(VsockConfigError::EmptyVsockId);
            }
            if has_control_character(vsock_id) {
                return Err(VsockConfigError::InvalidVsockId {
                    vsock_id: vsock_id.to_string(),
                });
            }
        }

        if input.uds_path.is_empty() {
            return Err(VsockConfigError::EmptySocketPath);
        }
        if has_control_character(&input.uds_path) {
            return Err(VsockConfigError::InvalidSocketPath);
        }

        Ok(Self {
            vsock_id: input.vsock_id,
            guest_cid: input.guest_cid,
            uds_path: PathBuf::from(input.uds_path),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VsockConfigError {
    GuestCidTooSmall { guest_cid: u32, min: u32 },
    EmptyVsockId,
    InvalidVsockId { vsock_id: String },
    EmptySocketPath,
    InvalidSocketPath,
}

impl fmt::Display for VsockConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GuestCidTooSmall { guest_cid, min } => {
                write!(f, "vsock guest_cid {guest_cid} is below minimum {min}")
            }
            Self::EmptyVsockId => f.write_str("vsock_id must not be empty"),
            Self::InvalidVsockId { .. } => {
                f.write_str("vsock_id must not contain control characters")
            }
            Self::EmptySocketPath => f.write_str("vsock uds_path must not be empty"),
            Self::InvalidSocketPath => {
                f.write_str("vsock uds_path must not contain control characters")
            }
        }
    }
}

impl std::error::Error for VsockConfigError {}

fn has_control_character(value: &str) -> bool {
    value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    use std::path::Path;

    use super::{MIN_GUEST_CID, VsockConfigError, VsockConfigInput};

    fn validate(input: VsockConfigInput) -> Result<super::VsockConfig, VsockConfigError> {
        input.validate()
    }

    #[test]
    fn accepts_minimal_config() {
        let config =
            validate(VsockConfigInput::new(MIN_GUEST_CID, "./v.sock")).expect("valid config");

        assert_eq!(config.vsock_id(), None);
        assert_eq!(config.guest_cid(), MIN_GUEST_CID);
        assert_eq!(config.uds_path(), Path::new("./v.sock"));
    }

    #[test]
    fn accepts_optional_deprecated_vsock_id() {
        let config = validate(VsockConfigInput::new(42, "/tmp/v.sock").with_vsock_id("vsock_0"))
            .expect("valid config");

        assert_eq!(config.vsock_id(), Some("vsock_0"));
        assert_eq!(config.guest_cid(), 42);
        assert_eq!(config.uds_path(), Path::new("/tmp/v.sock"));
    }

    #[test]
    fn rejects_guest_cid_below_firecracker_minimum() {
        let err = validate(VsockConfigInput::new(2, "/tmp/v.sock"))
            .expect_err("small guest cid should fail");

        assert_eq!(
            err,
            VsockConfigError::GuestCidTooSmall {
                guest_cid: 2,
                min: MIN_GUEST_CID,
            }
        );
        assert_eq!(err.to_string(), "vsock guest_cid 2 is below minimum 3");
    }

    #[test]
    fn rejects_empty_vsock_id() {
        let err = validate(VsockConfigInput::new(3, "/tmp/v.sock").with_vsock_id(""))
            .expect_err("empty id should fail");

        assert_eq!(err, VsockConfigError::EmptyVsockId);
        assert_eq!(err.to_string(), "vsock_id must not be empty");
    }

    #[test]
    fn rejects_control_character_vsock_id_without_echoing_it() {
        let invalid = "id\nsecret";
        let err = validate(VsockConfigInput::new(3, "/tmp/v.sock").with_vsock_id(invalid))
            .expect_err("control character id should fail");

        assert_eq!(
            err,
            VsockConfigError::InvalidVsockId {
                vsock_id: invalid.to_string(),
            }
        );
        assert_eq!(
            err.to_string(),
            "vsock_id must not contain control characters"
        );
        assert!(!err.to_string().contains(invalid));
    }

    #[test]
    fn rejects_empty_socket_path() {
        let err =
            validate(VsockConfigInput::new(3, "")).expect_err("empty socket path should fail");

        assert_eq!(err, VsockConfigError::EmptySocketPath);
        assert_eq!(err.to_string(), "vsock uds_path must not be empty");
    }

    #[test]
    fn rejects_control_character_socket_path_without_echoing_it() {
        let invalid = "/tmp/v.sock\nsecret";
        let err = validate(VsockConfigInput::new(3, invalid))
            .expect_err("control character socket path should fail");

        assert_eq!(err, VsockConfigError::InvalidSocketPath);
        assert_eq!(
            err.to_string(),
            "vsock uds_path must not contain control characters"
        );
        assert!(!err.to_string().contains(invalid));
    }

    #[test]
    fn errors_have_no_sources() {
        assert!(VsockConfigError::EmptySocketPath.source().is_none());
    }
}
