//! Effective-profile contract and portable dump/verify orchestration.

use std::fmt;

use bangbang_runtime::cpu::{
    ARM64_CPU_TEMPLATE_REGISTER_COUNT, ArmRegisterAvailability, ArmRegisterBootDisposition,
    CpuConfigArmRegisterWidth, arm64_cpu_template_register_descriptor,
    arm64_cpu_template_register_descriptors,
};

use crate::HelperExitClass;
use crate::document::{
    CpuTemplateDocument, CpuTemplateEncodeError, CpuTemplateModifier, document_from_custom_template,
};
use crate::projection::PreparedCpuTemplateInspection;

const VALUE_REDACTED: &str = "<redacted>";

/// One exact-width effective register value.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ArmCpuTemplateValue {
    U32(u32),
    U64(u64),
    U128(u128),
}

impl ArmCpuTemplateValue {
    /// Return the exact architectural width.
    pub const fn width(self) -> CpuConfigArmRegisterWidth {
        match self {
            Self::U32(_) => CpuConfigArmRegisterWidth::U32,
            Self::U64(_) => CpuConfigArmRegisterWidth::U64,
            Self::U128(_) => CpuConfigArmRegisterWidth::U128,
        }
    }

    /// Return the value in a zero-extended transport slot.
    pub const fn zero_extended(self) -> u128 {
        match self {
            Self::U32(value) => value as u128,
            Self::U64(value) => value as u128,
            Self::U128(value) => value,
        }
    }
}

impl fmt::Debug for ArmCpuTemplateValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(VALUE_REDACTED)
    }
}

/// Availability result for one descriptor slot.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EffectiveRegisterStatus {
    Available(ArmCpuTemplateValue),
    Unavailable,
}

impl fmt::Debug for EffectiveRegisterStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Available(_) => formatter
                .debug_tuple("Available")
                .field(&VALUE_REDACTED)
                .finish(),
            Self::Unavailable => formatter.write_str("Unavailable"),
        }
    }
}

/// One identity-bound entry returned by an effective provider.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct EffectiveCpuTemplateProfileEntry {
    identity: u64,
    status: EffectiveRegisterStatus,
}

impl EffectiveCpuTemplateProfileEntry {
    /// Construct one available identity-bound entry.
    pub const fn available(identity: u64, value: ArmCpuTemplateValue) -> Self {
        Self {
            identity,
            status: EffectiveRegisterStatus::Available(value),
        }
    }

    /// Construct one explicitly unavailable identity-bound entry.
    pub const fn unavailable(identity: u64) -> Self {
        Self {
            identity,
            status: EffectiveRegisterStatus::Unavailable,
        }
    }

    /// Return the compatibility identity.
    pub const fn identity(self) -> u64 {
        self.identity
    }

    /// Return the effective availability/value status.
    pub const fn status(self) -> EffectiveRegisterStatus {
        self.status
    }
}

impl fmt::Debug for EffectiveCpuTemplateProfileEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EffectiveCpuTemplateProfileEntry")
            .field("identity", &VALUE_REDACTED)
            .field("status", &self.status)
            .finish()
    }
}

/// Invalid structure returned by an effective profile provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveCpuTemplateProfileError {
    EntryCount,
    IdentityOrOrder,
    Width,
    RequiredUnavailable,
}

impl fmt::Display for EffectiveCpuTemplateProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EntryCount => "effective CPU profile has an invalid entry count",
            Self::IdentityOrOrder => "effective CPU profile has an invalid identity order",
            Self::Width => "effective CPU profile has an invalid value width",
            Self::RequiredUnavailable => "effective CPU profile omits a baseline-required register",
        })
    }
}

impl std::error::Error for EffectiveCpuTemplateProfileError {}

/// A topology-common application-checkpoint result covering the exact runtime
/// descriptor census.
#[derive(Clone, PartialEq, Eq)]
pub struct EffectiveCpuTemplateProfile {
    entries: Vec<EffectiveCpuTemplateProfileEntry>,
}

