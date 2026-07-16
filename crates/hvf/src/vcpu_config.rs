//! Default arm64 vCPU configuration exposed by Hypervisor.framework.

use std::fmt;

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
    pub(crate) const fn new(values: [u64; 3]) -> Self {
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

/// One detached, same-configuration native-v1 cache compatibility manifest.
///
/// Unlike the standalone queries, both feature registers and cache geometry
/// are read from one owned default Hypervisor.framework configuration object.
/// The raw values are compatibility metadata and remain redacted from `Debug`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64VcpuCacheManifest {
    configuration: HvfArm64VcpuCacheConfiguration,
    geometry: HvfArm64VcpuCacheGeometry,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct HvfArm64VcpuCacheFdtSource {
    id_aa64mmfr2_el1: u64,
    manifest: HvfArm64VcpuCacheManifest,
}

impl HvfArm64VcpuCacheFdtSource {
    pub(crate) const fn new(id_aa64mmfr2_el1: u64, manifest: HvfArm64VcpuCacheManifest) -> Self {
        Self {
            id_aa64mmfr2_el1,
            manifest,
        }
    }

    pub(crate) const fn id_aa64mmfr2_el1(self) -> u64 {
        self.id_aa64mmfr2_el1
    }

    pub(crate) const fn manifest(self) -> HvfArm64VcpuCacheManifest {
        self.manifest
    }
}

impl fmt::Debug for HvfArm64VcpuCacheFdtSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfArm64VcpuCacheFdtSource")
            .field("cache_identity", &"<redacted>")
            .finish()
    }
}

impl HvfArm64VcpuCacheManifest {
    pub(crate) const fn new(
        configuration: HvfArm64VcpuCacheConfiguration,
        geometry: HvfArm64VcpuCacheGeometry,
    ) -> Self {
        Self {
            configuration,
            geometry,
        }
    }

    /// Return the default-vCPU cache feature registers.
    pub const fn configuration(self) -> HvfArm64VcpuCacheConfiguration {
        self.configuration
    }

    /// Return the default-vCPU cache geometry arrays.
    pub const fn geometry(self) -> HvfArm64VcpuCacheGeometry {
        self.geometry
    }
}

impl fmt::Debug for HvfArm64VcpuCacheManifest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfArm64VcpuCacheManifest")
            .field("cache_compatibility", &"<redacted>")
            .finish()
    }
}

impl HvfArm64VcpuCacheGeometry {
    pub(crate) const fn new(values: [[u64; 8]; 2]) -> Self {
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

    /// Query cache features and geometry from one default vCPU configuration.
    pub fn arm64_vcpu_cache_manifest() -> Result<HvfArm64VcpuCacheManifest, BackendError> {
        crate::ffi::get_arm64_vcpu_cache_manifest().map(|(configuration, geometry)| {
            HvfArm64VcpuCacheManifest::new(
                HvfArm64VcpuCacheConfiguration::new(configuration),
                HvfArm64VcpuCacheGeometry::new(geometry),
            )
        })
    }

    pub(crate) fn arm64_vcpu_cache_fdt_source() -> Result<HvfArm64VcpuCacheFdtSource, BackendError>
    {
        crate::ffi::get_arm64_vcpu_cache_fdt_source().map(
            |(id_aa64mmfr2_el1, configuration, geometry)| {
                HvfArm64VcpuCacheFdtSource::new(
                    id_aa64mmfr2_el1,
                    HvfArm64VcpuCacheManifest::new(
                        HvfArm64VcpuCacheConfiguration::new(configuration),
                        HvfArm64VcpuCacheGeometry::new(geometry),
                    ),
                )
            },
        )
    }
}

#[cfg(test)]
mod tests {
    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    use bangbang_runtime::BackendError;

    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    use super::HvfBackend;
    use super::{
        HvfArm64VcpuCacheConfiguration, HvfArm64VcpuCacheFdtSource, HvfArm64VcpuCacheGeometry,
        HvfArm64VcpuCacheManifest,
    };

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

    #[test]
    fn cache_manifest_preserves_both_values_and_redacts_debug() {
        let configuration = HvfArm64VcpuCacheConfiguration::new([1, 2, 3]);
        let geometry = HvfArm64VcpuCacheGeometry::new([[4; 8], [5; 8]]);
        let manifest = HvfArm64VcpuCacheManifest::new(configuration, geometry);

        assert_eq!(manifest.configuration(), configuration);
        assert_eq!(manifest.geometry(), geometry);
        assert_eq!(
            format!("{manifest:?}"),
            "HvfArm64VcpuCacheManifest { cache_compatibility: \"<redacted>\" }"
        );
    }

    #[test]
    fn cache_fdt_source_preserves_mmfr2_and_manifest_and_redacts_debug() {
        let manifest = HvfArm64VcpuCacheManifest::new(
            HvfArm64VcpuCacheConfiguration::new([1, 2, 3]),
            HvfArm64VcpuCacheGeometry::new([[4; 8], [5; 8]]),
        );
        let source = HvfArm64VcpuCacheFdtSource::new(u64::MAX, manifest);

        assert_eq!(source.id_aa64mmfr2_el1(), u64::MAX);
        assert_eq!(source.manifest(), manifest);
        assert_eq!(
            format!("{source:?}"),
            "HvfArm64VcpuCacheFdtSource { cache_identity: \"<redacted>\" }"
        );
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

    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    #[test]
    fn cache_manifest_query_reports_unsupported_compile_target() {
        assert_eq!(
            HvfBackend::arm64_vcpu_cache_manifest(),
            Err(BackendError::Unsupported(
                crate::ffi::UNSUPPORTED_TARGET_MESSAGE
            ))
        );
    }

    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    #[test]
    fn cache_fdt_source_query_reports_unsupported_compile_target() {
        assert_eq!(
            HvfBackend::arm64_vcpu_cache_fdt_source(),
            Err(BackendError::Unsupported(
                crate::ffi::UNSUPPORTED_TARGET_MESSAGE
            ))
        );
    }
}
