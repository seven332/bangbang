//! Backend-neutral CPU-template input and executable model.

use std::collections::HashSet;
use std::fmt;

pub const CPU_CONFIG_MAX_ENTRIES_PER_ARRAY: usize = 256;
pub const CPU_CONFIG_KVM_VCPU_FEATURE_WORDS: u32 = 7;

pub const KVM_REG_ARM64_ID_AA64PFR0_EL1: u64 = 0x6030_0000_0013_c020;
pub const KVM_REG_ARM64_ID_AA64ISAR0_EL1: u64 = 0x6030_0000_0013_c030;
pub const KVM_REG_ARM64_ID_AA64ISAR1_EL1: u64 = 0x6030_0000_0013_c031;
pub const KVM_REG_ARM64_ID_AA64MMFR2_EL1: u64 = 0x6030_0000_0013_c03a;

const ARM64_KVM_REG_ARCH_MASK: u64 = 0xff00_0000_0000_0000;
const ARM64_KVM_REG_ARCH: u64 = 0x6000_0000_0000_0000;
const CPU_CONFIG_VALUE_REDACTED: &str = "<redacted>";

#[derive(Clone, PartialEq, Eq)]
pub struct CpuConfigInput {
    kvm_capabilities: Vec<CpuConfigKvmCapability>,
    reg_modifiers: Vec<CpuConfigArmRegisterModifier>,
    vcpu_features: Vec<CpuConfigVcpuFeature>,
}

impl CpuConfigInput {
    pub const fn new(
        kvm_capabilities: Vec<CpuConfigKvmCapability>,
        reg_modifiers: Vec<CpuConfigArmRegisterModifier>,
        vcpu_features: Vec<CpuConfigVcpuFeature>,
    ) -> Self {
        Self {
            kvm_capabilities,
            reg_modifiers,
            vcpu_features,
        }
    }

    pub const fn noop() -> Self {
        Self::new(Vec::new(), Vec::new(), Vec::new())
    }

    pub fn kvm_capabilities(&self) -> &[CpuConfigKvmCapability] {
        &self.kvm_capabilities
    }

    pub fn reg_modifiers(&self) -> &[CpuConfigArmRegisterModifier] {
        &self.reg_modifiers
    }

    pub fn vcpu_features(&self) -> &[CpuConfigVcpuFeature] {
        &self.vcpu_features
    }

    pub const fn category(&self) -> Option<CpuConfigTemplateCategory> {
        match (
            self.kvm_capabilities.is_empty(),
            self.reg_modifiers.is_empty(),
            self.vcpu_features.is_empty(),
        ) {
            (true, true, true) => None,
            (false, true, true) => Some(CpuConfigTemplateCategory::KvmCapabilities),
            (true, false, true) => Some(CpuConfigTemplateCategory::ArmRegisterModifiers),
            (true, true, false) => Some(CpuConfigTemplateCategory::VcpuFeatures),
            _ => Some(CpuConfigTemplateCategory::Mixed),
        }
    }

    pub fn into_custom_template(self) -> Result<Option<CustomCpuTemplate>, CpuConfigError> {
        self.validate_shape()?;

        match self.category() {
            None => Ok(None),
            Some(CpuConfigTemplateCategory::KvmCapabilities) => {
                Err(CpuConfigError::KvmCapabilitiesUnsupported)
            }
            Some(CpuConfigTemplateCategory::VcpuFeatures) => {
                Err(CpuConfigError::VcpuFeaturesUnsupported)
            }
            Some(CpuConfigTemplateCategory::Mixed) => Err(CpuConfigError::MixedUnsupported),
            Some(CpuConfigTemplateCategory::ArmRegisterModifiers) => {
                let modifiers = self
                    .reg_modifiers
                    .into_iter()
                    .map(CpuConfigArmRegisterModifier::into_executable)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Some(CustomCpuTemplate { modifiers }))
            }
        }
    }

    fn validate_shape(&self) -> Result<(), CpuConfigError> {
        validate_len(
            self.kvm_capabilities.len(),
            CpuConfigCollection::KvmCapabilities,
        )?;
        validate_len(
            self.reg_modifiers.len(),
            CpuConfigCollection::RegisterModifiers,
        )?;
        validate_len(self.vcpu_features.len(), CpuConfigCollection::VcpuFeatures)?;

        let mut capability_ids = HashSet::with_capacity(self.kvm_capabilities.len());
        for capability in &self.kvm_capabilities {
            if !capability_ids.insert(capability.value()) {
                return Err(CpuConfigError::DuplicateIdentity {
                    collection: CpuConfigCollection::KvmCapabilities,
                });
            }
        }

        let mut register_ids = HashSet::with_capacity(self.reg_modifiers.len());
        for modifier in &self.reg_modifiers {
            modifier.validate_shape()?;
            if !register_ids.insert(modifier.id()) {
                return Err(CpuConfigError::DuplicateIdentity {
                    collection: CpuConfigCollection::RegisterModifiers,
                });
            }
        }

        let mut feature_indexes = HashSet::with_capacity(self.vcpu_features.len());
        for feature in &self.vcpu_features {
            feature.validate_shape()?;
            if !feature_indexes.insert(feature.index()) {
                return Err(CpuConfigError::DuplicateIdentity {
                    collection: CpuConfigCollection::VcpuFeatures,
                });
            }
        }

        Ok(())
    }
}

