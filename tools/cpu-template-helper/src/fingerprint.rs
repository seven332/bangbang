//! Strict platform-tagged CPU-fingerprint document and dump orchestration.

use std::fmt;

use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{Value, value::RawValue};

use crate::document::{
    CpuTemplateDocument, CpuTemplateDocumentError, decode_cpu_template_document,
};
use crate::profile::{
    CpuTemplateOperationError, EffectiveCpuTemplateProvider, capture_document_with_provider,
};
use crate::projection::PreparedCpuTemplateInspection;
use crate::{CPU_TEMPLATE_DOCUMENT_MAX_BYTES, HelperExitClass};

/// Current closed CPU-fingerprint schema version.
pub const CPU_FINGERPRINT_SCHEMA_VERSION: u32 = 1;
/// Producer identity used by the Bangbang helper.
pub const CPU_FINGERPRINT_PRODUCER: &str = "bangbang-cpu-template-helper";
/// Pinned Firecracker command/document compatibility target.
pub const CPU_FINGERPRINT_FIRECRACKER_COMPATIBILITY: &str = "1.16.0";
/// Maximum UTF-8 byte length of one producer version.
pub const CPU_FINGERPRINT_PRODUCER_VERSION_MAX_BYTES: usize = 64;
/// Maximum UTF-8 byte length of one kernel or platform fact.
pub const CPU_FINGERPRINT_FACT_MAX_BYTES: usize = 255;

const VALUE_REDACTED: &str = "<redacted>";
const MACOS_OPERATING_SYSTEM: &str = "Darwin";
const MACOS_MACHINE: &str = "arm64";
const LINUX_OPERATING_SYSTEM: &str = "Linux";
const LINUX_MACHINE: &str = "aarch64";

/// Platform represented by a closed fingerprint host variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuFingerprintPlatform {
    Macos,
    Linux,
}

#[derive(Clone, PartialEq, Eq)]
struct NormalizedFact(String);

impl NormalizedFact {
    fn try_new(value: String) -> Result<Self, CpuFingerprintDocumentError> {
        if value.is_empty()
            || value.len() > CPU_FINGERPRINT_FACT_MAX_BYTES
            || value.trim() != value
            || value.chars().any(char::is_control)
        {
            return Err(CpuFingerprintDocumentError::NonCanonical);
        }
        Ok(Self(value))
    }

    fn get(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, PartialEq, Eq)]
struct KernelFingerprint {
    operating_system: NormalizedFact,
    release: NormalizedFact,
    machine: NormalizedFact,
}

#[derive(Clone, PartialEq, Eq)]
enum HostFingerprintVariant {
    Macos {
        product: Option<NormalizedFact>,
        target: Option<NormalizedFact>,
        cpu_family: Option<u32>,
    },
    Linux {
        microcode_version: NormalizedFact,
        bios_version: NormalizedFact,
        bios_revision: NormalizedFact,
    },
}

/// Normalized common kernel facts and one closed platform host variant.
#[derive(Clone, PartialEq, Eq)]
pub struct HostFingerprint {
    kernel: KernelFingerprint,
    variant: HostFingerprintVariant,
}

impl HostFingerprint {
    /// Construct and validate one macOS/Apple Silicon host fingerprint.
    pub fn try_macos(
        operating_system: String,
        release: String,
        machine: String,
        product: Option<String>,
        target: Option<String>,
        cpu_family: Option<u32>,
    ) -> Result<Self, CpuFingerprintDocumentError> {
        let kernel = KernelFingerprint {
            operating_system: NormalizedFact::try_new(operating_system)?,
            release: NormalizedFact::try_new(release)?,
            machine: NormalizedFact::try_new(machine)?,
        };
        if kernel.operating_system.get() != MACOS_OPERATING_SYSTEM
            || kernel.machine.get() != MACOS_MACHINE
        {
            return Err(CpuFingerprintDocumentError::NonCanonical);
        }
        Ok(Self {
            kernel,
            variant: HostFingerprintVariant::Macos {
                product: product.map(NormalizedFact::try_new).transpose()?,
                target: target.map(NormalizedFact::try_new).transpose()?,
                cpu_family,
            },
        })
    }

