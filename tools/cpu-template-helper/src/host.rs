//! Public macOS host-fact adapter for CPU fingerprints.

#[cfg(any(target_os = "macos", test))]
use crate::fingerprint::CPU_FINGERPRINT_FACT_MAX_BYTES;
use crate::fingerprint::{HostFingerprint, HostFingerprintProvider, HostFingerprintProviderError};

#[cfg(any(target_os = "macos", test))]
const RAW_C_STRING_MAX_BYTES: usize = CPU_FINGERPRINT_FACT_MAX_BYTES + 1;

/// Production host-fingerprint provider used by the public helper executable.
#[derive(Debug, Default)]
pub struct SystemHostFingerprintProvider;

impl SystemHostFingerprintProvider {
    /// Construct a stateless production provider.
    pub const fn new() -> Self {
        Self
    }
}

impl HostFingerprintProvider for SystemHostFingerprintProvider {
    fn capture(&mut self) -> Result<HostFingerprint, HostFingerprintProviderError> {
        #[cfg(target_os = "macos")]
        {
            capture_macos(&mut SystemMacosHostFactSource)
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(HostFingerprintProviderError::Unsupported)
        }
    }
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MacosStringSelector {
    Product,
    Target,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostFactSourceError {
    System,
    Size,
    Width,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone)]
struct RawKernelFacts {
    operating_system: Vec<u8>,
    release: Vec<u8>,
    machine: Vec<u8>,
}

#[cfg(any(target_os = "macos", test))]
trait MacosHostFactSource {
    fn uname(&mut self) -> Result<RawKernelFacts, HostFactSourceError>;

    fn sysctl_string(
        &mut self,
        selector: MacosStringSelector,
    ) -> Result<Option<Vec<u8>>, HostFactSourceError>;

