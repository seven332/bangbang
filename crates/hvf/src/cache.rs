//! Guest cache presentation derived from public HVF and macOS facts.

#[cfg(target_os = "macos")]
use std::ffi::CString;
use std::fmt;
use std::io;

use bangbang_runtime::BackendError;
use bangbang_runtime::fdt::{
    Arm64FdtCache, Arm64FdtCacheError, Arm64FdtCacheHierarchy, Arm64FdtCacheHierarchyError,
    Arm64FdtCacheType,
};

use crate::HvfBackend;
use crate::vcpu_config::HvfArm64VcpuCacheFdtSource;

const MAX_PERFORMANCE_LEVELS: u32 = 32;
const MAX_GUEST_VCPUS: u8 = 32;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PreparedHvfArm64Cache {
    source: HvfArm64VcpuCacheFdtSource,
    hierarchy: Arm64FdtCacheHierarchy,
}

impl PreparedHvfArm64Cache {
    pub(crate) fn into_parts(self) -> (HvfArm64VcpuCacheFdtSource, Arm64FdtCacheHierarchy) {
        (self.source, self.hierarchy)
    }
}

impl fmt::Debug for PreparedHvfArm64Cache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedHvfArm64Cache")
            .field("cache_presentation", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HostFactReadErrorKind {
    InvalidName,
    WrongWidth,
    System,
}

pub struct HostFactReadError {
    kind: HostFactReadErrorKind,
}

impl HostFactReadError {
    #[cfg(target_os = "macos")]
    const fn invalid_name() -> Self {
        Self {
            kind: HostFactReadErrorKind::InvalidName,
        }
    }

    #[cfg(target_os = "macos")]
    const fn wrong_width() -> Self {
        Self {
            kind: HostFactReadErrorKind::WrongWidth,
        }
    }

    fn system(_: io::Error) -> Self {
        Self {
            kind: HostFactReadErrorKind::System,
        }
    }
}

impl fmt::Debug for HostFactReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for HostFactReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            HostFactReadErrorKind::InvalidName => {
                f.write_str("public macOS cache fact name is invalid")
            }
            HostFactReadErrorKind::WrongWidth => {
                f.write_str("public macOS cache fact has an unexpected width")
            }
            HostFactReadErrorKind::System => f.write_str("public macOS cache fact query failed"),
        }
    }
}

impl std::error::Error for HostFactReadError {}

trait HostCacheFactReader {
    fn read_u32(&self, name: &str) -> Result<Option<u32>, HostFactReadError>;
    fn read_u64(&self, name: &str) -> Result<Option<u64>, HostFactReadError>;
}

#[derive(Debug, Default, Clone, Copy)]
struct MacOsHostCacheFactReader;

impl HostCacheFactReader for MacOsHostCacheFactReader {
    fn read_u32(&self, name: &str) -> Result<Option<u32>, HostFactReadError> {
        read_sysctl_u32(name)
    }

    fn read_u64(&self, name: &str) -> Result<Option<u64>, HostFactReadError> {
        read_sysctl_u32(name).map(|value| value.map(u64::from))
    }
}

#[cfg(target_os = "macos")]
fn read_sysctl_u32(name: &str) -> Result<Option<u32>, HostFactReadError> {
    read_sysctl_width::<{ std::mem::size_of::<u32>() }>(name)
        .map(|value| value.map(u32::from_ne_bytes))
}

#[cfg(target_os = "macos")]
fn read_sysctl_width<const WIDTH: usize>(
    name: &str,
) -> Result<Option<[u8; WIDTH]>, HostFactReadError> {
    let name = CString::new(name).map_err(|_| HostFactReadError::invalid_name())?;
    let mut value = [0; WIDTH];
    let mut size = WIDTH;
    // SAFETY: `name` is NUL terminated, `value` is writable for exactly its
    // declared width, `size` points to that width, and this is a read-only query.
    let result = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&raw mut value).cast(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        let source = io::Error::last_os_error();
        if matches!(
            source.raw_os_error(),
            Some(libc::ENOENT) | Some(libc::EINVAL)
        ) {
            return Ok(None);
        }
        return Err(HostFactReadError::system(source));
    }
    if size != WIDTH {
        return Err(HostFactReadError::wrong_width());
    }
    Ok(Some(value))
}