    /// Construct and validate one Linux/arm64 host fingerprint.
    pub fn try_linux(
        operating_system: String,
        release: String,
        machine: String,
        microcode_version: String,
        bios_version: String,
        bios_revision: String,
    ) -> Result<Self, CpuFingerprintDocumentError> {
        let kernel = KernelFingerprint {
            operating_system: NormalizedFact::try_new(operating_system)?,
            release: NormalizedFact::try_new(release)?,
            machine: NormalizedFact::try_new(machine)?,
        };
        if kernel.operating_system.get() != LINUX_OPERATING_SYSTEM
            || kernel.machine.get() != LINUX_MACHINE
        {
            return Err(CpuFingerprintDocumentError::NonCanonical);
        }
        Ok(Self {
            kernel,
            variant: HostFingerprintVariant::Linux {
                microcode_version: NormalizedFact::try_new(microcode_version)?,
                bios_version: NormalizedFact::try_new(bios_version)?,
                bios_revision: NormalizedFact::try_new(bios_revision)?,
            },
        })
    }

    /// Return the exact operating-system identity.
    pub fn operating_system(&self) -> &str {
        self.kernel.operating_system.get()
    }

    /// Return the exact kernel release.
    pub fn release(&self) -> &str {
        self.kernel.release.get()
    }

    /// Return the exact machine identity.
    pub fn machine(&self) -> &str {
        self.kernel.machine.get()
    }

    /// Return the closed platform tag.
    pub const fn platform(&self) -> CpuFingerprintPlatform {
        match &self.variant {
            HostFingerprintVariant::Macos { .. } => CpuFingerprintPlatform::Macos,
            HostFingerprintVariant::Linux { .. } => CpuFingerprintPlatform::Linux,
        }
    }

    /// Return the macOS product fact when this is a macOS variant and it is available.
    pub fn macos_product(&self) -> Option<&str> {
        match &self.variant {
            HostFingerprintVariant::Macos { product, .. } => {
                product.as_ref().map(NormalizedFact::get)
            }
            HostFingerprintVariant::Linux { .. } => None,
        }
    }

    /// Return the macOS target fact when this is a macOS variant and it is available.
    pub fn macos_target(&self) -> Option<&str> {
        match &self.variant {
            HostFingerprintVariant::Macos { target, .. } => {
                target.as_ref().map(NormalizedFact::get)
            }
            HostFingerprintVariant::Linux { .. } => None,
        }
    }

    /// Return the exact macOS CPU-family identity when available.
    pub const fn macos_cpu_family(&self) -> Option<u32> {
        match &self.variant {
            HostFingerprintVariant::Macos { cpu_family, .. } => *cpu_family,
            HostFingerprintVariant::Linux { .. } => None,
        }
    }

    /// Return Linux microcode identity for a Linux variant.
    pub fn linux_microcode_version(&self) -> Option<&str> {
        match &self.variant {
            HostFingerprintVariant::Linux {
                microcode_version, ..
            } => Some(microcode_version.get()),
            HostFingerprintVariant::Macos { .. } => None,
        }
    }

    /// Return Linux BIOS version for a Linux variant.
    pub fn linux_bios_version(&self) -> Option<&str> {
        match &self.variant {
            HostFingerprintVariant::Linux { bios_version, .. } => Some(bios_version.get()),
            HostFingerprintVariant::Macos { .. } => None,
        }
    }

    /// Return Linux BIOS revision for a Linux variant.
    pub fn linux_bios_revision(&self) -> Option<&str> {
        match &self.variant {
            HostFingerprintVariant::Linux { bios_revision, .. } => Some(bios_revision.get()),
            HostFingerprintVariant::Macos { .. } => None,
        }
    }
}