impl EffectiveCpuTemplateProfile {
    /// Validate identity, order, width, and availability against the runtime
    /// descriptor authority.
    pub fn try_new(
        entries: Vec<EffectiveCpuTemplateProfileEntry>,
    ) -> Result<Self, EffectiveCpuTemplateProfileError> {
        if entries.len() != ARM64_CPU_TEMPLATE_REGISTER_COUNT {
            return Err(EffectiveCpuTemplateProfileError::EntryCount);
        }
        for (entry, descriptor) in entries
            .iter()
            .copied()
            .zip(arm64_cpu_template_register_descriptors())
        {
            if entry.identity != descriptor.identity() {
                return Err(EffectiveCpuTemplateProfileError::IdentityOrOrder);
            }
            match entry.status {
                EffectiveRegisterStatus::Available(value)
                    if value.width() != descriptor.width() =>
                {
                    return Err(EffectiveCpuTemplateProfileError::Width);
                }
                EffectiveRegisterStatus::Unavailable
                    if descriptor.availability() == ArmRegisterAvailability::Baseline =>
                {
                    return Err(EffectiveCpuTemplateProfileError::RequiredUnavailable);
                }
                EffectiveRegisterStatus::Available(_) | EffectiveRegisterStatus::Unavailable => {}
            }
        }
        Ok(Self { entries })
    }

    /// Return entries in exact descriptor order.
    pub fn entries(&self) -> &[EffectiveCpuTemplateProfileEntry] {
        &self.entries
    }

    fn entry(&self, identity: u64) -> Option<EffectiveCpuTemplateProfileEntry> {
        self.entries
            .iter()
            .copied()
            .find(|entry| entry.identity == identity)
    }
}

impl fmt::Debug for EffectiveCpuTemplateProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EffectiveCpuTemplateProfile")
            .field("entry_count", &self.entries.len())
            .field("values", &VALUE_REDACTED)
            .finish()
    }
}

/// Stable value-free failure stage returned by a production effective
/// provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveProfileProviderError {
    Unsupported,
    Prepare,
    Apply,
    Capture,
    Teardown,
}

impl fmt::Display for EffectiveProfileProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unsupported => "effective CPU inspection is unsupported",
            Self::Prepare => "effective CPU inspection preparation failed",
            Self::Apply => "effective CPU-template application failed",
            Self::Capture => "effective CPU profile capture failed",
            Self::Teardown => "effective CPU inspection teardown failed",
        })
    }
}

impl std::error::Error for EffectiveProfileProviderError {}

/// Platform adapter for one real topology-common application-checkpoint
/// inspection.
pub trait EffectiveCpuTemplateProvider {
    fn inspect(
        &mut self,
        request: &PreparedCpuTemplateInspection,
    ) -> Result<EffectiveCpuTemplateProfile, EffectiveProfileProviderError>;
}

/// Portable dump or verification failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuTemplateOperationError {
    Provider(EffectiveProfileProviderError),
    NoTemplate,
    RequestedRegisterUnavailable,
    VerificationMismatch,
    Encoding(CpuTemplateEncodeError),
}

impl CpuTemplateOperationError {
    /// All orchestration failures are operational exit class 1.
    pub const fn exit_class(self) -> HelperExitClass {
        HelperExitClass::OperationalFailure
    }
}

impl fmt::Display for CpuTemplateOperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Provider(_) => "effective CPU inspection failed",
            Self::NoTemplate => "no custom CPU template was selected",
            Self::RequestedRegisterUnavailable => {
                "a requested CPU-template register is unavailable"
            }
            Self::VerificationMismatch => "effective CPU-template verification failed",
            Self::Encoding(_) => "effective CPU profile could not be encoded",
        })
    }
}

impl std::error::Error for CpuTemplateOperationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Provider(source) => Some(source),
            Self::Encoding(source) => Some(source),
            Self::NoTemplate | Self::RequestedRegisterUnavailable | Self::VerificationMismatch => {
                None
            }
        }
    }
}

/// Capture and canonically encode every available retained profile entry.
pub fn dump_with_provider(
    provider: &mut impl EffectiveCpuTemplateProvider,
    request: &PreparedCpuTemplateInspection,
) -> Result<Vec<u8>, CpuTemplateOperationError> {
    capture_document_with_provider(provider, request)?
        .canonical_bytes()
        .map_err(CpuTemplateOperationError::Encoding)
}

