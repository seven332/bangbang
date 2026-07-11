//! Arm64 SME configuration exposed by Hypervisor.framework.

use bangbang_runtime::BackendError;

use crate::HvfBackend;

/// Detached Hypervisor.framework Arm64 SME configuration.
///
/// The maximum streaming vector length is the largest SVL, in bytes, that a
/// guest may use. It is configuration-wide and distinct from SVE/SME feature
/// identification, the effective SVL selected through `SMCR_EL1`, mutable SME
/// PSTATE, and the conditionally present Z, P, ZA, and ZT0 contents. This
/// getter-only value defines no feature or destination-compatibility policy,
/// buffer ownership, persistence, snapshot schema, or restore behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64SmeConfiguration {
    max_svl_bytes: usize,
}

impl HvfArm64SmeConfiguration {
    const fn new(max_svl_bytes: usize) -> Self {
        Self { max_svl_bytes }
    }

    /// Return the maximum streaming vector length guests may use, in bytes.
    pub const fn max_svl_bytes(self) -> usize {
        self.max_svl_bytes
    }
}

impl HvfBackend {
    /// Query the configuration-wide Arm64 SME limits exposed by HVF.
    ///
    /// This query requires macOS 15.2 or newer, takes no VM or vCPU handle, and
    /// may be called before creating a backend or VM. It preserves
    /// Hypervisor.framework errors, including `HV_UNSUPPORTED` on hardware
    /// without SME support.
    pub fn arm64_sme_configuration() -> Result<HvfArm64SmeConfiguration, BackendError> {
        crate::ffi::get_sme_config_max_svl_bytes().map(HvfArm64SmeConfiguration::new)
    }
}

#[cfg(test)]
mod tests {
    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    use bangbang_runtime::BackendError;

    use super::HvfArm64SmeConfiguration;
    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    use super::HvfBackend;

    #[test]
    fn configuration_preserves_maximum_svl_bytes() {
        let configuration = HvfArm64SmeConfiguration::new(usize::MAX - 0x1234);

        assert_eq!(configuration.max_svl_bytes(), usize::MAX - 0x1234);
    }

    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    #[test]
    fn query_reports_unsupported_compile_target() {
        assert_eq!(
            HvfBackend::arm64_sme_configuration(),
            Err(BackendError::Unsupported(
                crate::ffi::UNSUPPORTED_TARGET_MESSAGE
            ))
        );
    }
}