impl fmt::Debug for HostFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostFingerprint")
            .field("platform", &self.platform())
            .field("facts", &VALUE_REDACTED)
            .finish()
    }
}

/// One normalized versioned CPU fingerprint.
#[derive(Clone, PartialEq, Eq)]
pub struct CpuFingerprintDocument {
    producer_version: String,
    host: HostFingerprint,
    guest_cpu_config: CpuTemplateDocument,
}

impl CpuFingerprintDocument {
    /// Construct a document produced by the running helper version.
    pub fn new_current(
        host: HostFingerprint,
        guest_cpu_config: CpuTemplateDocument,
    ) -> Result<Self, CpuFingerprintDocumentError> {
        Self::try_new(env!("CARGO_PKG_VERSION").to_owned(), host, guest_cpu_config)
    }

    fn try_new(
        producer_version: String,
        host: HostFingerprint,
        guest_cpu_config: CpuTemplateDocument,
    ) -> Result<Self, CpuFingerprintDocumentError> {
        validate_producer_version(&producer_version)?;
        Ok(Self {
            producer_version,
            host,
            guest_cpu_config,
        })
    }

    /// Return the producing helper's canonical SemVer.
    pub fn producer_version(&self) -> &str {
        &self.producer_version
    }

    /// Return normalized kernel and platform host facts.
    pub const fn host(&self) -> &HostFingerprint {
        &self.host
    }

    /// Return the normalized effective guest CPU configuration.
    pub const fn guest_cpu_config(&self) -> &CpuTemplateDocument {
        &self.guest_cpu_config
    }

    /// Encode fixed-field pretty JSON with exactly one trailing newline.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, CpuFingerprintEncodeError> {
        let wire = EncodeDocument {
            schema_version: CPU_FINGERPRINT_SCHEMA_VERSION,
            producer: EncodeProducer {
                name: CPU_FINGERPRINT_PRODUCER,
                version: &self.producer_version,
                firecracker_compatibility: CPU_FINGERPRINT_FIRECRACKER_COMPATIBILITY,
            },
            kernel: EncodeKernel {
                operating_system: self.host.kernel.operating_system.get(),
                release: self.host.kernel.release.get(),
                machine: self.host.kernel.machine.get(),
            },
            host: EncodeHost::from(&self.host.variant),
            guest_cpu_config: &self.guest_cpu_config,
        };
        let mut bytes = serde_json::to_vec_pretty(&wire)
            .map_err(|_| CpuFingerprintEncodeError::Serialization)?;
        bytes.push(b'\n');
        if bytes.len() > CPU_TEMPLATE_DOCUMENT_MAX_BYTES {
            return Err(CpuFingerprintEncodeError::TooLarge);
        }
        Ok(bytes)
    }

    /// Encode, strictly reparse, and require byte-identical canonical output.
    pub fn validated_canonical_bytes(&self) -> Result<Vec<u8>, CpuFingerprintEncodeError> {
        let bytes = self.canonical_bytes()?;
        let contents =
            std::str::from_utf8(&bytes).map_err(|_| CpuFingerprintEncodeError::Validation)?;
        let reparsed = decode_cpu_fingerprint_document(contents)
            .map_err(|_| CpuFingerprintEncodeError::Validation)?;
        if reparsed != *self || reparsed.canonical_bytes().as_deref() != Ok(bytes.as_slice()) {
            return Err(CpuFingerprintEncodeError::Validation);
        }
        Ok(bytes)
    }
}

impl fmt::Debug for CpuFingerprintDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CpuFingerprintDocument")
            .field("producer_version", &self.producer_version)
            .field("platform", &self.host.platform())
            .field("host_facts", &VALUE_REDACTED)
            .field("guest_cpu_config", &VALUE_REDACTED)
            .finish()
    }
}

