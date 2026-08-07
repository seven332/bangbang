//! Deterministic field-selected CPU-fingerprint comparison.

use std::collections::BTreeSet;
use std::fmt;

use clap::ValueEnum;
use serde::Serialize;

use crate::CPU_TEMPLATE_DOCUMENT_MAX_BYTES;
use crate::document::CpuTemplateDocument;
use crate::fingerprint::{CpuFingerprintDocument, CpuFingerprintPlatform};
use crate::strip::strip_cpu_template_documents;

const VALUE_REDACTED: &str = "<redacted>";

/// One closed public CPU-fingerprint comparison field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum CpuFingerprintField {
    /// Producing Bangbang helper version.
    ProducerVersion,
    /// Host kernel release.
    KernelRelease,
    /// macOS product identity.
    MacosProduct,
    /// macOS target identity.
    MacosTarget,
    /// macOS CPU-family identity.
    MacosCpuFamily,
    /// Linux CPU microcode identity.
    LinuxMicrocodeVersion,
    /// Linux BIOS version.
    LinuxBiosVersion,
    /// Linux BIOS revision.
    LinuxBiosRevision,
    /// Effective guest CPU configuration.
    GuestCpuConfig,
}

impl CpuFingerprintField {
    const ALL: [Self; 9] = [
        Self::ProducerVersion,
        Self::KernelRelease,
        Self::MacosProduct,
        Self::MacosTarget,
        Self::MacosCpuFamily,
        Self::LinuxMicrocodeVersion,
        Self::LinuxBiosVersion,
        Self::LinuxBiosRevision,
        Self::GuestCpuConfig,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::ProducerVersion => "producer_version",
            Self::KernelRelease => "kernel_release",
            Self::MacosProduct => "macos_product",
            Self::MacosTarget => "macos_target",
            Self::MacosCpuFamily => "macos_cpu_family",
            Self::LinuxMicrocodeVersion => "linux_microcode_version",
            Self::LinuxBiosVersion => "linux_bios_version",
            Self::LinuxBiosRevision => "linux_bios_revision",
            Self::GuestCpuConfig => "guest_cpu_config",
        }
    }

    const fn is_applicable(self, platform: CpuFingerprintPlatform) -> bool {
        match self {
            Self::ProducerVersion | Self::KernelRelease | Self::GuestCpuConfig => true,
            Self::MacosProduct | Self::MacosTarget | Self::MacosCpuFamily => {
                matches!(platform, CpuFingerprintPlatform::Macos)
            }
            Self::LinuxMicrocodeVersion | Self::LinuxBiosVersion | Self::LinuxBiosRevision => {
                matches!(platform, CpuFingerprintPlatform::Linux)
            }
        }
    }
}

/// Validated absent/default or explicit filter selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuFingerprintFilterSelection {
    explicit: Option<BTreeSet<CpuFingerprintField>>,
}

impl CpuFingerprintFilterSelection {
    /// Select every field applicable to the admitted platform.
    pub const fn all_applicable() -> Self {
        Self { explicit: None }
    }

    /// Validate and normalize an explicit list independently of caller order.
    pub fn explicit(fields: Vec<CpuFingerprintField>) -> Result<Self, CpuFingerprintFilterError> {
        if fields.is_empty() {
            return Err(CpuFingerprintFilterError::Empty);
        }
        let field_count = fields.len();
        let fields = fields.into_iter().collect::<BTreeSet<_>>();
        if fields.len() != field_count {
            return Err(CpuFingerprintFilterError::Duplicate);
        }
        Ok(Self {
            explicit: Some(fields),
        })
    }

    fn selects(&self, field: CpuFingerprintField, platform: CpuFingerprintPlatform) -> bool {
        match &self.explicit {
            None => field.is_applicable(platform),
            Some(fields) => fields.contains(&field),
        }
    }

    fn validate_platform(
        &self,
        platform: CpuFingerprintPlatform,
    ) -> Result<(), CpuFingerprintCompareError> {
        if let Some(fields) = &self.explicit
            && fields.iter().any(|field| !field.is_applicable(platform))
        {
            return Err(CpuFingerprintCompareError::UnavailableFilter);
        }
        Ok(())
    }
}

impl Default for CpuFingerprintFilterSelection {
    fn default() -> Self {
        Self::all_applicable()
    }
}

/// Invalid explicit CPU-fingerprint filter selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuFingerprintFilterError {
    /// No field was supplied for an explicit selection.
    Empty,
    /// One field was supplied more than once.
    Duplicate,
}