    fn cpu_family(&mut self) -> Result<Option<u32>, HostFactSourceError>;
}

#[cfg(any(target_os = "macos", test))]
fn capture_macos(
    source: &mut impl MacosHostFactSource,
) -> Result<HostFingerprint, HostFingerprintProviderError> {
    let kernel = source
        .uname()
        .map_err(|_| HostFingerprintProviderError::Kernel)?;
    let operating_system = decode_fixed_c_array(&kernel.operating_system)
        .map_err(|_| HostFingerprintProviderError::Kernel)?;
    let release =
        decode_fixed_c_array(&kernel.release).map_err(|_| HostFingerprintProviderError::Kernel)?;
    let machine =
        decode_fixed_c_array(&kernel.machine).map_err(|_| HostFingerprintProviderError::Kernel)?;

    let product = source
        .sysctl_string(MacosStringSelector::Product)
        .map_err(|_| HostFingerprintProviderError::Product)?
        .map(|raw| decode_sysctl_c_string(&raw))
        .transpose()
        .map_err(|_| HostFingerprintProviderError::Product)?;
    let target = source
        .sysctl_string(MacosStringSelector::Target)
        .map_err(|_| HostFingerprintProviderError::Target)?
        .map(|raw| decode_sysctl_c_string(&raw))
        .transpose()
        .map_err(|_| HostFingerprintProviderError::Target)?;
    let cpu_family = source
        .cpu_family()
        .map_err(|_| HostFingerprintProviderError::CpuFamily)?;

    HostFingerprint::try_macos(
        operating_system,
        release,
        machine,
        product,
        target,
        cpu_family,
    )
    .map_err(|_| HostFingerprintProviderError::Validation)
}

#[cfg(any(target_os = "macos", test))]
fn decode_fixed_c_array(raw: &[u8]) -> Result<String, HostFactSourceError> {
    if raw.len() != RAW_C_STRING_MAX_BYTES {
        return Err(HostFactSourceError::Size);
    }
    let terminator = raw
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(HostFactSourceError::Size)?;
    if terminator == 0
        || raw
            .get(terminator..)
            .is_none_or(|tail| !tail.iter().all(|byte| *byte == 0))
    {
        return Err(HostFactSourceError::Size);
    }
    std::str::from_utf8(raw.get(..terminator).ok_or(HostFactSourceError::Size)?)
        .map(str::to_owned)
        .map_err(|_| HostFactSourceError::System)
}

#[cfg(any(target_os = "macos", test))]
fn decode_sysctl_c_string(raw: &[u8]) -> Result<String, HostFactSourceError> {
    if !(2..=RAW_C_STRING_MAX_BYTES).contains(&raw.len())
        || raw.last() != Some(&0)
        || raw
            .get(..raw.len().saturating_sub(1))
            .is_none_or(|value| value.contains(&0))
    {
        return Err(HostFactSourceError::Size);
    }
    std::str::from_utf8(
        raw.get(..raw.len().saturating_sub(1))
            .ok_or(HostFactSourceError::Size)?,
    )
    .map(str::to_owned)
    .map_err(|_| HostFactSourceError::System)
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct SystemMacosHostFactSource;

#[cfg(target_os = "macos")]
impl MacosHostFactSource for SystemMacosHostFactSource {
    fn uname(&mut self) -> Result<RawKernelFacts, HostFactSourceError> {
        let mut raw = std::mem::MaybeUninit::<libc::utsname>::zeroed();
        // SAFETY: `raw` points to writable storage for exactly one `utsname`; a successful call
        // initializes the complete structure before it is assumed initialized below.
        if unsafe { libc::uname(raw.as_mut_ptr()) } != 0 {
            return Err(HostFactSourceError::System);
        }
        // SAFETY: the successful `uname` call above initialized the structure.
        let raw = unsafe { raw.assume_init() };
        Ok(RawKernelFacts {
            operating_system: c_chars_to_bytes(&raw.sysname),
            release: c_chars_to_bytes(&raw.release),
            machine: c_chars_to_bytes(&raw.machine),
        })
    }

    fn sysctl_string(
        &mut self,
        selector: MacosStringSelector,
    ) -> Result<Option<Vec<u8>>, HostFactSourceError> {
        read_sysctl_string(selector_name(selector))
    }

    fn cpu_family(&mut self) -> Result<Option<u32>, HostFactSourceError> {
        read_sysctl_u32(b"hw.cpufamily\0")
    }
}

#[cfg(target_os = "macos")]
fn c_chars_to_bytes<const N: usize>(raw: &[libc::c_char; N]) -> Vec<u8> {
    raw.iter().map(|byte| *byte as u8).collect()
}

#[cfg(target_os = "macos")]
const fn selector_name(selector: MacosStringSelector) -> &'static [u8] {
    match selector {
        MacosStringSelector::Product => b"hw.product\0",
        MacosStringSelector::Target => b"hw.target\0",
    }
}

#[cfg(target_os = "macos")]
fn read_sysctl_string(name: &[u8]) -> Result<Option<Vec<u8>>, HostFactSourceError> {
    let mut length = 0_usize;
    // SAFETY: `name` is one of the static NUL-terminated selector byte strings above; a null output
    // pointer with `length` requests the required byte count and no new value is supplied.
    if unsafe {
        libc::sysctlbyname(
            name.as_ptr().cast(),
            std::ptr::null_mut(),
            &raw mut length,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return classify_sysctl_error();
    }
    if !(2..=RAW_C_STRING_MAX_BYTES).contains(&length) {
        return Err(HostFactSourceError::Size);
    }

    let requested = length;
    let mut bytes = vec![0_u8; requested];
    // SAFETY: `bytes` owns `requested` writable bytes, `length` describes that capacity, `name` is
    // NUL-terminated, and the call supplies no new value.
    if unsafe {
        libc::sysctlbyname(
            name.as_ptr().cast(),
            bytes.as_mut_ptr().cast(),
            &raw mut length,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return classify_sysctl_error();
    }
    if length != requested {
        return Err(HostFactSourceError::Size);
    }
    Ok(Some(bytes))
}

#[cfg(target_os = "macos")]
fn read_sysctl_u32(name: &[u8]) -> Result<Option<u32>, HostFactSourceError> {
    let mut value = 0_u32;
    let mut length = std::mem::size_of::<u32>();
    // SAFETY: `name` is the static NUL-terminated `hw.cpufamily` selector, `value` provides exactly
    // `length` writable bytes, and the call supplies no new value.
    if unsafe {
        libc::sysctlbyname(
            name.as_ptr().cast(),
            (&raw mut value).cast(),
            &raw mut length,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return classify_sysctl_error();
    }
    if length != std::mem::size_of::<u32>() {
        return Err(HostFactSourceError::Width);
    }
    Ok(Some(value))
}

#[cfg(target_os = "macos")]
fn classify_sysctl_error<T>() -> Result<Option<T>, HostFactSourceError> {
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::ENOENT | libc::EINVAL) => Ok(None),
        _ => Err(HostFactSourceError::System),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing)]

    use crate::fingerprint::CpuFingerprintPlatform;

    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Call {
        Uname,
        Product,
        Target,
        CpuFamily,
    }

    struct FakeSource {
        kernel: Result<RawKernelFacts, HostFactSourceError>,
        product: Result<Option<Vec<u8>>, HostFactSourceError>,
        target: Result<Option<Vec<u8>>, HostFactSourceError>,
        cpu_family: Result<Option<u32>, HostFactSourceError>,
        calls: Vec<Call>,
    }

    impl MacosHostFactSource for FakeSource {
        fn uname(&mut self) -> Result<RawKernelFacts, HostFactSourceError> {
            self.calls.push(Call::Uname);
            self.kernel.clone()
        }

        fn sysctl_string(
            &mut self,
            selector: MacosStringSelector,
        ) -> Result<Option<Vec<u8>>, HostFactSourceError> {
            match selector {
                MacosStringSelector::Product => {
                    self.calls.push(Call::Product);
                    self.product.clone()
                }
                MacosStringSelector::Target => {
                    self.calls.push(Call::Target);
                    self.target.clone()
                }
            }
        }

        fn cpu_family(&mut self) -> Result<Option<u32>, HostFactSourceError> {
            self.calls.push(Call::CpuFamily);
            self.cpu_family
        }
    }

    fn fixed(value: &[u8]) -> Vec<u8> {
        let mut raw = vec![0; RAW_C_STRING_MAX_BYTES];
        raw[..value.len()].copy_from_slice(value);
        raw
    }

    fn source() -> FakeSource {
        FakeSource {
            kernel: Ok(RawKernelFacts {
                operating_system: fixed(b"Darwin"),
                release: fixed(b"25.5.0"),
                machine: fixed(b"arm64"),
            }),
            product: Ok(Some(b"Mac16,1\0".to_vec())),
            target: Ok(Some(b"J475cAP\0".to_vec())),
            cpu_family: Ok(Some(0x1b588bb3)),
            calls: Vec::new(),
        }
    }

    #[test]
    fn capture_queries_exact_public_facts_once_in_order() {
        let mut source = source();
        let host = capture_macos(&mut source).expect("host facts should validate");

        assert_eq!(
            source.calls,
            [Call::Uname, Call::Product, Call::Target, Call::CpuFamily]
        );
        assert_eq!(host.platform(), CpuFingerprintPlatform::Macos);
        assert_eq!(host.operating_system(), "Darwin");
        assert_eq!(host.release(), "25.5.0");
        assert_eq!(host.machine(), "arm64");
        assert_eq!(host.macos_product(), Some("Mac16,1"));
        assert_eq!(host.macos_target(), Some("J475cAP"));
        assert_eq!(host.macos_cpu_family(), Some(0x1b588bb3));
    }

    #[test]
    fn missing_public_selectors_are_explicitly_unavailable() {
        let mut source = source();
        source.product = Ok(None);
        source.target = Ok(None);
        source.cpu_family = Ok(None);

        let host = capture_macos(&mut source).expect("missing facts should validate");
        assert_eq!(host.macos_product(), None);
        assert_eq!(host.macos_target(), None);
        assert_eq!(host.macos_cpu_family(), None);
    }

    #[test]
    fn failures_stop_at_the_exact_capture_stage() {
        let mut product_failure = source();
        product_failure.product = Err(HostFactSourceError::System);
        assert_eq!(
            capture_macos(&mut product_failure),
            Err(HostFingerprintProviderError::Product)
        );
        assert_eq!(product_failure.calls, [Call::Uname, Call::Product]);

        let mut target_failure = source();
        target_failure.target = Err(HostFactSourceError::System);
        assert_eq!(
            capture_macos(&mut target_failure),
            Err(HostFingerprintProviderError::Target)
        );
        assert_eq!(
            target_failure.calls,
            [Call::Uname, Call::Product, Call::Target]
        );

        let mut family_failure = source();
        family_failure.cpu_family = Err(HostFactSourceError::Width);
        assert_eq!(
            capture_macos(&mut family_failure),
            Err(HostFingerprintProviderError::CpuFamily)
        );
    }

    #[test]
    fn c_string_decoders_reject_malformed_and_unbounded_values() {
        assert_eq!(
            decode_fixed_c_array(&fixed(b"Darwin")).as_deref(),
            Ok("Darwin")
        );
        assert_eq!(
            decode_sysctl_c_string(b"Mac16,1\0").as_deref(),
            Ok("Mac16,1")
        );
        let mut maximum = vec![b'x'; RAW_C_STRING_MAX_BYTES];
        *maximum.last_mut().expect("maximum fixture is nonempty") = 0;
        assert_eq!(
            decode_sysctl_c_string(&maximum)
                .expect("maximum bounded C string should decode")
                .len(),
            CPU_FINGERPRINT_FACT_MAX_BYTES
        );

        for malformed in [
            Vec::new(),
            vec![0],
            b"missing-terminator".to_vec(),
            b"interior\0terminator\0".to_vec(),
            vec![b'x'; RAW_C_STRING_MAX_BYTES + 1],
            vec![0xff, 0],
        ] {
            assert!(decode_sysctl_c_string(&malformed).is_err());
        }

        let mut trailing_garbage = fixed(b"Darwin");
        trailing_garbage[RAW_C_STRING_MAX_BYTES - 1] = b'x';
        assert!(decode_fixed_c_array(&trailing_garbage).is_err());
        assert!(decode_fixed_c_array(&fixed(&vec![b'x'; RAW_C_STRING_MAX_BYTES])).is_err());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn production_provider_is_explicitly_unsupported_off_macos() {
        assert_eq!(
            SystemHostFingerprintProvider::new().capture(),
            Err(HostFingerprintProviderError::Unsupported)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn production_provider_returns_only_the_reviewed_macos_shape() {
        let host = SystemHostFingerprintProvider::new()
            .capture()
            .expect("public host facts should be readable");
        assert_eq!(host.platform(), CpuFingerprintPlatform::Macos);
        assert_eq!(host.operating_system(), "Darwin");
        assert_eq!(host.machine(), "arm64");
    }
}