#[cfg(not(target_os = "macos"))]
fn read_sysctl_u32(_: &str) -> Result<Option<u32>, HostFactReadError> {
    Err(HostFactReadError::system(io::Error::new(
        io::ErrorKind::Unsupported,
        "public macOS cache facts are unavailable on this target",
    )))
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct DecodedCache {
    level: u8,
    cache_type: Arm64FdtCacheType,
    size: u32,
    line_size: u32,
    sets: u32,
    ways: u32,
}

#[derive(Clone, PartialEq, Eq)]
struct DecodedCacheHierarchy {
    caches: Vec<DecodedCache>,
}

impl DecodedCacheHierarchy {
    fn cache(&self, level: u8, cache_type: Arm64FdtCacheType) -> Option<DecodedCache> {
        self.caches
            .iter()
            .copied()
            .find(|cache| cache.level == level && cache.cache_type == cache_type)
    }

    fn unified_or(&self, level: u8, cache_type: Arm64FdtCacheType) -> Option<DecodedCache> {
        self.cache(level, cache_type)
            .or_else(|| self.cache(level, Arm64FdtCacheType::Unified))
    }

    fn last_level(&self) -> u8 {
        self.caches.last().map_or(0, |cache| cache.level)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct HostPerformanceLevel {
    physical_cpu_max: Option<u32>,
    logical_cpu_max: Option<u32>,
    l1d_size: Option<u64>,
    l1i_size: Option<u64>,
    l2_size: Option<u64>,
    cpus_per_l2: Option<u32>,
    l3_size: Option<u64>,
    cpus_per_l3: Option<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct SelectedSharing {
    l2: Option<u32>,
    l3: Option<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CandidateResult {
    Match(SelectedSharing),
    Mismatch,
    Incomplete,
    InvalidSharing,
}

pub enum HvfArm64CacheTopologyError {
    Backend { source: BackendError },
    HostFact { source: HostFactReadError },
    MissingPerformanceLevelCount,
    InvalidPerformanceLevelCount,
    InvalidCcidx,
    InvalidClidr,
    MissingL1,
    UnsupportedShape,
    InvalidCtr,
    InvalidDczid,
    InvalidCcsidr,
    GeometryOverflow,
    IncompleteHostFacts,
    InvalidHostSharing,
    NoMatchingPerformanceLevel,
    AmbiguousPerformanceLevel,
    InvalidVcpuCount,
    RuntimeCache { source: Arm64FdtCacheError },
    RuntimeHierarchy { source: Arm64FdtCacheHierarchyError },
}

impl fmt::Debug for HvfArm64CacheTopologyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for HvfArm64CacheTopologyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend { .. } => f.write_str("default HVF cache identity query failed"),
            Self::HostFact { source } => write!(f, "host cache admission failed: {source}"),
            Self::MissingPerformanceLevelCount => {
                f.write_str("host cache performance-level count is unavailable")
            }
            Self::InvalidPerformanceLevelCount => {
                f.write_str("host cache performance-level count is invalid")
            }
            Self::InvalidCcidx => f.write_str("HVF cache index format is unsupported"),
            Self::InvalidClidr => f.write_str("HVF cache level description is invalid"),
            Self::MissingL1 => f.write_str("HVF cache description has no level-one cache"),
            Self::UnsupportedShape => {
                f.write_str("HVF cache shape cannot be proven by public host facts")
            }
            Self::InvalidCtr => f.write_str("HVF cache line metadata is inconsistent"),
            Self::InvalidDczid => f.write_str("HVF data-zero metadata is invalid"),
            Self::InvalidCcsidr => f.write_str("HVF cache geometry encoding is invalid"),
            Self::GeometryOverflow => f.write_str("HVF cache geometry exceeds FDT limits"),
            Self::IncompleteHostFacts => f.write_str("public host cache facts are incomplete"),
            Self::InvalidHostSharing => {
                f.write_str("public host cache sharing facts are inconsistent")
            }
            Self::NoMatchingPerformanceLevel => {
                f.write_str("no public host cache description matches the HVF view")
            }
            Self::AmbiguousPerformanceLevel => {
                f.write_str("multiple public host cache descriptions match the HVF view")
            }
            Self::InvalidVcpuCount => {
                f.write_str("configured vCPU count is outside the cache-presentation range")
            }
            Self::RuntimeCache { source } => {
                write!(f, "runtime cache geometry rejected the HVF view: {source}")
            }
            Self::RuntimeHierarchy { source } => {
                write!(f, "runtime cache hierarchy rejected the HVF view: {source}")
            }
        }
    }
}

impl std::error::Error for HvfArm64CacheTopologyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Backend { source } => Some(source),
            Self::HostFact { source } => Some(source),
            Self::RuntimeCache { source } => Some(source),
            Self::RuntimeHierarchy { source } => Some(source),
            Self::MissingPerformanceLevelCount
            | Self::InvalidPerformanceLevelCount
            | Self::InvalidCcidx
            | Self::InvalidClidr
            | Self::MissingL1
            | Self::UnsupportedShape
            | Self::InvalidCtr
            | Self::InvalidDczid
            | Self::InvalidCcsidr
            | Self::GeometryOverflow
            | Self::IncompleteHostFacts
            | Self::InvalidHostSharing
            | Self::NoMatchingPerformanceLevel
            | Self::AmbiguousPerformanceLevel
            | Self::InvalidVcpuCount => None,
        }
    }
}

impl HvfBackend {
    pub fn arm64_fdt_cache_hierarchy(
        vcpu_count: u8,
    ) -> Result<Arm64FdtCacheHierarchy, HvfArm64CacheTopologyError> {
        prepare_arm64_cache(vcpu_count).map(|prepared| prepared.hierarchy)
    }
}

pub(crate) fn prepare_arm64_cache(
    vcpu_count: u8,
) -> Result<PreparedHvfArm64Cache, HvfArm64CacheTopologyError> {
    let source = HvfBackend::arm64_vcpu_cache_fdt_source()
        .map_err(|source| HvfArm64CacheTopologyError::Backend { source })?;
    prepare_arm64_cache_with(source, vcpu_count, &MacOsHostCacheFactReader)
}

fn prepare_arm64_cache_with(
    source: HvfArm64VcpuCacheFdtSource,
    vcpu_count: u8,
    reader: &impl HostCacheFactReader,
) -> Result<PreparedHvfArm64Cache, HvfArm64CacheTopologyError> {
    if vcpu_count == 0 || vcpu_count > MAX_GUEST_VCPUS {
        return Err(HvfArm64CacheTopologyError::InvalidVcpuCount);
    }
    let decoded = decode_cache_hierarchy(source)?;
    validate_supported_shape(&decoded)?;
    let sharing = select_host_sharing(&decoded, reader)?;
    let hierarchy = runtime_hierarchy(&decoded, sharing)?;
    Ok(PreparedHvfArm64Cache { source, hierarchy })
}

fn decode_cache_hierarchy(
    source: HvfArm64VcpuCacheFdtSource,
) -> Result<DecodedCacheHierarchy, HvfArm64CacheTopologyError> {
    let ccidx = (source.id_aa64mmfr2_el1() >> 20) & 0xf;
    if ccidx > 1 {
        return Err(HvfArm64CacheTopologyError::InvalidCcidx);
    }
    let manifest = source.manifest();
    let configuration = manifest.configuration();
    let clidr = configuration.clidr_el1();
    if clidr >> 47 != 0 {
        return Err(HvfArm64CacheTopologyError::InvalidClidr);
    }

    let geometry = manifest.geometry();
    let mut caches = Vec::new();
    for level in 1_u8..=7 {
        let cache_type = (clidr >> (3 * (level - 1))) & 0x7;
        if cache_type == 0 {
            break;
        }
        let index = usize::from(level - 1);
        let data = geometry
            .data_or_unified_ccsidr_el1()
            .get(index)
            .copied()
            .ok_or(HvfArm64CacheTopologyError::InvalidCcsidr)?;
        let instruction = geometry
            .instruction_ccsidr_el1()
            .get(index)
            .copied()
            .ok_or(HvfArm64CacheTopologyError::InvalidCcsidr)?;
        match cache_type {
            1 => caches.push(decode_ccsidr(
                level,
                Arm64FdtCacheType::Instruction,
                instruction,
                ccidx == 1,
            )?),
            2 => caches.push(decode_ccsidr(
                level,
                Arm64FdtCacheType::Data,
                data,
                ccidx == 1,
            )?),
            3 => {
                caches.push(decode_ccsidr(
                    level,
                    Arm64FdtCacheType::Data,
                    data,
                    ccidx == 1,
                )?);
                caches.push(decode_ccsidr(
                    level,
                    Arm64FdtCacheType::Instruction,
                    instruction,
                    ccidx == 1,
                )?);
            }
            4 => caches.push(decode_ccsidr(
                level,
                Arm64FdtCacheType::Unified,
                data,
                ccidx == 1,
            )?),
            _ => return Err(HvfArm64CacheTopologyError::InvalidClidr),
        }
    }
    if caches.first().is_none_or(|cache| cache.level != 1) {
        return Err(HvfArm64CacheTopologyError::MissingL1);
    }

    let decoded = DecodedCacheHierarchy { caches };
    validate_ctr(configuration.ctr_el0(), &decoded)?;
    validate_dczid(configuration.dczid_el0())?;
    Ok(decoded)
}