/// Strict CPU-fingerprint decode or construction failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuFingerprintDocumentError {
    TooLarge,
    Malformed,
    NonCanonical,
    Unsupported,
}

impl fmt::Display for CpuFingerprintDocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TooLarge => "CPU-fingerprint document exceeds the size limit",
            Self::Malformed => "CPU-fingerprint document is malformed",
            Self::NonCanonical => "CPU-fingerprint document is not semantically canonical",
            Self::Unsupported => "CPU-fingerprint document requests unsupported state",
        })
    }
}

impl std::error::Error for CpuFingerprintDocumentError {}

/// Canonical CPU-fingerprint encoding failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuFingerprintEncodeError {
    Serialization,
    TooLarge,
    Validation,
}

impl fmt::Display for CpuFingerprintEncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Serialization => "CPU-fingerprint document could not be encoded",
            Self::TooLarge => "encoded CPU-fingerprint document exceeds the size limit",
            Self::Validation => "encoded CPU-fingerprint document failed strict validation",
        })
    }
}

impl std::error::Error for CpuFingerprintEncodeError {}

/// Decode and normalize one version-1 CPU-fingerprint document.
pub fn decode_cpu_fingerprint_document(
    contents: &str,
) -> Result<CpuFingerprintDocument, CpuFingerprintDocumentError> {
    if contents.len() > CPU_TEMPLATE_DOCUMENT_MAX_BYTES {
        return Err(CpuFingerprintDocumentError::TooLarge);
    }
    let wire: DecodeDocument =
        serde_json::from_str(contents).map_err(|_| CpuFingerprintDocumentError::Malformed)?;
    if wire.schema_version != CPU_FINGERPRINT_SCHEMA_VERSION
        || wire.producer.name != CPU_FINGERPRINT_PRODUCER
        || wire.producer.firecracker_compatibility != CPU_FINGERPRINT_FIRECRACKER_COMPATIBILITY
    {
        return Err(CpuFingerprintDocumentError::Unsupported);
    }
    validate_producer_version(&wire.producer.version)?;

    let host = match wire.host {
        DecodeHost::Macos {
            product,
            target,
            cpu_family,
        } => HostFingerprint::try_macos(
            wire.kernel.operating_system,
            wire.kernel.release,
            wire.kernel.machine,
            decode_nullable_string(product)?,
            decode_nullable_string(target)?,
            decode_nullable_string(cpu_family)?
                .map(|value| decode_cpu_family(&value))
                .transpose()?,
        )?,
        DecodeHost::Linux {
            microcode_version,
            bios_version,
            bios_revision,
        } => HostFingerprint::try_linux(
            wire.kernel.operating_system,
            wire.kernel.release,
            wire.kernel.machine,
            microcode_version,
            bios_version,
            bios_revision,
        )?,
    };
    let guest_cpu_config = decode_cpu_template_document(wire.guest_cpu_config.get())
        .map_err(map_cpu_template_document_error)?;
    CpuFingerprintDocument::try_new(wire.producer.version, host, guest_cpu_config)
}

fn validate_producer_version(version: &str) -> Result<(), CpuFingerprintDocumentError> {
    if version.is_empty() || version.len() > CPU_FINGERPRINT_PRODUCER_VERSION_MAX_BYTES {
        return Err(CpuFingerprintDocumentError::NonCanonical);
    }
    let parsed = Version::parse(version).map_err(|_| CpuFingerprintDocumentError::NonCanonical)?;
    if parsed.to_string() != version {
        return Err(CpuFingerprintDocumentError::NonCanonical);
    }
    Ok(())
}

fn decode_cpu_family(value: &str) -> Result<u32, CpuFingerprintDocumentError> {
    let digits = value
        .strip_prefix("0x")
        .filter(|digits| digits.len() == 8)
        .ok_or(CpuFingerprintDocumentError::NonCanonical)?;
    if !digits
        .bytes()
        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(CpuFingerprintDocumentError::NonCanonical);
    }
    u32::from_str_radix(digits, 16).map_err(|_| CpuFingerprintDocumentError::NonCanonical)
}

