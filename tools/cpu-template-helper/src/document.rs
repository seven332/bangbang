//! Strict custom-template decoding and canonical encoding.

use std::fmt;

use bangbang_api::config::parse_cpu_config_document;
use bangbang_runtime::cpu::{
    ArmRegisterModifier, CpuConfigArmRegisterModifier, CpuConfigArmRegisterWidth, CpuConfigError,
    CpuConfigInput, CustomCpuTemplate, arm64_cpu_template_register_descriptors,
};
use serde::{Serialize, Serializer};

use crate::CPU_TEMPLATE_DOCUMENT_MAX_BYTES;
use crate::projection::cpu_config_input_from_request;

const VALUE_REDACTED: &str = "<redacted>";

/// Failure while decoding a persisted custom CPU template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuTemplateDocumentError {
    TooLarge,
    Malformed,
    NonCanonical,
    Unsupported,
}

impl fmt::Display for CpuTemplateDocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TooLarge => "CPU-template document exceeds the size limit",
            Self::Malformed => "CPU-template document is malformed",
            Self::NonCanonical => "CPU-template document is not semantically canonical",
            Self::Unsupported => "CPU-template document requests unsupported state",
        })
    }
}

impl std::error::Error for CpuTemplateDocumentError {}

/// Failure while encoding a canonical custom CPU template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuTemplateEncodeError {
    Serialization,
    TooLarge,
}

impl fmt::Display for CpuTemplateEncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Serialization => "CPU-template document could not be encoded",
            Self::TooLarge => "encoded CPU-template document exceeds the size limit",
        })
    }
}

impl std::error::Error for CpuTemplateEncodeError {}

/// One normalized arm64 register modifier.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CpuTemplateModifier {
    identity: u64,
    width: CpuConfigArmRegisterWidth,
    filter: u128,
    value: u128,
}

impl CpuTemplateModifier {
    pub(crate) const fn new(
        identity: u64,
        width: CpuConfigArmRegisterWidth,
        filter: u128,
        value: u128,
    ) -> Self {
        Self {
            identity,
            width,
            filter,
            value,
        }
    }

    /// Return the compatibility identity.
    pub const fn identity(self) -> u64 {
        self.identity
    }

    /// Return the exact register width.
    pub const fn width(self) -> CpuConfigArmRegisterWidth {
        self.width
    }

    /// Return the modifier filter to trusted comparison code.
    pub const fn filter(self) -> u128 {
        self.filter
    }

    /// Return the modifier value to trusted comparison code.
    pub const fn value(self) -> u128 {
        self.value
    }
}

impl fmt::Debug for CpuTemplateModifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CpuTemplateModifier")
            .field("width", &self.width)
            .field("identity", &VALUE_REDACTED)
            .field("filter", &VALUE_REDACTED)
            .field("value", &VALUE_REDACTED)
            .finish()
    }
}

/// One normalized Firecracker-shaped arm64 custom CPU template.
#[derive(Clone, PartialEq, Eq)]
pub struct CpuTemplateDocument {
    modifiers: Vec<CpuTemplateModifier>,
}

impl CpuTemplateDocument {
    /// Return modifiers in ascending compatibility-identity order.
    pub fn modifiers(&self) -> &[CpuTemplateModifier] {
        &self.modifiers
    }

    /// Convert the normalized document back into runtime input.
    pub fn to_runtime_input(&self) -> CpuConfigInput {
        CpuConfigInput::new(
            Vec::new(),
            self.modifiers
                .iter()
                .copied()
                .map(|modifier| {
                    CpuConfigArmRegisterModifier::new(
                        modifier.identity,
                        modifier.width,
                        modifier.filter,
                        modifier.value,
                    )
                })
                .collect(),
            Vec::new(),
        )
    }

    /// Encode fixed-field, fixed-width, sorted pretty JSON with one newline.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, CpuTemplateEncodeError> {
        let mut bytes =
            serde_json::to_vec_pretty(self).map_err(|_| CpuTemplateEncodeError::Serialization)?;
        bytes.push(b'\n');
        if bytes.len() > CPU_TEMPLATE_DOCUMENT_MAX_BYTES {
            return Err(CpuTemplateEncodeError::TooLarge);
        }
        Ok(bytes)
    }

    pub(crate) fn from_modifiers(mut modifiers: Vec<CpuTemplateModifier>) -> Self {
        modifiers.sort_by_key(|modifier| modifier.identity);
        Self { modifiers }
    }
}

impl Serialize for CpuTemplateDocument {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        WireDocument {
            kvm_capabilities: Vec::new(),
            reg_modifiers: self
                .modifiers
                .iter()
                .copied()
                .map(|modifier| WireModifier {
                    addr: format!("0x{:016x}", modifier.identity),
                    bitmap: canonical_bitmap(modifier),
                })
                .collect(),
            vcpu_features: Vec::new(),
        }
        .serialize(serializer)
    }
}