fn decode_ccsidr(
    level: u8,
    cache_type: Arm64FdtCacheType,
    raw: u64,
    ccidx: bool,
) -> Result<DecodedCache, HvfArm64CacheTopologyError> {
    let (sets_minus_one, ways_minus_one) = if ccidx {
        if raw >> 56 != 0 || (raw & 0xff00_0000) != 0 {
            return Err(HvfArm64CacheTopologyError::InvalidCcsidr);
        }
        ((raw >> 32) & 0x00ff_ffff, (raw >> 3) & 0x1f_ffff)
    } else {
        if raw >> 32 != 0 {
            return Err(HvfArm64CacheTopologyError::InvalidCcsidr);
        }
        ((raw >> 13) & 0x7fff, (raw >> 3) & 0x3ff)
    };
    let line_size = 1_u64
        .checked_shl(
            u32::try_from((raw & 0x7) + 4)
                .map_err(|_| HvfArm64CacheTopologyError::GeometryOverflow)?,
        )
        .ok_or(HvfArm64CacheTopologyError::GeometryOverflow)?;
    let sets = sets_minus_one
        .checked_add(1)
        .ok_or(HvfArm64CacheTopologyError::GeometryOverflow)?;
    let ways = ways_minus_one
        .checked_add(1)
        .ok_or(HvfArm64CacheTopologyError::GeometryOverflow)?;
    let size = line_size
        .checked_mul(sets)
        .and_then(|value| value.checked_mul(ways))
        .ok_or(HvfArm64CacheTopologyError::GeometryOverflow)?;

    Ok(DecodedCache {
        level,
        cache_type,
        size: u32::try_from(size).map_err(|_| HvfArm64CacheTopologyError::GeometryOverflow)?,
        line_size: u32::try_from(line_size)
            .map_err(|_| HvfArm64CacheTopologyError::GeometryOverflow)?,
        sets: u32::try_from(sets).map_err(|_| HvfArm64CacheTopologyError::GeometryOverflow)?,
        ways: u32::try_from(ways).map_err(|_| HvfArm64CacheTopologyError::GeometryOverflow)?,
    })
}

fn validate_ctr(
    ctr: u64,
    hierarchy: &DecodedCacheHierarchy,
) -> Result<(), HvfArm64CacheTopologyError> {
    const CTR_RES1: u64 = 1 << 31;
    const DEFINED_CTR_FIELDS: u64 = 0x3f_0000_0000 | CTR_RES1 | 0x3fff_c00f;
    if ctr & !DEFINED_CTR_FIELDS != 0 || ctr & CTR_RES1 == 0 {
        return Err(HvfArm64CacheTopologyError::InvalidCtr);
    }
    let instruction_minimum = hierarchy
        .caches
        .iter()
        .filter(|cache| {
            matches!(
                cache.cache_type,
                Arm64FdtCacheType::Instruction | Arm64FdtCacheType::Unified
            )
        })
        .map(|cache| cache.line_size)
        .min();
    let data_minimum = hierarchy
        .caches
        .iter()
        .filter(|cache| {
            matches!(
                cache.cache_type,
                Arm64FdtCacheType::Data | Arm64FdtCacheType::Unified
            )
        })
        .map(|cache| cache.line_size)
        .min();
    let ctr_instruction = 4_u32
        .checked_shl(u32::try_from(ctr & 0xf).map_err(|_| HvfArm64CacheTopologyError::InvalidCtr)?)
        .ok_or(HvfArm64CacheTopologyError::InvalidCtr)?;
    let ctr_data = 4_u32
        .checked_shl(
            u32::try_from((ctr >> 16) & 0xf).map_err(|_| HvfArm64CacheTopologyError::InvalidCtr)?,
        )
        .ok_or(HvfArm64CacheTopologyError::InvalidCtr)?;
    if instruction_minimum.is_some_and(|minimum| minimum != ctr_instruction)
        || data_minimum.is_some_and(|minimum| minimum != ctr_data)
    {
        return Err(HvfArm64CacheTopologyError::InvalidCtr);
    }
    Ok(())
}

fn validate_dczid(dczid: u64) -> Result<(), HvfArm64CacheTopologyError> {
    if dczid >> 5 != 0 || (dczid & 0xf) > 9 {
        return Err(HvfArm64CacheTopologyError::InvalidDczid);
    }
    Ok(())
}

fn validate_supported_shape(
    hierarchy: &DecodedCacheHierarchy,
) -> Result<(), HvfArm64CacheTopologyError> {
    let l1 = hierarchy.caches.iter().filter(|cache| cache.level == 1);
    let l1_types = l1.map(|cache| cache.cache_type).collect::<Vec<_>>();
    if l1_types.as_slice() != [Arm64FdtCacheType::Unified]
        && l1_types.as_slice() != [Arm64FdtCacheType::Data, Arm64FdtCacheType::Instruction]
    {
        return Err(HvfArm64CacheTopologyError::UnsupportedShape);
    }
    if hierarchy.last_level() > 3
        || hierarchy
            .caches
            .iter()
            .any(|cache| cache.level > 1 && cache.cache_type != Arm64FdtCacheType::Unified)
    {
        return Err(HvfArm64CacheTopologyError::UnsupportedShape);
    }
    Ok(())
}