/// Capture every available retained profile entry as one normalized document.
pub fn capture_document_with_provider(
    provider: &mut impl EffectiveCpuTemplateProvider,
    request: &PreparedCpuTemplateInspection,
) -> Result<CpuTemplateDocument, CpuTemplateOperationError> {
    let profile = provider
        .inspect(request)
        .map_err(CpuTemplateOperationError::Provider)?;
    let modifiers = profile
        .entries
        .iter()
        .copied()
        .zip(arm64_cpu_template_register_descriptors())
        .filter_map(|(entry, descriptor)| {
            if descriptor.boot_disposition() != ArmRegisterBootDisposition::Retained {
                return None;
            }
            let EffectiveRegisterStatus::Available(value) = entry.status else {
                return None;
            };
            let filter = descriptor.allowed_filter();
            Some(CpuTemplateModifier::new(
                descriptor.identity(),
                descriptor.width(),
                filter,
                value.zero_extended() & filter,
            ))
        })
        .collect();
    Ok(CpuTemplateDocument::from_modifiers(modifiers))
}

/// Capture once and compare the selected custom template under its filters.
pub fn verify_with_provider(
    provider: &mut impl EffectiveCpuTemplateProvider,
    request: &PreparedCpuTemplateInspection,
) -> Result<(), CpuTemplateOperationError> {
    let template = request
        .custom_template()
        .ok_or(CpuTemplateOperationError::NoTemplate)?;
    let expected = document_from_custom_template(template)
        .ok_or(CpuTemplateOperationError::VerificationMismatch)?;
    let profile = provider
        .inspect(request)
        .map_err(CpuTemplateOperationError::Provider)?;

    for modifier in expected.modifiers().iter().copied() {
        let descriptor = arm64_cpu_template_register_descriptor(modifier.identity())
            .ok_or(CpuTemplateOperationError::VerificationMismatch)?;
        debug_assert_eq!(descriptor.width(), modifier.width());
        let entry = profile
            .entry(modifier.identity())
            .ok_or(CpuTemplateOperationError::VerificationMismatch)?;
        let EffectiveRegisterStatus::Available(effective) = entry.status else {
            return Err(CpuTemplateOperationError::RequestedRegisterUnavailable);
        };
        if effective.zero_extended() & modifier.filter() != modifier.value() {
            return Err(CpuTemplateOperationError::VerificationMismatch);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing)]

    use bangbang_runtime::cpu::{
        KVM_REG_ARM64_ACTLR_EL1, KVM_REG_ARM64_CORE_PC, KVM_REG_ARM64_CORE_SP_EL0,
        KVM_REG_ARM64_ID_AA64SMFR0_EL1, KVM_REG_ARM64_ID_AA64ZFR0_EL1,
    };

    use super::*;
    use crate::document::decode_cpu_template_document;
    use crate::projection::prepare_inspection_request;

    #[derive(Debug)]
    struct FakeProvider {
        profile: EffectiveCpuTemplateProfile,
        calls: usize,
    }

    impl EffectiveCpuTemplateProvider for FakeProvider {
        fn inspect(
            &mut self,
            _request: &PreparedCpuTemplateInspection,
        ) -> Result<EffectiveCpuTemplateProfile, EffectiveProfileProviderError> {
            self.calls += 1;
            Ok(self.profile.clone())
        }
    }

    fn zero_value(width: CpuConfigArmRegisterWidth) -> ArmCpuTemplateValue {
        match width {
            CpuConfigArmRegisterWidth::U32 => ArmCpuTemplateValue::U32(0),
            CpuConfigArmRegisterWidth::U64 => ArmCpuTemplateValue::U64(0),
            CpuConfigArmRegisterWidth::U128 => ArmCpuTemplateValue::U128(0),
        }
    }

    fn profile_with(overrides: &[(u64, EffectiveRegisterStatus)]) -> EffectiveCpuTemplateProfile {
        let entries = arm64_cpu_template_register_descriptors()
            .map(|descriptor| {
                let status = overrides
                    .iter()
                    .find(|(identity, _)| *identity == descriptor.identity())
                    .map_or_else(
                        || EffectiveRegisterStatus::Available(zero_value(descriptor.width())),
                        |(_, status)| *status,
                    );
                EffectiveCpuTemplateProfileEntry {
                    identity: descriptor.identity(),
                    status,
                }
            })
            .collect();
        EffectiveCpuTemplateProfile::try_new(entries).expect("profile fixture should validate")
    }

    #[test]
    fn profile_rejects_same_width_identity_swaps_and_required_omissions() {
        let profile = profile_with(&[]);
        let mut swapped = profile.entries.clone();
        swapped.swap(3, 4);
        assert_eq!(
            EffectiveCpuTemplateProfile::try_new(swapped),
            Err(EffectiveCpuTemplateProfileError::IdentityOrOrder)
        );

        let unavailable = profile_with(&[])
            .entries
            .into_iter()
            .map(|entry| {
                if entry.identity == KVM_REG_ARM64_CORE_SP_EL0 {
                    EffectiveCpuTemplateProfileEntry::unavailable(entry.identity)
                } else {
                    entry
                }
            })
            .collect();
        assert_eq!(
            EffectiveCpuTemplateProfile::try_new(unavailable),
            Err(EffectiveCpuTemplateProfileError::RequiredUnavailable)
        );
    }

    #[test]
    fn dump_uses_one_profile_and_excludes_boot_overridden_targets() {
        let request =
            prepare_inspection_request(None, None).expect("default request should prepare");
        let mut provider = FakeProvider {
            profile: profile_with(&[]),
            calls: 0,
        };
        let bytes = dump_with_provider(&mut provider, &request).expect("dump should encode");
        let document = decode_cpu_template_document(
            std::str::from_utf8(&bytes).expect("dump should be UTF-8"),
        )
        .expect("dump should reparse");

        assert_eq!(provider.calls, 1);
        assert_eq!(document.modifiers().len(), 77);
        assert!(
            document
                .modifiers()
                .iter()
                .all(|modifier| modifier.identity() != KVM_REG_ARM64_CORE_PC)
        );
    }

    #[test]
    fn optional_unavailable_registers_are_omitted_but_explicit_requests_fail() {
        let unavailable = [
            KVM_REG_ARM64_ACTLR_EL1,
            KVM_REG_ARM64_ID_AA64ZFR0_EL1,
            KVM_REG_ARM64_ID_AA64SMFR0_EL1,
        ]
        .map(|identity| (identity, EffectiveRegisterStatus::Unavailable));
        let profile = profile_with(&unavailable);
        let default_request =
            prepare_inspection_request(None, None).expect("default request should prepare");
        let mut dump_provider = FakeProvider {
            profile: profile.clone(),
            calls: 0,
        };
        let bytes = dump_with_provider(&mut dump_provider, &default_request)
            .expect("dump should omit unavailable optional registers");
        let document = decode_cpu_template_document(
            std::str::from_utf8(&bytes).expect("dump should be UTF-8"),
        )
        .expect("dump should reparse");
        assert_eq!(document.modifiers().len(), 74);

        let contents = format!(
            "{{\"reg_modifiers\":[{{\"addr\":\"0x{KVM_REG_ARM64_ACTLR_EL1:016x}\",\"bitmap\":\"0b1x\"}}]}}"
        );
        let explicit_request = prepare_inspection_request(None, Some(&contents))
            .expect("ACTLR EnTSO request should prepare");
        let mut verify_provider = FakeProvider { profile, calls: 0 };
        assert_eq!(
            verify_with_provider(&mut verify_provider, &explicit_request),
            Err(CpuTemplateOperationError::RequestedRegisterUnavailable)
        );
    }

    #[test]
    fn verify_uses_filters_and_checks_boot_overridden_application_values() {
        let contents = format!(
            "{{\"reg_modifiers\":[{{\"addr\":\"0x{KVM_REG_ARM64_CORE_PC:016x}\",\"bitmap\":\"0b1x\"}}]}}"
        );
        let request = prepare_inspection_request(None, Some(&contents))
            .expect("explicit template should prepare");
        let mut matching = FakeProvider {
            profile: profile_with(&[(
                KVM_REG_ARM64_CORE_PC,
                EffectiveRegisterStatus::Available(ArmCpuTemplateValue::U64(0b11)),
            )]),
            calls: 0,
        };
        verify_with_provider(&mut matching, &request).expect("masked value should match");
        assert_eq!(matching.calls, 1);

        let mut mismatching = FakeProvider {
            profile: profile_with(&[]),
            calls: 0,
        };
        assert_eq!(
            verify_with_provider(&mut mismatching, &request),
            Err(CpuTemplateOperationError::VerificationMismatch)
        );
    }

    #[test]
    fn verify_without_custom_template_does_not_call_provider() {
        let request =
            prepare_inspection_request(None, None).expect("default request should prepare");
        let mut provider = FakeProvider {
            profile: profile_with(&[]),
            calls: 0,
        };
        assert_eq!(
            verify_with_provider(&mut provider, &request),
            Err(CpuTemplateOperationError::NoTemplate)
        );
        assert_eq!(provider.calls, 0);
    }
}