fn decode_nullable_string(value: Value) -> Result<Option<String>, CpuFingerprintDocumentError> {
    match value {
        Value::Null => Ok(None),
        Value::String(value) => Ok(Some(value)),
        _ => Err(CpuFingerprintDocumentError::Malformed),
    }
}

fn map_cpu_template_document_error(error: CpuTemplateDocumentError) -> CpuFingerprintDocumentError {
    match error {
        CpuTemplateDocumentError::TooLarge => CpuFingerprintDocumentError::TooLarge,
        CpuTemplateDocumentError::Malformed => CpuFingerprintDocumentError::Malformed,
        CpuTemplateDocumentError::NonCanonical => CpuFingerprintDocumentError::NonCanonical,
        CpuTemplateDocumentError::Unsupported => CpuFingerprintDocumentError::Unsupported,
    }
}

#[derive(Serialize)]
struct EncodeDocument<'a> {
    schema_version: u32,
    producer: EncodeProducer<'a>,
    kernel: EncodeKernel<'a>,
    host: EncodeHost<'a>,
    guest_cpu_config: &'a CpuTemplateDocument,
}

#[derive(Serialize)]
struct EncodeProducer<'a> {
    name: &'a str,
    version: &'a str,
    firecracker_compatibility: &'a str,
}

#[derive(Serialize)]
struct EncodeKernel<'a> {
    operating_system: &'a str,
    release: &'a str,
    machine: &'a str,
}

#[derive(Serialize)]
#[serde(tag = "platform", rename_all = "lowercase")]
enum EncodeHost<'a> {
    Macos {
        product: Option<&'a str>,
        target: Option<&'a str>,
        cpu_family: Option<String>,
    },
    Linux {
        microcode_version: &'a str,
        bios_version: &'a str,
        bios_revision: &'a str,
    },
}

impl<'a> From<&'a HostFingerprintVariant> for EncodeHost<'a> {
    fn from(value: &'a HostFingerprintVariant) -> Self {
        match value {
            HostFingerprintVariant::Macos {
                product,
                target,
                cpu_family,
            } => Self::Macos {
                product: product.as_ref().map(NormalizedFact::get),
                target: target.as_ref().map(NormalizedFact::get),
                cpu_family: cpu_family.map(|value| format!("0x{value:08x}")),
            },
            HostFingerprintVariant::Linux {
                microcode_version,
                bios_version,
                bios_revision,
            } => Self::Linux {
                microcode_version: microcode_version.get(),
                bios_version: bios_version.get(),
                bios_revision: bios_revision.get(),
            },
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecodeDocument {
    schema_version: u32,
    producer: DecodeProducer,
    kernel: DecodeKernel,
    host: DecodeHost,
    guest_cpu_config: Box<RawValue>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecodeProducer {
    name: String,
    version: String,
    firecracker_compatibility: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecodeKernel {
    operating_system: String,
    release: String,
    machine: String,
}

#[derive(Deserialize)]
#[serde(tag = "platform", rename_all = "lowercase", deny_unknown_fields)]
enum DecodeHost {
    Macos {
        product: Value,
        target: Value,
        cpu_family: Value,
    },
    Linux {
        microcode_version: String,
        bios_version: String,
        bios_revision: String,
    },
}

/// Stable value-free failure stage returned by a host-fingerprint provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostFingerprintProviderError {
    Unsupported,
    Kernel,
    Product,
    Target,
    CpuFamily,
    Validation,
}

impl fmt::Display for HostFingerprintProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unsupported => "host fingerprint capture is unsupported",
            Self::Kernel => "kernel fingerprint capture failed",
            Self::Product => "host product fingerprint capture failed",
            Self::Target => "host target fingerprint capture failed",
            Self::CpuFamily => "host CPU-family fingerprint capture failed",
            Self::Validation => "host fingerprint validation failed",
        })
    }
}