impl fmt::Debug for CpuTemplateDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CpuTemplateDocument")
            .field("modifier_count", &self.modifiers.len())
            .field("values", &VALUE_REDACTED)
            .finish()
    }
}

/// Decode, close over the runtime target inventory, and normalize one custom
/// CPU-template document.
pub fn decode_cpu_template_document(
    contents: &str,
) -> Result<CpuTemplateDocument, CpuTemplateDocumentError> {
    if contents.len() > CPU_TEMPLATE_DOCUMENT_MAX_BYTES {
        return Err(CpuTemplateDocumentError::TooLarge);
    }
    let request =
        parse_cpu_config_document(contents).map_err(|_| CpuTemplateDocumentError::Malformed)?;
    let input = cpu_config_input_from_request(&request);
    input
        .clone()
        .into_custom_template()
        .map_err(classify_runtime_error)?;

    let mut modifiers = input
        .reg_modifiers()
        .iter()
        .copied()
        .map(|modifier| {
            CpuTemplateModifier::new(
                modifier.id(),
                modifier.width(),
                modifier.filter(),
                modifier.value(),
            )
        })
        .collect::<Vec<_>>();
    modifiers.sort_by_key(|modifier| modifier.identity);
    Ok(CpuTemplateDocument { modifiers })
}

pub(crate) fn document_from_custom_template(
    template: &CustomCpuTemplate,
) -> Option<CpuTemplateDocument> {
    let modifiers = template
        .modifiers()
        .iter()
        .copied()
        .map(modifier_from_runtime)
        .collect::<Option<Vec<_>>>()?;
    Some(CpuTemplateDocument::from_modifiers(modifiers))
}

fn modifier_from_runtime(modifier: ArmRegisterModifier) -> Option<CpuTemplateModifier> {
    let descriptor = arm64_cpu_template_register_descriptors()
        .find(|descriptor| descriptor.register() == modifier.register())?;
    Some(CpuTemplateModifier::new(
        descriptor.identity(),
        descriptor.width(),
        modifier.filter(),
        modifier.value(),
    ))
}

fn classify_runtime_error(error: CpuConfigError) -> CpuTemplateDocumentError {
    match error {
        CpuConfigError::TooManyEntries { .. }
        | CpuConfigError::DuplicateIdentity { .. }
        | CpuConfigError::FeatureIndexOutOfRange
        | CpuConfigError::InvalidRegisterArchitecture
        | CpuConfigError::InvalidRegisterWidth
        | CpuConfigError::ValueOutsideFilter { .. }
        | CpuConfigError::ValueOutsideRegisterWidth
        | CpuConfigError::InvalidRegisterIdentity
        | CpuConfigError::RegisterAliasUnsupported
        | CpuConfigError::ActlrFilterUnsupported => CpuTemplateDocumentError::NonCanonical,
        CpuConfigError::KvmCapabilitiesUnsupported
        | CpuConfigError::VcpuFeaturesUnsupported
        | CpuConfigError::MixedUnsupported
        | CpuConfigError::BootReservedRegister
        | CpuConfigError::Aarch32BankedRegisterUnavailable
        | CpuConfigError::KvmDemuxRegisterUnsupported
        | CpuConfigError::KvmFirmwareRegisterUnsupported
        | CpuConfigError::KvmFirmwareFeatureRegisterUnsupported
        | CpuConfigError::KvmSveRegisterUnsupported
        | CpuConfigError::UnknownKvmRegisterClass
        | CpuConfigError::TopologyRegisterUnsupported
        | CpuConfigError::LifecycleRegisterUnsupported
        | CpuConfigError::SecuritySensitiveRegisterUnsupported
        | CpuConfigError::TimeDependentRegisterUnsupported
        | CpuConfigError::SeparatelyOwnedRegisterUnsupported
        | CpuConfigError::MutableSmeRegisterUnsupported
        | CpuConfigError::DisabledEl2RegisterUnsupported
        | CpuConfigError::UnnamedSystemRegisterUnsupported => CpuTemplateDocumentError::Unsupported,
    }
}

fn canonical_bitmap(modifier: CpuTemplateModifier) -> String {
    let bit_count = modifier.width.bits();
    let mut bitmap = String::with_capacity(bit_count as usize + 2);
    bitmap.push_str("0b");
    for bit in (0..bit_count).rev() {
        let mask = 1_u128 << bit;
        bitmap.push(if modifier.filter & mask == 0 {
            'x'
        } else if modifier.value & mask == 0 {
            '0'
        } else {
            '1'
        });
    }
    bitmap
}

#[derive(Serialize)]
struct WireDocument {
    kvm_capabilities: Vec<()>,
    reg_modifiers: Vec<WireModifier>,
    vcpu_features: Vec<()>,
}