impl fmt::Display for CpuFingerprintFilterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "CPU-fingerprint filter selection is empty",
            Self::Duplicate => "CPU-fingerprint filter selection contains a duplicate",
        })
    }
}

impl std::error::Error for CpuFingerprintFilterError {}

/// Successful typed CPU-fingerprint comparison result.
#[derive(Clone, PartialEq, Eq)]
pub enum CpuFingerprintCompareOutcome {
    /// Every selected fact is equal.
    Equal,
    /// One complete canonical diagnostic containing selected differences.
    Different(Vec<u8>),
}

impl fmt::Debug for CpuFingerprintCompareOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Equal => formatter.write_str("Equal"),
            Self::Different(_) => formatter
                .debug_tuple("Different")
                .field(&VALUE_REDACTED)
                .finish(),
        }
    }
}

/// Failure while comparing two admitted CPU-fingerprint documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuFingerprintCompareError {
    /// The documents describe different host platforms.
    PlatformMismatch,
    /// One selected field does not exist on the admitted platform.
    UnavailableFilter,
    /// Guest CPU differences could not be transformed.
    GuestTransform,
    /// The canonical diagnostic could not be serialized.
    Encoding,
    /// The canonical diagnostic exceeded the helper document limit.
    TooLarge,
}

impl fmt::Display for CpuFingerprintCompareError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::PlatformMismatch => "CPU-fingerprint platforms do not match",
            Self::UnavailableFilter => {
                "CPU-fingerprint filter is unavailable for the host platform"
            }
            Self::GuestTransform => "CPU-fingerprint guest difference could not be transformed",
            Self::Encoding => "CPU-fingerprint differences could not be encoded",
            Self::TooLarge => "CPU-fingerprint differences exceed the size limit",
        })
    }
}

impl std::error::Error for CpuFingerprintCompareError {}

#[derive(Serialize)]
struct DifferenceDocument {
    differences: Vec<Difference>,
}

#[derive(Serialize)]
struct Difference {
    name: &'static str,
    prev: DifferenceValue,
    curr: DifferenceValue,
}

#[derive(Serialize)]
#[serde(untagged)]
enum DifferenceValue {
    Scalar(Option<String>),
    Guest(CpuTemplateDocument),
}

/// Compare selected facts and return either equality or one canonical diagnostic.
pub fn compare_cpu_fingerprints(
    prev: &CpuFingerprintDocument,
    curr: &CpuFingerprintDocument,
    filters: &CpuFingerprintFilterSelection,
) -> Result<CpuFingerprintCompareOutcome, CpuFingerprintCompareError> {
    let platform = prev.host().platform();
    if curr.host().platform() != platform {
        return Err(CpuFingerprintCompareError::PlatformMismatch);
    }
    filters.validate_platform(platform)?;

    let mut differences = Vec::new();
    for field in CpuFingerprintField::ALL {
        if !filters.selects(field, platform) {
            continue;
        }
        if let Some(difference) = compare_field(field, prev, curr)? {
            differences.push(difference);
        }
    }

    if differences.is_empty() {
        return Ok(CpuFingerprintCompareOutcome::Equal);
    }
    let mut bytes = serde_json::to_vec_pretty(&DifferenceDocument { differences })
        .map_err(|_| CpuFingerprintCompareError::Encoding)?;
    bytes.push(b'\n');
    if bytes.len() > CPU_TEMPLATE_DOCUMENT_MAX_BYTES {
        return Err(CpuFingerprintCompareError::TooLarge);
    }
    Ok(CpuFingerprintCompareOutcome::Different(bytes))
}