fn select_host_sharing(
    hierarchy: &DecodedCacheHierarchy,
    reader: &impl HostCacheFactReader,
) -> Result<SelectedSharing, HvfArm64CacheTopologyError> {
    let count = reader
        .read_u32("hw.nperflevels")
        .map_err(|source| HvfArm64CacheTopologyError::HostFact { source })?
        .ok_or(HvfArm64CacheTopologyError::MissingPerformanceLevelCount)?;
    if count == 0 || count > MAX_PERFORMANCE_LEVELS {
        return Err(HvfArm64CacheTopologyError::InvalidPerformanceLevelCount);
    }

    let mut selected = None;
    let mut saw_incomplete = false;
    let mut saw_invalid_sharing = false;
    for index in 0..count {
        let facts = read_performance_level(reader, index)?;
        match match_performance_level(hierarchy, facts) {
            CandidateResult::Match(sharing) => {
                if selected.replace(sharing).is_some() {
                    return Err(HvfArm64CacheTopologyError::AmbiguousPerformanceLevel);
                }
            }
            CandidateResult::Mismatch => {}
            CandidateResult::Incomplete => saw_incomplete = true,
            CandidateResult::InvalidSharing => saw_invalid_sharing = true,
        }
    }

    if saw_invalid_sharing {
        return Err(HvfArm64CacheTopologyError::InvalidHostSharing);
    }
    if saw_incomplete {
        return Err(HvfArm64CacheTopologyError::IncompleteHostFacts);
    }
    selected.ok_or(HvfArm64CacheTopologyError::NoMatchingPerformanceLevel)
}

fn read_performance_level(
    reader: &impl HostCacheFactReader,
    index: u32,
) -> Result<HostPerformanceLevel, HvfArm64CacheTopologyError> {
    let prefix = format!("hw.perflevel{index}");
    let read_u32 = |suffix: &str| {
        reader
            .read_u32(&format!("{prefix}.{suffix}"))
            .map_err(|source| HvfArm64CacheTopologyError::HostFact { source })
    };
    let read_u64 = |suffix: &str| {
        reader
            .read_u64(&format!("{prefix}.{suffix}"))
            .map_err(|source| HvfArm64CacheTopologyError::HostFact { source })
    };
    Ok(HostPerformanceLevel {
        physical_cpu_max: read_u32("physicalcpu_max")?,
        logical_cpu_max: read_u32("logicalcpu_max")?,
        l1d_size: read_u64("l1dcachesize")?,
        l1i_size: read_u64("l1icachesize")?,
        l2_size: read_u64("l2cachesize")?,
        cpus_per_l2: read_u32("cpusperl2")?,
        l3_size: read_u64("l3cachesize")?,
        cpus_per_l3: read_u32("cpusperl3")?,
    })
}