#[derive(Serialize)]
struct WireModifier {
    addr: String,
    bitmap: String,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing)]

    use bangbang_runtime::cpu::{
        KVM_REG_ARM64_CORE_FPCR, KVM_REG_ARM64_CORE_SP_EL0, kvm_reg_arm64_core_q,
    };

    use super::*;

    #[test]
    fn normalizes_three_widths_and_round_trips_canonical_bytes() {
        let q0 = kvm_reg_arm64_core_q(0).expect("Q0 should have an identity");
        let contents = format!(
            "{{\"reg_modifiers\":[\
             {{\"bitmap\":\"0b1x\",\"addr\":\"0x{q0:x}\"}},\
             {{\"addr\":\"0x{KVM_REG_ARM64_CORE_FPCR:x}\",\"bitmap\":\"0b1\"}},\
             {{\"addr\":\"0x{KVM_REG_ARM64_CORE_SP_EL0:x}\",\"bitmap\":\"0b0x1\"}}]}}"
        );
        let document = decode_cpu_template_document(&contents).expect("template should normalize");
        let bytes = document.canonical_bytes().expect("template should encode");
        let text = std::str::from_utf8(&bytes).expect("canonical JSON should be UTF-8");

        assert!(text.ends_with("\n"));
        assert!(text.contains("\"kvm_capabilities\": []"));
        assert!(text.contains("\"vcpu_features\": []"));
        assert!(text.contains(&format!("0x{KVM_REG_ARM64_CORE_FPCR:016x}")));
        assert!(text.contains(&format!("0x{KVM_REG_ARM64_CORE_SP_EL0:016x}")));
        assert!(text.contains(&format!("0x{q0:016x}")));
        assert_eq!(text.matches("0b").count(), 3);

        let reparsed = decode_cpu_template_document(text).expect("canonical JSON should reparse");
        assert_eq!(reparsed, document);
    }

    #[test]
    fn canonical_output_has_one_exact_wire_form() {
        let contents = format!(
            "{{\"reg_modifiers\":[{{\"bitmap\":\"0b1\",\"addr\":\"0x{KVM_REG_ARM64_CORE_FPCR:x}\"}}]}}"
        );
        let document = decode_cpu_template_document(&contents).expect("template should normalize");
        let bitmap = format!("0b{}1", "x".repeat(31));
        let expected = format!(
            "{{\n  \"kvm_capabilities\": [],\n  \"reg_modifiers\": [\n    {{\n      \"addr\": \"0x{KVM_REG_ARM64_CORE_FPCR:016x}\",\n      \"bitmap\": \"{bitmap}\"\n    }}\n  ],\n  \"vcpu_features\": []\n}}\n"
        );
        assert_eq!(
            document.canonical_bytes().as_deref(),
            Ok(expected.as_bytes())
        );
    }

    #[test]
    fn rejects_oversized_malformed_and_unsupported_documents_without_values() {
        let oversized = " ".repeat(CPU_TEMPLATE_DOCUMENT_MAX_BYTES + 1);
        assert_eq!(
            decode_cpu_template_document(&oversized),
            Err(CpuTemplateDocumentError::TooLarge)
        );
        assert_eq!(
            decode_cpu_template_document("{"),
            Err(CpuTemplateDocumentError::Malformed)
        );
        assert_eq!(
            decode_cpu_template_document(r#"{"kvm_capabilities":["1"]}"#),
            Err(CpuTemplateDocumentError::Unsupported)
        );
        assert_eq!(
            format!("{:?}", decode_cpu_template_document("{").unwrap_err()),
            "Malformed"
        );

        let duplicate_target = format!(
            "{{\"reg_modifiers\":[{{\"addr\":\"0x{KVM_REG_ARM64_CORE_FPCR:x}\",\"bitmap\":\"0b1\"}},{{\"addr\":\"0x{KVM_REG_ARM64_CORE_FPCR:x}\",\"bitmap\":\"0b0\"}}]}}"
        );
        assert_eq!(
            decode_cpu_template_document(&duplicate_target),
            Err(CpuTemplateDocumentError::Malformed)
        );
        assert_eq!(
            decode_cpu_template_document(r#"{"unknown":[]}"#),
            Err(CpuTemplateDocumentError::Malformed)
        );
        assert_eq!(
            decode_cpu_template_document(
                r#"{"reg_modifiers":[{"addr":"0x60300000001000d5","bitmap":"0b1"}]}"#,
            ),
            Err(CpuTemplateDocumentError::NonCanonical)
        );
        assert_eq!(
            decode_cpu_template_document(
                r#"{"reg_modifiers":[{"addr":"0x603000000013c200","bitmap":"0b1"}]}"#,
            ),
            Err(CpuTemplateDocumentError::NonCanonical)
        );
    }
}