fn compare_field(
    field: CpuFingerprintField,
    prev: &CpuFingerprintDocument,
    curr: &CpuFingerprintDocument,
) -> Result<Option<Difference>, CpuFingerprintCompareError> {
    match field {
        CpuFingerprintField::ProducerVersion => Ok(scalar_difference(
            field,
            Some(prev.producer_version()),
            Some(curr.producer_version()),
        )),
        CpuFingerprintField::KernelRelease => Ok(scalar_difference(
            field,
            Some(prev.host().release()),
            Some(curr.host().release()),
        )),
        CpuFingerprintField::MacosProduct => Ok(scalar_difference(
            field,
            prev.host().macos_product(),
            curr.host().macos_product(),
        )),
        CpuFingerprintField::MacosTarget => Ok(scalar_difference(
            field,
            prev.host().macos_target(),
            curr.host().macos_target(),
        )),
        CpuFingerprintField::MacosCpuFamily => Ok(optional_owned_scalar_difference(
            field,
            format_cpu_family(prev.host().macos_cpu_family()),
            format_cpu_family(curr.host().macos_cpu_family()),
        )),
        CpuFingerprintField::LinuxMicrocodeVersion => required_platform_scalar_difference(
            field,
            prev.host().linux_microcode_version(),
            curr.host().linux_microcode_version(),
        ),
        CpuFingerprintField::LinuxBiosVersion => required_platform_scalar_difference(
            field,
            prev.host().linux_bios_version(),
            curr.host().linux_bios_version(),
        ),
        CpuFingerprintField::LinuxBiosRevision => required_platform_scalar_difference(
            field,
            prev.host().linux_bios_revision(),
            curr.host().linux_bios_revision(),
        ),
        CpuFingerprintField::GuestCpuConfig => guest_difference(field, prev, curr),
    }
}

fn scalar_difference(
    field: CpuFingerprintField,
    prev: Option<&str>,
    curr: Option<&str>,
) -> Option<Difference> {
    if prev == curr {
        None
    } else {
        Some(Difference {
            name: field.name(),
            prev: DifferenceValue::Scalar(prev.map(str::to_owned)),
            curr: DifferenceValue::Scalar(curr.map(str::to_owned)),
        })
    }
}

fn optional_owned_scalar_difference(
    field: CpuFingerprintField,
    prev: Option<String>,
    curr: Option<String>,
) -> Option<Difference> {
    if prev == curr {
        None
    } else {
        Some(Difference {
            name: field.name(),
            prev: DifferenceValue::Scalar(prev),
            curr: DifferenceValue::Scalar(curr),
        })
    }
}

fn required_platform_scalar_difference(
    field: CpuFingerprintField,
    prev: Option<&str>,
    curr: Option<&str>,
) -> Result<Option<Difference>, CpuFingerprintCompareError> {
    if prev.is_none() || curr.is_none() {
        return Err(CpuFingerprintCompareError::UnavailableFilter);
    }
    Ok(scalar_difference(field, prev, curr))
}

fn format_cpu_family(value: Option<u32>) -> Option<String> {
    value.map(|value| format!("0x{value:08x}"))
}