impl std::error::Error for HostFingerprintProviderError {}

/// Platform adapter for one normalized host-fingerprint capture.
pub trait HostFingerprintProvider {
    fn capture(&mut self) -> Result<HostFingerprint, HostFingerprintProviderError>;
}

/// Complete fingerprint dump failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuFingerprintOperationError {
    Host(HostFingerprintProviderError),
    Guest(CpuTemplateOperationError),
    Document(CpuFingerprintDocumentError),
    Encoding(CpuFingerprintEncodeError),
}

impl CpuFingerprintOperationError {
    /// All dump orchestration failures are operational exit class 1.
    pub const fn exit_class(self) -> HelperExitClass {
        HelperExitClass::OperationalFailure
    }
}

impl fmt::Display for CpuFingerprintOperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Host(_) => "host fingerprint capture failed",
            Self::Guest(_) => "effective CPU fingerprint capture failed",
            Self::Document(_) | Self::Encoding(_) => "CPU fingerprint could not be encoded",
        })
    }
}

impl std::error::Error for CpuFingerprintOperationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Host(source) => Some(source),
            Self::Guest(source) => Some(source),
            Self::Document(source) => Some(source),
            Self::Encoding(source) => Some(source),
        }
    }
}

/// Capture host and effective guest facts, then return strictly validated canonical bytes.
pub fn dump_with_providers(
    host_provider: &mut impl HostFingerprintProvider,
    effective_provider: &mut impl EffectiveCpuTemplateProvider,
    request: &PreparedCpuTemplateInspection,
) -> Result<Vec<u8>, CpuFingerprintOperationError> {
    let host = host_provider
        .capture()
        .map_err(CpuFingerprintOperationError::Host)?;
    let guest_cpu_config = capture_document_with_provider(effective_provider, request)
        .map_err(CpuFingerprintOperationError::Guest)?;
    CpuFingerprintDocument::new_current(host, guest_cpu_config)
        .map_err(CpuFingerprintOperationError::Document)?
        .validated_canonical_bytes()
        .map_err(CpuFingerprintOperationError::Encoding)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn guest() -> CpuTemplateDocument {
        decode_cpu_template_document("{}").expect("empty guest template should normalize")
    }

    fn macos_host() -> HostFingerprint {
        HostFingerprint::try_macos(
            "Darwin".to_owned(),
            "25.5.0".to_owned(),
            "arm64".to_owned(),
            Some("Mac16,1".to_owned()),
            Some("J475cAP".to_owned()),
            Some(0x1b588bb3),
        )
        .expect("macOS fixture should validate")
    }

    #[test]
    fn macos_golden_bytes_round_trip_and_accept_other_canonical_producer_versions() {
        let document =
            CpuFingerprintDocument::try_new("2.3.4-beta.1".to_owned(), macos_host(), guest())
                .expect("fixture should validate");
        let expected = concat!(
            "{\n",
            "  \"schema_version\": 1,\n",
            "  \"producer\": {\n",
            "    \"name\": \"bangbang-cpu-template-helper\",\n",
            "    \"version\": \"2.3.4-beta.1\",\n",
            "    \"firecracker_compatibility\": \"1.16.0\"\n",
            "  },\n",
            "  \"kernel\": {\n",
            "    \"operating_system\": \"Darwin\",\n",
            "    \"release\": \"25.5.0\",\n",
            "    \"machine\": \"arm64\"\n",
            "  },\n",
            "  \"host\": {\n",
            "    \"platform\": \"macos\",\n",
            "    \"product\": \"Mac16,1\",\n",
            "    \"target\": \"J475cAP\",\n",
            "    \"cpu_family\": \"0x1b588bb3\"\n",
            "  },\n",
            "  \"guest_cpu_config\": {\n",
            "    \"kvm_capabilities\": [],\n",
            "    \"reg_modifiers\": [],\n",
            "    \"vcpu_features\": []\n",
            "  }\n",
            "}\n",
        );
        let bytes = document
            .validated_canonical_bytes()
            .expect("document should encode and reparse");
        assert_eq!(bytes, expected.as_bytes());
        assert_eq!(
            decode_cpu_fingerprint_document(expected),
            Ok(document.clone())
        );
        assert_eq!(
            decode_cpu_fingerprint_document(&format!("  {expected}\n")),
            Ok(document)
        );
    }

    #[test]
    fn unavailable_macos_facts_are_present_nulls() {
        let host = HostFingerprint::try_macos(
            "Darwin".to_owned(),
            "25.5.0".to_owned(),
            "arm64".to_owned(),
            None,
            None,
            None,
        )
        .expect("unavailable facts should validate");
        let bytes = CpuFingerprintDocument::new_current(host, guest())
            .expect("document should construct")
            .canonical_bytes()
            .expect("document should encode");
        let text = std::str::from_utf8(&bytes).expect("document should be UTF-8");
        let expected_host = concat!(
            "  \"host\": {\n",
            "    \"platform\": \"macos\",\n",
            "    \"product\": null,\n",
            "    \"target\": null,\n",
            "    \"cpu_family\": null\n",
            "  },\n",
        );
        assert!(text.contains(expected_host));

        for (name, field) in [
            ("product", "    \"product\": null,\n"),
            ("target", "    \"target\": null,\n"),
            ("cpu_family", "    \"cpu_family\": null\n"),
        ] {
            let mutated = text.replace(field, "");
            assert_ne!(mutated, text, "{name} field should be present");
            assert_eq!(
                decode_cpu_fingerprint_document(&mutated),
                Err(CpuFingerprintDocumentError::Malformed),
                "missing {name} must fail"
            );
        }
    }

    #[test]
    fn linux_variant_is_closed_and_preserves_linux_meanings() {
        let host = HostFingerprint::try_linux(
            "Linux".to_owned(),
            "6.1.141".to_owned(),
            "aarch64".to_owned(),
            "0x00000042".to_owned(),
            "1.2.3".to_owned(),
            "4.5".to_owned(),
        )
        .expect("Linux fixture should validate");
        let document =
            CpuFingerprintDocument::new_current(host, guest()).expect("document should construct");
        let bytes = document
            .validated_canonical_bytes()
            .expect("Linux document should round trip");
        let text = std::str::from_utf8(&bytes).expect("document should be UTF-8");
        let expected_host = concat!(
            "  \"host\": {\n",
            "    \"platform\": \"linux\",\n",
            "    \"microcode_version\": \"0x00000042\",\n",
            "    \"bios_version\": \"1.2.3\",\n",
            "    \"bios_revision\": \"4.5\"\n",
            "  },\n",
        );
        assert!(text.contains(expected_host));
        let reparsed = decode_cpu_fingerprint_document(text).expect("Linux document should decode");
        assert_eq!(reparsed.host().platform(), CpuFingerprintPlatform::Linux);
        assert_eq!(
            reparsed.host().linux_microcode_version(),
            Some("0x00000042")
        );
        assert_eq!(reparsed.host().linux_bios_version(), Some("1.2.3"));
        assert_eq!(reparsed.host().linux_bios_revision(), Some("4.5"));

        let mixed = String::from_utf8(bytes)
            .expect("document should be UTF-8")
            .replace("\"bios_revision\": \"4.5\"", "\"product\": null");
        assert_eq!(
            decode_cpu_fingerprint_document(&mixed),
            Err(CpuFingerprintDocumentError::Malformed)
        );
    }

    #[test]
    fn rejects_unsupported_duplicate_mixed_and_noncanonical_inputs() {
        let text = String::from_utf8(
            CpuFingerprintDocument::new_current(macos_host(), guest())
                .expect("document should construct")
                .canonical_bytes()
                .expect("document should encode"),
        )
        .expect("document should be UTF-8");

        for mutated in [
            text.replace("\"schema_version\": 1", "\"schema_version\": 2"),
            text.replace(CPU_FINGERPRINT_PRODUCER, "another-helper"),
            text.replace(CPU_FINGERPRINT_FIRECRACKER_COMPATIBILITY, "1.17.0"),
        ] {
            assert_eq!(
                decode_cpu_fingerprint_document(&mutated),
                Err(CpuFingerprintDocumentError::Unsupported)
            );
        }

        for mutated in [
            text.replace("\"version\": \"0.1.0\"", "\"version\": \"01.0.0\""),
            text.replace("\"release\": \"25.5.0\"", "\"release\": \" 25.5.0\""),
            text.replace(
                "\"cpu_family\": \"0x1b588bb3\"",
                "\"cpu_family\": \"0X1B588BB3\"",
            ),
            text.replace(
                "\"operating_system\": \"Darwin\"",
                "\"operating_system\": \"Linux\"",
            ),
        ] {
            assert_eq!(
                decode_cpu_fingerprint_document(&mutated),
                Err(CpuFingerprintDocumentError::NonCanonical)
            );
        }

        let duplicate = text.replace(
            "\"schema_version\": 1,",
            "\"schema_version\": 1,\n  \"schema_version\": 1,",
        );
        assert_eq!(
            decode_cpu_fingerprint_document(&duplicate),
            Err(CpuFingerprintDocumentError::Malformed)
        );
        let unknown = text.replace(
            "\"product\": \"Mac16,1\",",
            "\"product\": \"Mac16,1\",\n    \"serial\": \"forbidden\",",
        );
        assert_eq!(
            decode_cpu_fingerprint_document(&unknown),
            Err(CpuFingerprintDocumentError::Malformed)
        );
    }

    #[test]
    fn rejects_fact_and_document_bounds_without_exposing_values() {
        let maximum_version = format!("1.0.0+{}", "a".repeat(58));
        assert_eq!(
            maximum_version.len(),
            CPU_FINGERPRINT_PRODUCER_VERSION_MAX_BYTES
        );
        assert!(CpuFingerprintDocument::try_new(maximum_version, macos_host(), guest()).is_ok());
        let oversized_version = format!("1.0.0+{}", "a".repeat(59));
        assert_eq!(
            CpuFingerprintDocument::try_new(oversized_version, macos_host(), guest()),
            Err(CpuFingerprintDocumentError::NonCanonical)
        );

        assert!(
            HostFingerprint::try_macos(
                "Darwin".to_owned(),
                "x".repeat(CPU_FINGERPRINT_FACT_MAX_BYTES),
                "arm64".to_owned(),
                None,
                None,
                None,
            )
            .is_ok()
        );
        assert_eq!(
            HostFingerprint::try_macos(
                "Darwin".to_owned(),
                "x".repeat(CPU_FINGERPRINT_FACT_MAX_BYTES + 1),
                "arm64".to_owned(),
                None,
                None,
                None,
            ),
            Err(CpuFingerprintDocumentError::NonCanonical)
        );
        assert_eq!(
            decode_cpu_fingerprint_document(&" ".repeat(CPU_TEMPLATE_DOCUMENT_MAX_BYTES + 1)),
            Err(CpuFingerprintDocumentError::TooLarge)
        );

        let error = HostFingerprint::try_macos(
            "Darwin".to_owned(),
            "secret\nrelease".to_owned(),
            "arm64".to_owned(),
            None,
            None,
            None,
        )
        .expect_err("control characters should fail");
        assert!(!format!("{error:?}").contains("secret"));
    }
}
