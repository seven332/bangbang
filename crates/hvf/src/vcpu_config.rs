//! Default arm64 vCPU configuration exposed by Hypervisor.framework.

use bangbang_runtime::BackendError;

use crate::HvfBackend;

/// Detached cache feature registers from the default arm64 vCPU configuration.
///
/// `CTR_EL0`, `CLIDR_EL1`, and `DCZID_EL0` describe cache features exposed by
/// a fresh default Hypervisor.framework vCPU configuration. They are immutable
/// configuration metadata, not the live `CSSELR_EL1` selector or the selected
/// instruction/data cache geometry reported through `CCSIDR_EL1`. This raw
/// getter-only value defines no interpretation, feature mask, destination
/// compatibility decision, cache maintenance, persistence, snapshot schema, or
/// restore policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64VcpuCacheConfiguration {
    ctr_el0: u64,
    clidr_el1: u64,
    dczid_el0: u64,
}

impl HvfArm64VcpuCacheConfiguration {
    const fn new(values: [u64; 3]) -> Self {
        let [ctr_el0, clidr_el1, dczid_el0] = values;
        Self {
            ctr_el0,
            clidr_el1,
            dczid_el0,
        }
    }

    /// Return the raw default-configuration `CTR_EL0` feature value.
    pub const fn ctr_el0(self) -> u64 {
        self.ctr_el0
    }

    /// Return the raw default-configuration `CLIDR_EL1` feature value.
    pub const fn clidr_el1(self) -> u64 {
        self.clidr_el1
    }

    /// Return the raw default-configuration `DCZID_EL0` feature value.
    pub const fn dczid_el0(self) -> u64 {
        self.dczid_el0
    }
}

/// Detached raw cache geometry from the default arm64 vCPU configuration.
///
/// Hypervisor.framework supplies eight `CCSIDR_EL1` values for data or unified
/// caches and eight more for instruction caches. This value preserves every
/// entry exactly as returned. It does not identify implemented cache levels,
/// interpret any field, reconcile the arrays with `CTR_EL0` or `CLIDR_EL1`, or
/// represent the live `CSSELR_EL1`-selected view. It defines no feature mask,
/// destination compatibility decision, synchronization, cache maintenance,
/// persistence, snapshot schema, or restore policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64VcpuCacheGeometry {
    data_or_unified_ccsidr_el1: [u64; 8],
    instruction_ccsidr_el1: [u64; 8],
}

impl HvfArm64VcpuCacheGeometry {
    const fn new(values: [[u64; 8]; 2]) -> Self {
        let [data_or_unified_ccsidr_el1, instruction_ccsidr_el1] = values;
        Self {
            data_or_unified_ccsidr_el1,
            instruction_ccsidr_el1,
        }
    }

    /// Return all raw data or unified cache `CCSIDR_EL1` values.
    pub const fn data_or_unified_ccsidr_el1(&self) -> &[u64; 8] {
        &self.data_or_unified_ccsidr_el1
    }

    /// Return all raw instruction cache `CCSIDR_EL1` values.
    pub const fn instruction_ccsidr_el1(&self) -> &[u64; 8] {
        &self.instruction_ccsidr_el1
    }
}

impl HvfBackend {
    /// Query cache features from a fresh default arm64 vCPU configuration.
    ///
    /// This query takes no VM or vCPU handle and may be called before creating
    /// a backend or VM. It does not change the configuration used by
    /// `HvfBackend::create_vcpu`, which continues to request HVF's default by
    /// passing no explicit configuration object.
    pub fn arm64_vcpu_cache_configuration() -> Result<HvfArm64VcpuCacheConfiguration, BackendError>
    {
        crate::ffi::get_arm64_vcpu_cache_feature_registers()
            .map(HvfArm64VcpuCacheConfiguration::new)
    }

    /// Query raw cache geometry from a fresh default arm64 vCPU configuration.
    ///
    /// This query is independent of
    /// [`HvfBackend::arm64_vcpu_cache_configuration`]: each method creates and
    /// releases its own default configuration, so their results do not form
    /// one atomic manifest. This query takes no VM or vCPU handle and does not
    /// change the null/default configuration used by `HvfBackend::create_vcpu`.
    pub fn arm64_vcpu_cache_geometry() -> Result<HvfArm64VcpuCacheGeometry, BackendError> {
        crate::ffi::get_arm64_vcpu_cache_geometry().map(HvfArm64VcpuCacheGeometry::new)
    }
}

#[cfg(test)]
mod tests {
    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    use bangbang_runtime::BackendError;

    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    use super::HvfBackend;
    use super::{HvfArm64VcpuCacheConfiguration, HvfArm64VcpuCacheGeometry};

    #[test]
    fn cache_configuration_preserves_all_feature_values() {
        let configuration =
            HvfArm64VcpuCacheConfiguration::new([0, u64::MAX, 0x0123_4567_89ab_cdef]);

        assert_eq!(configuration.ctr_el0(), 0);
        assert_eq!(configuration.clidr_el1(), u64::MAX);
        assert_eq!(configuration.dczid_el0(), 0x0123_4567_89ab_cdef);
    }

    #[test]
    fn cache_geometry_preserves_all_ccsidr_values() {
        let values = [
            [
                0,
                1,
                u64::MAX,
                0x0123_4567_89ab_cdef,
                0xfedc_ba98_7654_3210,
                4,
                5,
                6,
            ],
            [
                0x8000_0000_0000_0000,
                0x7fff_ffff_ffff_ffff,
                7,
                8,
                9,
                10,
                11,
                12,
            ],
        ];
        let geometry = HvfArm64VcpuCacheGeometry::new(values);

        assert_eq!(geometry.data_or_unified_ccsidr_el1(), &values[0]);
        assert_eq!(geometry.instruction_ccsidr_el1(), &values[1]);
    }

    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    #[test]
    fn cache_configuration_query_reports_unsupported_compile_target() {
        assert_eq!(
            HvfBackend::arm64_vcpu_cache_configuration(),
            Err(BackendError::Unsupported(
                crate::ffi::UNSUPPORTED_TARGET_MESSAGE
            ))
        );
    }

    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    #[test]
    fn cache_geometry_query_reports_unsupported_compile_target() {
        assert_eq!(
            HvfBackend::arm64_vcpu_cache_geometry(),
            Err(BackendError::Unsupported(
                crate::ffi::UNSUPPORTED_TARGET_MESSAGE
            ))
        );
    }
}