fn guest_difference(
    field: CpuFingerprintField,
    prev: &CpuFingerprintDocument,
    curr: &CpuFingerprintDocument,
) -> Result<Option<Difference>, CpuFingerprintCompareError> {
    if prev.guest_cpu_config() == curr.guest_cpu_config() {
        return Ok(None);
    }
    let stripped = strip_cpu_template_documents(vec![
        prev.guest_cpu_config().clone(),
        curr.guest_cpu_config().clone(),
    ])
    .map_err(|_| CpuFingerprintCompareError::GuestTransform)?;
    let mut stripped = stripped.into_iter();
    let prev = stripped
        .next()
        .ok_or(CpuFingerprintCompareError::GuestTransform)?;
    let curr = stripped
        .next()
        .ok_or(CpuFingerprintCompareError::GuestTransform)?;
    if stripped.next().is_some() {
        return Err(CpuFingerprintCompareError::GuestTransform);
    }
    Ok(Some(Difference {
        name: field.name(),
        prev: DifferenceValue::Guest(prev),
        curr: DifferenceValue::Guest(curr),
    }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use bangbang_runtime::cpu::{
        CpuConfigArmRegisterWidth, KVM_REG_ARM64_CORE_FPCR, KVM_REG_ARM64_CORE_SP_EL0,
        KVM_REG_ARM64_CORE_SP_EL1, kvm_reg_arm64_core_q,
    };

    use super::*;
    use crate::document::CpuTemplateModifier;
    use crate::fingerprint::{HostFingerprint, decode_cpu_fingerprint_document};

    const EMPTY_GUEST: &str = r#"{"kvm_capabilities":[],"reg_modifiers":[],"vcpu_features":[]}"#;

    fn macos_document(
        version: &str,
        release: &str,
        product: Option<&str>,
        target: Option<&str>,
        cpu_family: Option<&str>,
        guest: &str,
    ) -> CpuFingerprintDocument {
        let product = product.map_or_else(|| "null".to_owned(), |value| format!(r#""{value}""#));
        let target = target.map_or_else(|| "null".to_owned(), |value| format!(r#""{value}""#));
        let cpu_family =
            cpu_family.map_or_else(|| "null".to_owned(), |value| format!(r#""{value}""#));
        decode_cpu_fingerprint_document(&format!(
            r#"{{
                "schema_version": 1,
                "producer": {{
                    "name": "bangbang-cpu-template-helper",
                    "version": "{version}",
                    "firecracker_compatibility": "1.16.0"
                }},
                "kernel": {{
                    "operating_system": "Darwin",
                    "release": "{release}",
                    "machine": "arm64"
                }},
                "host": {{
                    "platform": "macos",
                    "product": {product},
                    "target": {target},
                    "cpu_family": {cpu_family}
                }},
                "guest_cpu_config": {guest}
            }}"#,
        ))
        .expect("macOS fingerprint fixture should decode")
    }

    fn linux_document(
        version: &str,
        release: &str,
        microcode: &str,
        bios_version: &str,
        bios_revision: &str,
        guest: &str,
    ) -> CpuFingerprintDocument {
        decode_cpu_fingerprint_document(&format!(
            r#"{{
                "schema_version": 1,
                "producer": {{
                    "name": "bangbang-cpu-template-helper",
                    "version": "{version}",
                    "firecracker_compatibility": "1.16.0"
                }},
                "kernel": {{
                    "operating_system": "Linux",
                    "release": "{release}",
                    "machine": "aarch64"
                }},
                "host": {{
                    "platform": "linux",
                    "microcode_version": "{microcode}",
                    "bios_version": "{bios_version}",
                    "bios_revision": "{bios_revision}"
                }},
                "guest_cpu_config": {guest}
            }}"#,
        ))
        .expect("Linux fingerprint fixture should decode")
    }

    fn difference_bytes(outcome: CpuFingerprintCompareOutcome) -> Vec<u8> {
        match outcome {
            CpuFingerprintCompareOutcome::Different(bytes) => bytes,
            CpuFingerprintCompareOutcome::Equal => {
                panic!("fixture should produce differences")
            }
        }
    }

    #[test]
    fn equal_defaults_and_explicit_subsets_are_silent() {
        let document = macos_document(
            "0.1.0",
            "25.5.0",
            Some("Mac16,1"),
            None,
            Some("0x1b588bb3"),
            EMPTY_GUEST,
        );
        assert_eq!(
            compare_cpu_fingerprints(
                &document,
                &document,
                &CpuFingerprintFilterSelection::all_applicable(),
            ),
            Ok(CpuFingerprintCompareOutcome::Equal)
        );
        let filters = CpuFingerprintFilterSelection::explicit(vec![
            CpuFingerprintField::GuestCpuConfig,
            CpuFingerprintField::ProducerVersion,
        ])
        .expect("explicit filters should validate");
        assert_eq!(
            compare_cpu_fingerprints(&document, &document, &filters),
            Ok(CpuFingerprintCompareOutcome::Equal)
        );
    }

    #[test]
    fn macos_defaults_emit_all_differences_in_public_order_and_repeat() {
        let prev = macos_document(
            "0.1.0",
            "25.4.0",
            None,
            Some("OldTarget"),
            Some("0x00000001"),
            EMPTY_GUEST,
        );
        let curr = macos_document(
            "0.2.0",
            "25.5.0",
            Some("Mac16,1"),
            Some("NewTarget"),
            Some("0x00000002"),
            EMPTY_GUEST,
        );
        let expected = concat!(
            "{\n",
            "  \"differences\": [\n",
            "    {\n",
            "      \"name\": \"producer_version\",\n",
            "      \"prev\": \"0.1.0\",\n",
            "      \"curr\": \"0.2.0\"\n",
            "    },\n",
            "    {\n",
            "      \"name\": \"kernel_release\",\n",
            "      \"prev\": \"25.4.0\",\n",
            "      \"curr\": \"25.5.0\"\n",
            "    },\n",
            "    {\n",
            "      \"name\": \"macos_product\",\n",
            "      \"prev\": null,\n",
            "      \"curr\": \"Mac16,1\"\n",
            "    },\n",
            "    {\n",
            "      \"name\": \"macos_target\",\n",
            "      \"prev\": \"OldTarget\",\n",
            "      \"curr\": \"NewTarget\"\n",
            "    },\n",
            "    {\n",
            "      \"name\": \"macos_cpu_family\",\n",
            "      \"prev\": \"0x00000001\",\n",
            "      \"curr\": \"0x00000002\"\n",
            "    }\n",
            "  ]\n",
            "}\n",
        );
        for _ in 0..2 {
            let outcome = compare_cpu_fingerprints(
                &prev,
                &curr,
                &CpuFingerprintFilterSelection::all_applicable(),
            )
            .expect("comparison should succeed");
            assert_eq!(difference_bytes(outcome), expected.as_bytes());
        }
    }

    #[test]
    fn linux_defaults_emit_every_applicable_scalar_in_public_order() {
        let prev = linux_document(
            "0.1.0",
            "6.1",
            "microcode-a",
            "bios-a",
            "revision-a",
            EMPTY_GUEST,
        );
        let curr = linux_document(
            "0.2.0",
            "6.2",
            "microcode-b",
            "bios-b",
            "revision-b",
            EMPTY_GUEST,
        );
        let expected = concat!(
            "{\n",
            "  \"differences\": [\n",
            "    {\n",
            "      \"name\": \"producer_version\",\n",
            "      \"prev\": \"0.1.0\",\n",
            "      \"curr\": \"0.2.0\"\n",
            "    },\n",
            "    {\n",
            "      \"name\": \"kernel_release\",\n",
            "      \"prev\": \"6.1\",\n",
            "      \"curr\": \"6.2\"\n",
            "    },\n",
            "    {\n",
            "      \"name\": \"linux_microcode_version\",\n",
            "      \"prev\": \"microcode-a\",\n",
            "      \"curr\": \"microcode-b\"\n",
            "    },\n",
            "    {\n",
            "      \"name\": \"linux_bios_version\",\n",
            "      \"prev\": \"bios-a\",\n",
            "      \"curr\": \"bios-b\"\n",
            "    },\n",
            "    {\n",
            "      \"name\": \"linux_bios_revision\",\n",
            "      \"prev\": \"revision-a\",\n",
            "      \"curr\": \"revision-b\"\n",
            "    }\n",
            "  ]\n",
            "}\n",
        );
        let outcome = compare_cpu_fingerprints(
            &prev,
            &curr,
            &CpuFingerprintFilterSelection::all_applicable(),
        )
        .expect("comparison should succeed");
        assert_eq!(difference_bytes(outcome), expected.as_bytes());
    }

    #[test]
    fn linux_explicit_selection_ignores_caller_order_and_omits_unselected_values() {
        let prev = linux_document("0.1.0", "6.1", "r1", "bios-private-a", "rev-a", EMPTY_GUEST);
        let curr = linux_document("0.2.0", "6.2", "r2", "bios-private-b", "rev-b", EMPTY_GUEST);
        let filters = CpuFingerprintFilterSelection::explicit(vec![
            CpuFingerprintField::LinuxBiosRevision,
            CpuFingerprintField::ProducerVersion,
            CpuFingerprintField::LinuxMicrocodeVersion,
        ])
        .expect("filters should validate");
        let output = String::from_utf8(difference_bytes(
            compare_cpu_fingerprints(&prev, &curr, &filters).expect("comparison should succeed"),
        ))
        .expect("diagnostic should be UTF-8");
        let producer = output.find("producer_version").expect("producer diff");
        let microcode = output
            .find("linux_microcode_version")
            .expect("microcode diff");
        let revision = output
            .find("linux_bios_revision")
            .expect("BIOS revision diff");
        assert!(producer < microcode && microcode < revision);
        assert!(!output.contains("kernel_release"));
        assert!(!output.contains("bios-private"));
    }

    #[test]
    fn guest_difference_reuses_native_width_strip_and_preserves_missing_identity() {
        let host = HostFingerprint::try_macos(
            "Darwin".to_owned(),
            "25.5.0".to_owned(),
            "arm64".to_owned(),
            None,
            None,
            None,
        )
        .expect("host should validate");
        let q0 = kvm_reg_arm64_core_q(0).expect("Q0 should have an identity");
        let prev_guest = CpuTemplateDocument::from_modifiers(vec![
            CpuTemplateModifier::new(
                KVM_REG_ARM64_CORE_FPCR,
                CpuConfigArmRegisterWidth::U32,
                0b1111,
                0,
            ),
            CpuTemplateModifier::new(
                KVM_REG_ARM64_CORE_SP_EL0,
                CpuConfigArmRegisterWidth::U64,
                0b1111,
                0,
            ),
            CpuTemplateModifier::new(q0, CpuConfigArmRegisterWidth::U128, u128::MAX, 1 << 100),
            CpuTemplateModifier::new(
                KVM_REG_ARM64_CORE_SP_EL1,
                CpuConfigArmRegisterWidth::U64,
                1,
                1,
            ),
        ]);
        let curr_guest = CpuTemplateDocument::from_modifiers(vec![
            CpuTemplateModifier::new(
                KVM_REG_ARM64_CORE_FPCR,
                CpuConfigArmRegisterWidth::U32,
                0b1111,
                0b0011,
            ),
            CpuTemplateModifier::new(
                KVM_REG_ARM64_CORE_SP_EL0,
                CpuConfigArmRegisterWidth::U64,
                0b1111,
                0b0101,
            ),
            CpuTemplateModifier::new(q0, CpuConfigArmRegisterWidth::U128, u128::MAX, 1 << 101),
        ]);
        let prev = CpuFingerprintDocument::new_current(host.clone(), prev_guest)
            .expect("previous fingerprint should construct");
        let curr = CpuFingerprintDocument::new_current(host, curr_guest)
            .expect("current fingerprint should construct");
        let filters =
            CpuFingerprintFilterSelection::explicit(vec![CpuFingerprintField::GuestCpuConfig])
                .expect("filter should validate");
        let bytes = difference_bytes(
            compare_cpu_fingerprints(&prev, &curr, &filters).expect("comparison should succeed"),
        );
        let output = String::from_utf8(bytes.clone()).expect("diagnostic should be UTF-8");
        assert!(output.contains(r#""name": "guest_cpu_config""#));
        assert!(output.contains("0bxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx00"));
        assert!(output.contains("0bxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx11"));
        assert!(output.contains(&format!("0x{KVM_REG_ARM64_CORE_SP_EL0:016x}")));
        assert!(output.contains(&format!("0x{q0:016x}")));
        let missing_identity = format!("0x{KVM_REG_ARM64_CORE_SP_EL1:016x}");
        assert!(output.contains(&missing_identity));
        assert_eq!(output.matches(&missing_identity).count(), 1);
        assert!(bytes.len() <= CPU_TEMPLATE_DOCUMENT_MAX_BYTES);
        assert_eq!(
            difference_bytes(
                compare_cpu_fingerprints(&prev, &curr, &filters)
                    .expect("repeat comparison should succeed"),
            ),
            bytes
        );
    }

    #[test]
    fn rejects_empty_duplicate_cross_platform_and_unavailable_filters_without_values() {
        assert_eq!(
            CpuFingerprintFilterSelection::explicit(Vec::new()),
            Err(CpuFingerprintFilterError::Empty)
        );
        assert_eq!(
            CpuFingerprintFilterSelection::explicit(vec![
                CpuFingerprintField::KernelRelease,
                CpuFingerprintField::KernelRelease,
            ]),
            Err(CpuFingerprintFilterError::Duplicate)
        );

        let macos = macos_document("0.1.0", "25.5.0", None, None, None, EMPTY_GUEST);
        let linux = linux_document("0.1.0", "6.1", "r1", "v1", "r1", EMPTY_GUEST);
        assert_eq!(
            compare_cpu_fingerprints(
                &macos,
                &linux,
                &CpuFingerprintFilterSelection::all_applicable(),
            ),
            Err(CpuFingerprintCompareError::PlatformMismatch)
        );
        let unavailable =
            CpuFingerprintFilterSelection::explicit(vec![CpuFingerprintField::LinuxBiosVersion])
                .expect("filter syntax should validate");
        assert_eq!(
            compare_cpu_fingerprints(&macos, &macos, &unavailable),
            Err(CpuFingerprintCompareError::UnavailableFilter)
        );
        let unavailable =
            CpuFingerprintFilterSelection::explicit(vec![CpuFingerprintField::MacosProduct])
                .expect("filter syntax should validate");
        assert_eq!(
            compare_cpu_fingerprints(&linux, &linux, &unavailable),
            Err(CpuFingerprintCompareError::UnavailableFilter)
        );

        for error in [
            CpuFingerprintCompareError::PlatformMismatch,
            CpuFingerprintCompareError::UnavailableFilter,
            CpuFingerprintCompareError::GuestTransform,
            CpuFingerprintCompareError::Encoding,
            CpuFingerprintCompareError::TooLarge,
        ] {
            assert!(!error.to_string().contains("private"));
        }
        assert_eq!(
            format!(
                "{:?}",
                CpuFingerprintCompareOutcome::Different(b"private-value".to_vec())
            ),
            "Different(\"<redacted>\")"
        );
    }
}