fn match_performance_level(
    hierarchy: &DecodedCacheHierarchy,
    facts: HostPerformanceLevel,
) -> CandidateResult {
    let Some(l1i) = hierarchy.unified_or(1, Arm64FdtCacheType::Instruction) else {
        return CandidateResult::Mismatch;
    };
    let Some(l1d) = hierarchy.unified_or(1, Arm64FdtCacheType::Data) else {
        return CandidateResult::Mismatch;
    };
    let (Some(host_l1i), Some(host_l1d)) = (facts.l1i_size, facts.l1d_size) else {
        return CandidateResult::Incomplete;
    };
    if host_l1i != u64::from(l1i.size) || host_l1d != u64::from(l1d.size) {
        return CandidateResult::Mismatch;
    }

    let l2 = hierarchy.cache(2, Arm64FdtCacheType::Unified);
    let l3 = hierarchy.cache(3, Arm64FdtCacheType::Unified);
    let l2_result = match_optional_level(l2, facts.l2_size, facts.cpus_per_l2);
    let l3_result = match_optional_level(l3, facts.l3_size, facts.cpus_per_l3);
    if matches!(l2_result, OptionalLevelMatch::Mismatch)
        || matches!(l3_result, OptionalLevelMatch::Mismatch)
    {
        return CandidateResult::Mismatch;
    }
    if matches!(l2_result, OptionalLevelMatch::Incomplete)
        || matches!(l3_result, OptionalLevelMatch::Incomplete)
    {
        return CandidateResult::Incomplete;
    }
    let (Some(physical_max), Some(logical_max)) = (facts.physical_cpu_max, facts.logical_cpu_max)
    else {
        return CandidateResult::Incomplete;
    };
    if physical_max == 0 || logical_max < physical_max {
        return CandidateResult::InvalidSharing;
    }
    let l2_share = l2.map(|_| facts.cpus_per_l2.unwrap_or_default());
    let l3_share = l3.map(|_| facts.cpus_per_l3.unwrap_or_default());
    for share in [l2_share, l3_share].into_iter().flatten() {
        if share == 0 || share > physical_max || !physical_max.is_multiple_of(share) {
            return CandidateResult::InvalidSharing;
        }
    }
    if let (Some(l2_share), Some(l3_share)) = (l2_share, l3_share)
        && (l3_share < l2_share || !l3_share.is_multiple_of(l2_share))
    {
        return CandidateResult::InvalidSharing;
    }

    CandidateResult::Match(SelectedSharing {
        l2: l2_share,
        l3: l3_share,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OptionalLevelMatch {
    Match,
    Mismatch,
    Incomplete,
}

fn match_optional_level(
    cache: Option<DecodedCache>,
    host_size: Option<u64>,
    host_share: Option<u32>,
) -> OptionalLevelMatch {
    match (cache, host_size, host_share) {
        (None, None, None) => OptionalLevelMatch::Match,
        (None, _, _) => OptionalLevelMatch::Mismatch,
        (Some(_), None, _) | (Some(_), _, None) => OptionalLevelMatch::Incomplete,
        (Some(cache), Some(size), Some(_)) if size == u64::from(cache.size) => {
            OptionalLevelMatch::Match
        }
        (Some(_), Some(_), Some(_)) => OptionalLevelMatch::Mismatch,
    }
}

fn runtime_hierarchy(
    decoded: &DecodedCacheHierarchy,
    sharing: SelectedSharing,
) -> Result<Arm64FdtCacheHierarchy, HvfArm64CacheTopologyError> {
    let mut caches = Vec::new();
    caches
        .try_reserve_exact(decoded.caches.len())
        .map_err(|_| HvfArm64CacheTopologyError::GeometryOverflow)?;
    for cache in &decoded.caches {
        let cpus_per_unit = match cache.level {
            1 => 1,
            2 => sharing
                .l2
                .ok_or(HvfArm64CacheTopologyError::IncompleteHostFacts)?,
            3 => sharing
                .l3
                .ok_or(HvfArm64CacheTopologyError::IncompleteHostFacts)?,
            _ => return Err(HvfArm64CacheTopologyError::UnsupportedShape),
        };
        caches.push(
            Arm64FdtCache::new(
                cache.level,
                cache.cache_type,
                cache.size,
                cache.line_size,
                cache.sets,
                cache.ways,
                cpus_per_unit,
            )
            .map_err(|source| HvfArm64CacheTopologyError::RuntimeCache { source })?,
        );
    }
    Arm64FdtCacheHierarchy::new(caches)
        .map_err(|source| HvfArm64CacheTopologyError::RuntimeHierarchy { source })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        HostCacheFactReader, HostFactReadError, HvfArm64CacheTopologyError,
        prepare_arm64_cache_with,
    };
    use crate::vcpu_config::{
        HvfArm64VcpuCacheConfiguration, HvfArm64VcpuCacheFdtSource, HvfArm64VcpuCacheGeometry,
        HvfArm64VcpuCacheManifest,
    };
    use bangbang_runtime::fdt::Arm64FdtCacheType;

    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn macos_sysctl_boundary_distinguishes_values_widths_and_absent_names() {
        let count = super::read_sysctl_u32("hw.nperflevels")
            .expect("documented performance-level count query should succeed")
            .expect("documented performance-level count should exist");
        assert!(count > 0);

        let wrong_width =
            super::read_sysctl_width::<{ std::mem::size_of::<u64>() }>("hw.nperflevels")
                .expect_err("performance-level count has the documented 32-bit width");
        assert!(matches!(
            wrong_width.kind,
            super::HostFactReadErrorKind::WrongWidth
        ));

        let l1d_name = "hw.perflevel0.l1dcachesize";
        let l1d_size = super::read_sysctl_u32(l1d_name)
            .expect("documented cache-size query should succeed")
            .expect("first performance level should publish an L1 data cache size");
        assert!(l1d_size > 0);
        let wrong_cache_width =
            super::read_sysctl_width::<{ std::mem::size_of::<u64>() }>(l1d_name)
                .expect_err("public macOS performance-level cache sizes use a 32-bit payload");
        assert!(matches!(
            wrong_cache_width.kind,
            super::HostFactReadErrorKind::WrongWidth
        ));
        assert!(matches!(
            super::read_sysctl_u32("hw.bangbang_selector_does_not_exist"),
            Ok(None)
        ));
    }

    #[derive(Default)]
    struct TestHostCacheFactReader {
        u32_values: BTreeMap<String, u32>,
        u64_values: BTreeMap<String, u64>,
        failing_name: Option<String>,
    }

    impl TestHostCacheFactReader {
        fn insert_u32(&mut self, name: impl Into<String>, value: u32) {
            self.u32_values.insert(name.into(), value);
        }

        fn insert_u64(&mut self, name: impl Into<String>, value: u64) {
            self.u64_values.insert(name.into(), value);
        }

        fn fail_on(&mut self, name: impl Into<String>) {
            self.failing_name = Some(name.into());
        }

        fn add_performance_level(&mut self, index: u32, facts: super::HostPerformanceLevel) {
            let prefix = format!("hw.perflevel{index}");
            self.insert_u32(
                format!("{prefix}.physicalcpu_max"),
                facts.physical_cpu_max.expect("test physical maximum"),
            );
            self.insert_u32(
                format!("{prefix}.logicalcpu_max"),
                facts.logical_cpu_max.expect("test logical maximum"),
            );
            self.insert_u64(
                format!("{prefix}.l1dcachesize"),
                facts.l1d_size.expect("test L1 data size"),
            );
            self.insert_u64(
                format!("{prefix}.l1icachesize"),
                facts.l1i_size.expect("test L1 instruction size"),
            );
            if let (Some(size), Some(share)) = (facts.l2_size, facts.cpus_per_l2) {
                self.insert_u64(format!("{prefix}.l2cachesize"), size);
                self.insert_u32(format!("{prefix}.cpusperl2"), share);
            }
            if let (Some(size), Some(share)) = (facts.l3_size, facts.cpus_per_l3) {
                self.insert_u64(format!("{prefix}.l3cachesize"), size);
                self.insert_u32(format!("{prefix}.cpusperl3"), share);
            }
        }
    }

    fn performance_level(
        physical_cpu_max: u32,
        logical_cpu_max: u32,
        l1d_size: u64,
        l1i_size: u64,
        l2: Option<(u64, u32)>,
        l3: Option<(u64, u32)>,
    ) -> super::HostPerformanceLevel {
        super::HostPerformanceLevel {
            physical_cpu_max: Some(physical_cpu_max),
            logical_cpu_max: Some(logical_cpu_max),
            l1d_size: Some(l1d_size),
            l1i_size: Some(l1i_size),
            l2_size: l2.map(|(size, _)| size),
            cpus_per_l2: l2.map(|(_, share)| share),
            l3_size: l3.map(|(size, _)| size),
            cpus_per_l3: l3.map(|(_, share)| share),
        }
    }

    impl HostCacheFactReader for TestHostCacheFactReader {
        fn read_u32(&self, name: &str) -> Result<Option<u32>, HostFactReadError> {
            if self.failing_name.as_deref() == Some(name) {
                return Err(HostFactReadError::system(std::io::Error::other(
                    "sensitive host failure",
                )));
            }
            Ok(self.u32_values.get(name).copied())
        }

        fn read_u64(&self, name: &str) -> Result<Option<u64>, HostFactReadError> {
            if self.failing_name.as_deref() == Some(name) {
                return Err(HostFactReadError::system(std::io::Error::other(
                    "sensitive host failure",
                )));
            }
            Ok(self.u64_values.get(name).copied())
        }
    }

    fn line_exponent(line_size: u64) -> u64 {
        u64::from(line_size.trailing_zeros() - 4)
    }

    fn legacy_ccsidr(line_size: u64, sets: u64, ways: u64) -> u64 {
        ((sets - 1) << 13) | ((ways - 1) << 3) | line_exponent(line_size)
    }

    fn ccidx_ccsidr(line_size: u64, sets: u64, ways: u64) -> u64 {
        ((sets - 1) << 32) | ((ways - 1) << 3) | line_exponent(line_size)
    }

    fn ctr(line_size: u64) -> u64 {
        let minimum = u64::from((line_size / 4).trailing_zeros());
        (1 << 31) | minimum | (minimum << 16)
    }

    fn source(
        mmfr2: u64,
        ctr: u64,
        clidr: u64,
        dczid: u64,
        geometry: [[u64; 8]; 2],
    ) -> HvfArm64VcpuCacheFdtSource {
        HvfArm64VcpuCacheFdtSource::new(
            mmfr2,
            HvfArm64VcpuCacheManifest::new(
                HvfArm64VcpuCacheConfiguration::new([ctr, clidr, dczid]),
                HvfArm64VcpuCacheGeometry::new(geometry),
            ),
        )
    }

    fn split_l1_l2_source(ccidx: bool) -> HvfArm64VcpuCacheFdtSource {
        let encode = if ccidx { ccidx_ccsidr } else { legacy_ccsidr };
        let mut geometry = [[u64::MAX; 8]; 2];
        geometry[0][0] = encode(64, 128, 8);
        geometry[1][0] = encode(64, 512, 4);
        geometry[0][1] = encode(128, 2048, 16);
        source(u64::from(ccidx) << 20, ctr(64), 0x23, 4, geometry)
    }

    fn matching_two_level_reader() -> TestHostCacheFactReader {
        let mut reader = TestHostCacheFactReader::default();
        reader.insert_u32("hw.nperflevels", 2);
        reader.add_performance_level(
            0,
            performance_level(8, 8, 128 * KIB, 192 * KIB, Some((16 * MIB, 4)), None),
        );
        reader.add_performance_level(
            1,
            performance_level(4, 4, 64 * KIB, 128 * KIB, Some((4 * MIB, 4)), None),
        );
        reader
    }

    #[test]
    fn legacy_geometry_selects_one_public_performance_level() {
        let source = split_l1_l2_source(false);
        let prepared = prepare_arm64_cache_with(source, 6, &matching_two_level_reader())
            .expect("legacy cache geometry should match the efficiency performance level");
        let (retained_source, hierarchy) = prepared.into_parts();

        assert_eq!(retained_source, source);
        let caches = hierarchy.caches();
        assert_eq!(caches.len(), 3);
        assert_eq!(caches[0].cache_type(), Arm64FdtCacheType::Data);
        assert_eq!(caches[0].size(), 64 * 1024);
        assert_eq!(caches[0].line_size(), 64);
        assert_eq!(caches[0].sets(), 128);
        assert_eq!(caches[0].ways(), 8);
        assert_eq!(caches[0].cpus_per_unit(), 1);
        assert_eq!(caches[1].cache_type(), Arm64FdtCacheType::Instruction);
        assert_eq!(caches[1].size(), 128 * 1024);
        assert_eq!(caches[1].sets(), 512);
        assert_eq!(caches[1].ways(), 4);
        assert_eq!(caches[2].cache_type(), Arm64FdtCacheType::Unified);
        assert_eq!(caches[2].size(), 4 * 1024 * 1024);
        assert_eq!(caches[2].line_size(), 128);
        assert_eq!(caches[2].sets(), 2048);
        assert_eq!(caches[2].ways(), 16);
        assert_eq!(caches[2].cpus_per_unit(), 4);
    }

    #[test]
    fn ccidx_geometry_uses_revised_set_and_way_fields() {
        let cache_source = split_l1_l2_source(true);
        let hierarchy = prepare_arm64_cache_with(cache_source, 4, &matching_two_level_reader())
            .expect("CCIDX cache geometry should decode from the revised fields")
            .into_parts()
            .1;

        assert_eq!(hierarchy.caches()[0].sets(), 128);
        assert_eq!(hierarchy.caches()[0].ways(), 8);
        assert_eq!(hierarchy.caches()[2].sets(), 2048);
        assert_eq!(hierarchy.caches()[2].ways(), 16);
    }

    #[test]
    fn inactive_geometry_entries_are_not_interpreted() {
        let original = split_l1_l2_source(false);
        let manifest = original.manifest();
        let source = source(
            original.id_aa64mmfr2_el1(),
            manifest.configuration().ctr_el0(),
            manifest.configuration().clidr_el1() | (7 << 9),
            manifest.configuration().dczid_el0(),
            [
                *manifest.geometry().data_or_unified_ccsidr_el1(),
                *manifest.geometry().instruction_ccsidr_el1(),
            ],
        );

        prepare_arm64_cache_with(source, 4, &matching_two_level_reader())
            .expect("inactive CCSIDRs and Ctypes after the first zero must be ignored");
    }

    #[test]
    fn rejects_invalid_vcpu_counts_before_reading_host_facts() {
        let mut reader = matching_two_level_reader();
        reader.fail_on("hw.nperflevels");

        assert!(matches!(
            prepare_arm64_cache_with(split_l1_l2_source(false), 0, &reader),
            Err(HvfArm64CacheTopologyError::InvalidVcpuCount)
        ));
        assert!(matches!(
            prepare_arm64_cache_with(split_l1_l2_source(false), 33, &reader),
            Err(HvfArm64CacheTopologyError::InvalidVcpuCount)
        ));
    }

    #[test]
    fn rejects_unsupported_ccidx_and_invalid_clidr_values() {
        let reader = matching_two_level_reader();
        let geometry = split_l1_l2_source(false).manifest().geometry();
        let geometry = [
            *geometry.data_or_unified_ccsidr_el1(),
            *geometry.instruction_ccsidr_el1(),
        ];

        assert!(matches!(
            prepare_arm64_cache_with(source(2 << 20, ctr(64), 0x23, 4, geometry), 1, &reader),
            Err(HvfArm64CacheTopologyError::InvalidCcidx)
        ));
        for reserved_type in 5..=7 {
            assert!(matches!(
                prepare_arm64_cache_with(
                    source(0, ctr(64), reserved_type, 4, geometry),
                    1,
                    &reader,
                ),
                Err(HvfArm64CacheTopologyError::InvalidClidr)
            ));
        }
        assert!(matches!(
            prepare_arm64_cache_with(source(0, ctr(64), 0, 4, geometry), 1, &reader),
            Err(HvfArm64CacheTopologyError::MissingL1)
        ));
        assert!(matches!(
            prepare_arm64_cache_with(source(0, ctr(64), 1 << 47, 4, geometry), 1, &reader),
            Err(HvfArm64CacheTopologyError::InvalidClidr)
        ));
    }

    #[test]
    fn rejects_invalid_active_ccsidr_fields_and_geometry_overflow() {
        let reader = matching_two_level_reader();
        let mut legacy = [[0; 8]; 2];
        legacy[0][0] = 1 << 32;
        assert!(matches!(
            prepare_arm64_cache_with(source(0, ctr(64), 2, 4, legacy), 1, &reader),
            Err(HvfArm64CacheTopologyError::InvalidCcsidr)
        ));

        let mut revised = [[0; 8]; 2];
        revised[0][0] = 1 << 56;
        assert!(matches!(
            prepare_arm64_cache_with(source(1 << 20, ctr(64), 2, 4, revised), 1, &reader),
            Err(HvfArm64CacheTopologyError::InvalidCcsidr)
        ));

        revised[0][0] = 1 << 24;
        assert!(matches!(
            prepare_arm64_cache_with(source(1 << 20, ctr(64), 2, 4, revised), 1, &reader),
            Err(HvfArm64CacheTopologyError::InvalidCcsidr)
        ));

        revised[0][0] = 1 << 28;
        assert!(matches!(
            prepare_arm64_cache_with(source(1 << 20, ctr(64), 2, 4, revised), 1, &reader),
            Err(HvfArm64CacheTopologyError::InvalidCcsidr)
        ));

        revised[0][0] = ccidx_ccsidr(2048, 0x0100_0000, 0x0020_0000);
        assert!(matches!(
            prepare_arm64_cache_with(source(1 << 20, ctr(2048), 2, 4, revised), 1, &reader),
            Err(HvfArm64CacheTopologyError::GeometryOverflow)
        ));
    }

    #[test]
    fn rejects_ctr_and_dczid_inconsistency() {
        let reader = matching_two_level_reader();
        let valid = split_l1_l2_source(false);
        let manifest = valid.manifest();
        let geometry = [
            *manifest.geometry().data_or_unified_ccsidr_el1(),
            *manifest.geometry().instruction_ccsidr_el1(),
        ];

        assert!(matches!(
            prepare_arm64_cache_with(source(0, ctr(128), 0x23, 4, geometry), 1, &reader),
            Err(HvfArm64CacheTopologyError::InvalidCtr)
        ));
        prepare_arm64_cache_with(
            source(0, ctr(64) | (0x3f << 32), 0x23, 4, geometry),
            1,
            &reader,
        )
        .expect("defined CTR TminLine metadata must not be rejected");
        for reserved_ctr_bit in [1 << 4, 1 << 30, 1 << 38] {
            assert!(matches!(
                prepare_arm64_cache_with(
                    source(0, ctr(64) | reserved_ctr_bit, 0x23, 4, geometry),
                    1,
                    &reader,
                ),
                Err(HvfArm64CacheTopologyError::InvalidCtr)
            ));
        }
        assert!(matches!(
            prepare_arm64_cache_with(
                source(0, ctr(64) & !(1 << 31), 0x23, 4, geometry),
                1,
                &reader,
            ),
            Err(HvfArm64CacheTopologyError::InvalidCtr)
        ));
        assert!(matches!(
            prepare_arm64_cache_with(source(0, ctr(64), 0x23, 1 << 9, geometry), 1, &reader),
            Err(HvfArm64CacheTopologyError::InvalidDczid)
        ));
        assert!(matches!(
            prepare_arm64_cache_with(source(0, ctr(64), 0x23, 1 << 5, geometry), 1, &reader),
            Err(HvfArm64CacheTopologyError::InvalidDczid)
        ));
    }

    #[test]
    fn rejects_shapes_public_facts_cannot_prove() {
        let reader = matching_two_level_reader();
        let mut geometry = [[0; 8]; 2];
        for entries in &mut geometry {
            for value in entries.iter_mut().take(4) {
                *value = legacy_ccsidr(64, 64, 8);
            }
        }

        for l1_type in [1, 2] {
            assert!(matches!(
                prepare_arm64_cache_with(source(0, ctr(64), l1_type, 4, geometry), 1, &reader,),
                Err(HvfArm64CacheTopologyError::UnsupportedShape)
            ));
        }
        assert!(matches!(
            prepare_arm64_cache_with(source(0, ctr(64), 3 | (3 << 3), 4, geometry), 1, &reader,),
            Err(HvfArm64CacheTopologyError::UnsupportedShape)
        ));
        assert!(matches!(
            prepare_arm64_cache_with(
                source(0, ctr(64), 3 | (4 << 3) | (4 << 6) | (4 << 9), 4, geometry),
                1,
                &reader,
            ),
            Err(HvfArm64CacheTopologyError::UnsupportedShape)
        ));
    }

    #[test]
    fn rejects_missing_invalid_and_failed_performance_level_count_queries() {
        let source = split_l1_l2_source(false);

        assert!(matches!(
            prepare_arm64_cache_with(source, 1, &TestHostCacheFactReader::default()),
            Err(HvfArm64CacheTopologyError::MissingPerformanceLevelCount)
        ));
        for count in [0, 33] {
            let mut reader = TestHostCacheFactReader::default();
            reader.insert_u32("hw.nperflevels", count);
            assert!(matches!(
                prepare_arm64_cache_with(source, 1, &reader),
                Err(HvfArm64CacheTopologyError::InvalidPerformanceLevelCount)
            ));
        }
        let mut reader = TestHostCacheFactReader::default();
        reader.fail_on("hw.nperflevels");
        let error = prepare_arm64_cache_with(source, 1, &reader)
            .expect_err("host query failure must fail cache admission");
        assert!(matches!(error, HvfArm64CacheTopologyError::HostFact { .. }));
        assert!(!format!("{error:?}").contains("sensitive host failure"));
        let host_error = std::error::Error::source(&error)
            .expect("topology error should retain its redacted host-fact category");
        assert!(host_error.source().is_none());
    }

    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    #[test]
    fn public_cache_hierarchy_query_preserves_unsupported_target_error() {
        assert!(matches!(
            crate::HvfBackend::arm64_fdt_cache_hierarchy(1),
            Err(HvfArm64CacheTopologyError::Backend {
                source: bangbang_runtime::BackendError::Unsupported(_),
            })
        ));
    }

    #[test]
    fn rejects_incomplete_mismatched_and_ambiguous_host_descriptions() {
        let source = split_l1_l2_source(false);

        let mut incomplete = TestHostCacheFactReader::default();
        incomplete.insert_u32("hw.nperflevels", 1);
        incomplete.insert_u64("hw.perflevel0.l1dcachesize", 64 * KIB);
        incomplete.insert_u64("hw.perflevel0.l1icachesize", 128 * KIB);
        assert!(matches!(
            prepare_arm64_cache_with(source, 1, &incomplete),
            Err(HvfArm64CacheTopologyError::IncompleteHostFacts)
        ));

        let mut mismatch = TestHostCacheFactReader::default();
        mismatch.insert_u32("hw.nperflevels", 1);
        mismatch.add_performance_level(
            0,
            performance_level(4, 4, 32 * KIB, 32 * KIB, Some((MIB, 4)), None),
        );
        assert!(matches!(
            prepare_arm64_cache_with(source, 1, &mismatch),
            Err(HvfArm64CacheTopologyError::NoMatchingPerformanceLevel)
        ));

        let mut ambiguous = TestHostCacheFactReader::default();
        ambiguous.insert_u32("hw.nperflevels", 2);
        for index in 0..2 {
            ambiguous.add_performance_level(
                index,
                performance_level(4, 4, 64 * KIB, 128 * KIB, Some((4 * MIB, 4)), None),
            );
        }
        assert!(matches!(
            prepare_arm64_cache_with(source, 1, &ambiguous),
            Err(HvfArm64CacheTopologyError::AmbiguousPerformanceLevel)
        ));

        let mut incomplete_shadow = matching_two_level_reader();
        incomplete_shadow
            .u64_values
            .remove("hw.perflevel0.l1icachesize");
        assert!(matches!(
            prepare_arm64_cache_with(source, 1, &incomplete_shadow),
            Err(HvfArm64CacheTopologyError::IncompleteHostFacts)
        ));

        let mut invalid_shadow = matching_two_level_reader();
        invalid_shadow.insert_u64("hw.perflevel0.l1dcachesize", 64 * KIB);
        invalid_shadow.insert_u64("hw.perflevel0.l1icachesize", 128 * KIB);
        invalid_shadow.insert_u64("hw.perflevel0.l2cachesize", 4 * MIB);
        invalid_shadow.insert_u32("hw.perflevel0.cpusperl2", 0);
        assert!(matches!(
            prepare_arm64_cache_with(source, 1, &invalid_shadow),
            Err(HvfArm64CacheTopologyError::InvalidHostSharing)
        ));
    }

    #[test]
    fn rejects_invalid_host_sharing_without_capping_guest_vcpus() {
        let source = split_l1_l2_source(false);
        for (physical, logical, share) in [(0, 0, 4), (4, 3, 4), (4, 4, 0), (4, 4, 3)] {
            let mut reader = TestHostCacheFactReader::default();
            reader.insert_u32("hw.nperflevels", 1);
            reader.add_performance_level(
                0,
                performance_level(
                    physical,
                    logical,
                    64 * KIB,
                    128 * KIB,
                    Some((4 * MIB, share)),
                    None,
                ),
            );
            assert!(matches!(
                prepare_arm64_cache_with(source, 1, &reader),
                Err(HvfArm64CacheTopologyError::InvalidHostSharing)
            ));
        }

        prepare_arm64_cache_with(source, 32, &matching_two_level_reader())
            .expect("host core count proves sharing but does not cap admitted guest vCPUs");
    }

    #[test]
    fn rejects_non_nested_three_level_sharing() {
        let mut geometry = [[u64::MAX; 8]; 2];
        geometry[0][0] = legacy_ccsidr(64, 128, 8);
        geometry[1][0] = legacy_ccsidr(64, 512, 4);
        geometry[0][1] = legacy_ccsidr(128, 2048, 16);
        geometry[0][2] = legacy_ccsidr(128, 4096, 16);
        let source = source(0, ctr(64), 0x123, 4, geometry);
        let mut reader = TestHostCacheFactReader::default();
        reader.insert_u32("hw.nperflevels", 1);
        reader.add_performance_level(
            0,
            performance_level(
                12,
                12,
                64 * KIB,
                128 * KIB,
                Some((4 * MIB, 4)),
                Some((8 * MIB, 6)),
            ),
        );

        assert!(matches!(
            prepare_arm64_cache_with(source, 1, &reader),
            Err(HvfArm64CacheTopologyError::InvalidHostSharing)
        ));
    }

    #[test]
    fn unified_l1_requires_both_public_l1_sizes_to_match() {
        let mut geometry = [[u64::MAX; 8]; 2];
        geometry[0][0] = legacy_ccsidr(64, 128, 8);
        let source = source(0, ctr(64), 4, 4, geometry);
        let mut reader = TestHostCacheFactReader::default();
        reader.insert_u32("hw.nperflevels", 1);
        reader.add_performance_level(0, performance_level(4, 4, 64 * KIB, 64 * KIB, None, None));

        let hierarchy = prepare_arm64_cache_with(source, 1, &reader)
            .expect("a unified L1 should match equal public instruction and data sizes")
            .into_parts()
            .1;
        assert_eq!(hierarchy.caches().len(), 1);
        assert_eq!(
            hierarchy.caches()[0].cache_type(),
            Arm64FdtCacheType::Unified
        );

        reader.insert_u64("hw.perflevel0.l1icachesize", 128 * KIB);
        assert!(matches!(
            prepare_arm64_cache_with(source, 1, &reader),
            Err(HvfArm64CacheTopologyError::NoMatchingPerformanceLevel)
        ));
    }

    #[test]
    fn cache_values_are_redacted_from_debug_output() {
        let source = split_l1_l2_source(false);
        let prepared = prepare_arm64_cache_with(source, 4, &matching_two_level_reader())
            .expect("fixture should be admitted");

        assert_eq!(
            format!("{source:?}"),
            "HvfArm64VcpuCacheFdtSource { cache_identity: \"<redacted>\" }"
        );
        assert_eq!(
            format!("{prepared:?}"),
            "PreparedHvfArm64Cache { cache_presentation: \"<redacted>\" }"
        );
    }
}