impl Default for CpuConfigInput {
    fn default() -> Self {
        Self::noop()
    }
}

impl fmt::Debug for CpuConfigInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CpuConfigInput")
            .field("category", &self.category())
            .field("kvm_capability_count", &self.kvm_capabilities.len())
            .field("reg_modifier_count", &self.reg_modifiers.len())
            .field("vcpu_feature_count", &self.vcpu_features.len())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuConfigTemplateCategory {
    KvmCapabilities,
    VcpuFeatures,
    ArmRegisterModifiers,
    Mixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuConfigCollection {
    KvmCapabilities,
    VcpuFeatures,
    RegisterModifiers,
}

impl fmt::Display for CpuConfigCollection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::KvmCapabilities => f.write_str("kvm_capabilities"),
            Self::VcpuFeatures => f.write_str("vcpu_features"),
            Self::RegisterModifiers => f.write_str("reg_modifiers"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CpuConfigKvmCapability {
    Add(u32),
    Remove(u32),
}

impl CpuConfigKvmCapability {
    pub const fn value(self) -> u32 {
        match self {
            Self::Add(value) | Self::Remove(value) => value,
        }
    }
}

impl fmt::Debug for CpuConfigKvmCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let operation = match self {
            Self::Add(_) => "Add",
            Self::Remove(_) => "Remove",
        };
        f.debug_tuple(operation)
            .field(&CPU_CONFIG_VALUE_REDACTED)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuConfigArmRegisterWidth {
    U32,
    U64,
    U128,
}

impl CpuConfigArmRegisterWidth {
    pub const fn bits(self) -> u32 {
        match self {
            Self::U32 => 32,
            Self::U64 => 64,
            Self::U128 => 128,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CpuConfigArmRegisterModifier {
    id: u64,
    width: CpuConfigArmRegisterWidth,
    filter: u128,
    value: u128,
}

impl CpuConfigArmRegisterModifier {
    pub const fn new(id: u64, width: CpuConfigArmRegisterWidth, filter: u128, value: u128) -> Self {
        Self {
            id,
            width,
            filter,
            value,
        }
    }

    pub const fn id(self) -> u64 {
        self.id
    }

    pub const fn width(self) -> CpuConfigArmRegisterWidth {
        self.width
    }

    pub const fn filter(self) -> u128 {
        self.filter
    }

    pub const fn value(self) -> u128 {
        self.value
    }

    fn validate_shape(self) -> Result<(), CpuConfigError> {
        if self.id & ARM64_KVM_REG_ARCH_MASK != ARM64_KVM_REG_ARCH {
            return Err(CpuConfigError::InvalidRegisterArchitecture);
        }
        if self.value & !self.filter != 0 {
            return Err(CpuConfigError::ValueOutsideFilter {
                collection: CpuConfigCollection::RegisterModifiers,
            });
        }
        if let Some(limit) = width_limit(self.width)
            && (self.filter > limit || self.value > limit)
        {
            return Err(CpuConfigError::ValueOutsideRegisterWidth);
        }
        Ok(())
    }

    fn into_executable(self) -> Result<ArmIdRegisterModifier, CpuConfigError> {
        if self.width != CpuConfigArmRegisterWidth::U64 {
            return Err(CpuConfigError::UnsupportedRegisterWidth);
        }
        let register = match self.id {
            KVM_REG_ARM64_ID_AA64PFR0_EL1 => ArmIdRegister::Pfr0,
            KVM_REG_ARM64_ID_AA64ISAR0_EL1 => ArmIdRegister::Isar0,
            KVM_REG_ARM64_ID_AA64ISAR1_EL1 => ArmIdRegister::Isar1,
            KVM_REG_ARM64_ID_AA64MMFR2_EL1 => ArmIdRegister::Mmfr2,
            _ => return Err(CpuConfigError::UnsupportedRegister),
        };
        let filter =
            u64::try_from(self.filter).map_err(|_| CpuConfigError::ValueOutsideRegisterWidth)?;
        let value =
            u64::try_from(self.value).map_err(|_| CpuConfigError::ValueOutsideRegisterWidth)?;
        Ok(ArmIdRegisterModifier {
            register,
            filter,
            value,
        })
    }
}

impl fmt::Debug for CpuConfigArmRegisterModifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CpuConfigArmRegisterModifier")
            .field("width", &self.width)
            .field("id", &CPU_CONFIG_VALUE_REDACTED)
            .field("filter", &CPU_CONFIG_VALUE_REDACTED)
            .field("value", &CPU_CONFIG_VALUE_REDACTED)
            .finish()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CpuConfigVcpuFeature {
    index: u32,
    filter: u32,
    value: u32,
}

impl CpuConfigVcpuFeature {
    pub const fn new(index: u32, filter: u32, value: u32) -> Self {
        Self {
            index,
            filter,
            value,
        }
    }

    pub const fn index(self) -> u32 {
        self.index
    }

    pub const fn filter(self) -> u32 {
        self.filter
    }

    pub const fn value(self) -> u32 {
        self.value
    }

    fn validate_shape(self) -> Result<(), CpuConfigError> {
        if self.index >= CPU_CONFIG_KVM_VCPU_FEATURE_WORDS {
            return Err(CpuConfigError::FeatureIndexOutOfRange);
        }
        if self.value & !self.filter != 0 {
            return Err(CpuConfigError::ValueOutsideFilter {
                collection: CpuConfigCollection::VcpuFeatures,
            });
        }
        Ok(())
    }
}

impl fmt::Debug for CpuConfigVcpuFeature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CpuConfigVcpuFeature")
            .field("index", &CPU_CONFIG_VALUE_REDACTED)
            .field("filter", &CPU_CONFIG_VALUE_REDACTED)
            .field("value", &CPU_CONFIG_VALUE_REDACTED)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct CustomCpuTemplate {
    modifiers: Vec<ArmIdRegisterModifier>,
}

impl CustomCpuTemplate {
    pub fn modifiers(&self) -> &[ArmIdRegisterModifier] {
        &self.modifiers
    }

    pub fn is_empty(&self) -> bool {
        self.modifiers.is_empty()
    }
}

impl fmt::Debug for CustomCpuTemplate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CustomCpuTemplate")
            .field("modifier_count", &self.modifiers.len())
            .finish()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ArmIdRegister {
    Pfr0,
    Isar0,
    Isar1,
    Mmfr2,
}

impl fmt::Debug for ArmIdRegister {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(CPU_CONFIG_VALUE_REDACTED)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ArmIdRegisterModifier {
    register: ArmIdRegister,
    filter: u64,
    value: u64,
}

impl ArmIdRegisterModifier {
    pub const fn register(self) -> ArmIdRegister {
        self.register
    }

    pub const fn filter(self) -> u64 {
        self.filter
    }

    pub const fn value(self) -> u64 {
        self.value
    }

    pub const fn apply(self, baseline: u64) -> u64 {
        (baseline & !self.filter) | self.value
    }
}

impl fmt::Debug for ArmIdRegisterModifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ArmIdRegisterModifier")
            .field("register", &CPU_CONFIG_VALUE_REDACTED)
            .field("filter", &CPU_CONFIG_VALUE_REDACTED)
            .field("value", &CPU_CONFIG_VALUE_REDACTED)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuConfigError {
    TooManyEntries { collection: CpuConfigCollection },
    DuplicateIdentity { collection: CpuConfigCollection },
    FeatureIndexOutOfRange,
    InvalidRegisterArchitecture,
    ValueOutsideFilter { collection: CpuConfigCollection },
    ValueOutsideRegisterWidth,
    KvmCapabilitiesUnsupported,
    VcpuFeaturesUnsupported,
    MixedUnsupported,
    UnsupportedRegisterWidth,
    UnsupportedRegister,
}

impl fmt::Display for CpuConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyEntries { collection } => write!(
                f,
                "cpu-config {collection} exceeds the supported entry limit"
            ),
            Self::DuplicateIdentity { collection } => {
                write!(f, "cpu-config {collection} contains a duplicate identity")
            }
            Self::FeatureIndexOutOfRange => {
                f.write_str("cpu-config vcpu_features contains an out-of-range feature index")
            }
            Self::InvalidRegisterArchitecture => f.write_str(
                "cpu-config reg_modifiers contains a non-arm64 register identity",
            ),
            Self::ValueOutsideFilter { collection } => write!(
                f,
                "cpu-config {collection} contains value bits outside its filter"
            ),
            Self::ValueOutsideRegisterWidth => f.write_str(
                "cpu-config reg_modifiers contains a bitmap outside its register width",
            ),
            Self::KvmCapabilitiesUnsupported => f.write_str(
                "cpu-config kvm_capabilities are KVM-specific and are not supported on arm64 HVF",
            ),
            Self::VcpuFeaturesUnsupported => f.write_str(
                "cpu-config vcpu_features are KVM vCPU-init-specific and are not supported on arm64 HVF",
            ),
            Self::MixedUnsupported => f.write_str(
                "mixed cpu-config categories include KVM-specific or unsupported inputs on arm64 HVF",
            ),
            Self::UnsupportedRegisterWidth => f.write_str(
                "cpu-config reg_modifiers contains a register width not supported by this arm64 HVF profile",
            ),
            Self::UnsupportedRegister => f.write_str(
                "cpu-config reg_modifiers contains a register outside the supported arm64 HVF identification-register profile",
            ),
        }
    }
}

impl std::error::Error for CpuConfigError {}

fn validate_len(len: usize, collection: CpuConfigCollection) -> Result<(), CpuConfigError> {
    if len > CPU_CONFIG_MAX_ENTRIES_PER_ARRAY {
        Err(CpuConfigError::TooManyEntries { collection })
    } else {
        Ok(())
    }
}

const fn width_limit(width: CpuConfigArmRegisterWidth) -> Option<u128> {
    match width {
        CpuConfigArmRegisterWidth::U32 => Some(u32::MAX as u128),
        CpuConfigArmRegisterWidth::U64 => Some(u64::MAX as u128),
        CpuConfigArmRegisterWidth::U128 => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modifier(
        id: u64,
        width: CpuConfigArmRegisterWidth,
        filter: u128,
        value: u128,
    ) -> CpuConfigArmRegisterModifier {
        CpuConfigArmRegisterModifier::new(id, width, filter, value)
    }

    #[test]
    fn empty_input_clears_template() {
        assert_eq!(CpuConfigInput::noop().category(), None);
        assert_eq!(CpuConfigInput::noop().into_custom_template(), Ok(None));
    }

    #[test]
    fn prepares_the_four_supported_id_registers_in_order() {
        let input = CpuConfigInput::new(
            Vec::new(),
            vec![
                modifier(
                    KVM_REG_ARM64_ID_AA64PFR0_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    0x000f_000f_0000_0000,
                    0,
                ),
                modifier(
                    KVM_REG_ARM64_ID_AA64ISAR0_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    0xf0ff_0fff_0000_f000,
                    0x1000,
                ),
                modifier(
                    KVM_REG_ARM64_ID_AA64ISAR1_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    0x00ff_f000_00ff_f00f,
                    0x0010_0001,
                ),
                modifier(
                    KVM_REG_ARM64_ID_AA64MMFR2_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    0x0000_000f_0000_0000,
                    0,
                ),
            ],
            Vec::new(),
        );

        let template = input
            .into_custom_template()
            .expect("supported template should validate")
            .expect("nonempty template should be retained");
        assert_eq!(template.modifiers().len(), 4);
        assert_eq!(template.modifiers()[0].register(), ArmIdRegister::Pfr0);
        assert_eq!(template.modifiers()[1].register(), ArmIdRegister::Isar0);
        assert_eq!(template.modifiers()[2].register(), ArmIdRegister::Isar1);
        assert_eq!(template.modifiers()[3].register(), ArmIdRegister::Mmfr2);
        assert_eq!(
            template.modifiers()[1].apply(u64::MAX),
            0x0f00_f000_ffff_1fff
        );
    }

    #[test]
    fn rejects_kvm_only_mixed_width_and_unknown_register_inputs_distinctly() {
        assert_eq!(
            CpuConfigInput::new(vec![CpuConfigKvmCapability::Add(1)], Vec::new(), Vec::new(),)
                .into_custom_template(),
            Err(CpuConfigError::KvmCapabilitiesUnsupported)
        );
        assert_eq!(
            CpuConfigInput::new(
                Vec::new(),
                Vec::new(),
                vec![CpuConfigVcpuFeature::new(0, 1, 1)],
            )
            .into_custom_template(),
            Err(CpuConfigError::VcpuFeaturesUnsupported)
        );
        assert_eq!(
            CpuConfigInput::new(
                vec![CpuConfigKvmCapability::Add(1)],
                vec![modifier(
                    KVM_REG_ARM64_ID_AA64PFR0_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    1,
                    1,
                )],
                Vec::new(),
            )
            .into_custom_template(),
            Err(CpuConfigError::MixedUnsupported)
        );
        assert_eq!(
            CpuConfigInput::new(
                Vec::new(),
                vec![modifier(
                    0x6020_0000_0013_c020,
                    CpuConfigArmRegisterWidth::U32,
                    1,
                    1,
                )],
                Vec::new(),
            )
            .into_custom_template(),
            Err(CpuConfigError::UnsupportedRegisterWidth)
        );
        assert_eq!(
            CpuConfigInput::new(
                Vec::new(),
                vec![modifier(
                    0x6030_0000_0010_0000,
                    CpuConfigArmRegisterWidth::U64,
                    1,
                    1,
                )],
                Vec::new(),
            )
            .into_custom_template(),
            Err(CpuConfigError::UnsupportedRegister)
        );
    }

    #[test]
    fn revalidates_publicly_constructible_shape_without_leaking_values() {
        let duplicate = CpuConfigInput::new(
            vec![
                CpuConfigKvmCapability::Add(4_000_000_001),
                CpuConfigKvmCapability::Remove(4_000_000_001),
            ],
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(
            duplicate.into_custom_template(),
            Err(CpuConfigError::DuplicateIdentity {
                collection: CpuConfigCollection::KvmCapabilities,
            })
        );

        let bad_filter = CpuConfigInput::new(
            Vec::new(),
            vec![modifier(
                KVM_REG_ARM64_ID_AA64PFR0_EL1,
                CpuConfigArmRegisterWidth::U64,
                0,
                0xdead_beef,
            )],
            Vec::new(),
        );
        let error = bad_filter
            .into_custom_template()
            .expect_err("value outside filter should fail");
        assert_eq!(
            error,
            CpuConfigError::ValueOutsideFilter {
                collection: CpuConfigCollection::RegisterModifiers,
            }
        );
        assert!(!error.to_string().contains("dead"));
        assert!(!format!("{error:?}").contains("dead"));
    }

    #[test]
    fn debug_is_value_redacted_at_every_boundary() {
        let input = CpuConfigInput::new(
            vec![CpuConfigKvmCapability::Add(4_000_000_001)],
            vec![modifier(
                KVM_REG_ARM64_ID_AA64PFR0_EL1,
                CpuConfigArmRegisterWidth::U64,
                0xdead_beef_dead_beef,
                0xdead_beef,
            )],
            Vec::new(),
        );
        let debug = format!("{input:?}");
        for secret in ["4000000001", "603000000013c020", "dead"] {
            assert!(!debug.contains(secret), "debug leaked {secret}: {debug}");
        }
    }
}
