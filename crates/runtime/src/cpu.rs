//! Backend-neutral CPU-template input and executable model.

use std::collections::HashSet;
use std::fmt;

pub const CPU_CONFIG_MAX_ENTRIES_PER_ARRAY: usize = 256;
pub const CPU_CONFIG_KVM_VCPU_FEATURE_WORDS: u32 = 7;

pub const KVM_REG_ARM64_ID_AA64PFR0_EL1: u64 = 0x6030_0000_0013_c020;
pub const KVM_REG_ARM64_ID_AA64PFR1_EL1: u64 = 0x6030_0000_0013_c021;
pub const KVM_REG_ARM64_ID_AA64ZFR0_EL1: u64 = 0x6030_0000_0013_c024;
pub const KVM_REG_ARM64_ID_AA64SMFR0_EL1: u64 = 0x6030_0000_0013_c025;
pub const KVM_REG_ARM64_ID_AA64DFR0_EL1: u64 = 0x6030_0000_0013_c028;
pub const KVM_REG_ARM64_ID_AA64DFR1_EL1: u64 = 0x6030_0000_0013_c029;
pub const KVM_REG_ARM64_ID_AA64ISAR0_EL1: u64 = 0x6030_0000_0013_c030;
pub const KVM_REG_ARM64_ID_AA64ISAR1_EL1: u64 = 0x6030_0000_0013_c031;
pub const KVM_REG_ARM64_ID_AA64MMFR0_EL1: u64 = 0x6030_0000_0013_c038;
pub const KVM_REG_ARM64_ID_AA64MMFR1_EL1: u64 = 0x6030_0000_0013_c039;
pub const KVM_REG_ARM64_ID_AA64MMFR2_EL1: u64 = 0x6030_0000_0013_c03a;
pub const KVM_REG_ARM64_ACTLR_EL1: u64 = 0x6030_0000_0013_c081;

const ARM64_ACTLR_ENTSO_MASK: u64 = 1 << 1;

const KVM_REG_ARM64_CORE_U32_BASE: u64 = 0x6020_0000_0010_0000;
const KVM_REG_ARM64_CORE_U64_BASE: u64 = 0x6030_0000_0010_0000;
const KVM_REG_ARM64_CORE_U128_BASE: u64 = 0x6040_0000_0010_0000;
const KVM_REG_ARM64_CORE_INDEX_MASK: u64 = 0xffff;

pub const KVM_REG_ARM64_CORE_SP_EL0: u64 = KVM_REG_ARM64_CORE_U64_BASE | 62;
pub const KVM_REG_ARM64_CORE_PC: u64 = KVM_REG_ARM64_CORE_U64_BASE | 64;
pub const KVM_REG_ARM64_CORE_PSTATE: u64 = KVM_REG_ARM64_CORE_U64_BASE | 66;
pub const KVM_REG_ARM64_CORE_SP_EL1: u64 = KVM_REG_ARM64_CORE_U64_BASE | 68;
pub const KVM_REG_ARM64_CORE_ELR_EL1: u64 = KVM_REG_ARM64_CORE_U64_BASE | 70;
pub const KVM_REG_ARM64_CORE_SPSR_EL1: u64 = KVM_REG_ARM64_CORE_U64_BASE | 72;
pub const KVM_REG_ARM64_CORE_FPSR: u64 = KVM_REG_ARM64_CORE_U32_BASE | 212;
pub const KVM_REG_ARM64_CORE_FPCR: u64 = KVM_REG_ARM64_CORE_U32_BASE | 213;

const KVM_REG_ARM64_CORE_SPSR_ABT: u64 = KVM_REG_ARM64_CORE_U64_BASE | 74;
const KVM_REG_ARM64_CORE_SPSR_UND: u64 = KVM_REG_ARM64_CORE_U64_BASE | 76;
const KVM_REG_ARM64_CORE_SPSR_IRQ: u64 = KVM_REG_ARM64_CORE_U64_BASE | 78;
const KVM_REG_ARM64_CORE_SPSR_FIQ: u64 = KVM_REG_ARM64_CORE_U64_BASE | 80;

/// Return the canonical KVM arm64 core identity for X0-X30.
pub const fn kvm_reg_arm64_core_x(index: u8) -> Option<u64> {
    if index <= 30 {
        Some(KVM_REG_ARM64_CORE_U64_BASE | (index as u64 * 2))
    } else {
        None
    }
}

/// Return the canonical KVM arm64 core identity for Q0-Q31.
pub const fn kvm_reg_arm64_core_q(index: u8) -> Option<u64> {
    if index <= 31 {
        Some(KVM_REG_ARM64_CORE_U128_BASE | (84 + index as u64 * 4))
    } else {
        None
    }
}

const ARM64_KVM_REG_ARCH_MASK: u64 = 0xff00_0000_0000_0000;
const ARM64_KVM_REG_ARCH: u64 = 0x6000_0000_0000_0000;
const ARM64_KVM_REG_SIZE_MASK: u64 = 0x00f0_0000_0000_0000;
const ARM64_KVM_REG_SIZE_SHIFT: u32 = 52;
const ARM64_KVM_REG_COPROC_MASK: u64 = 0x0000_0000_0fff_0000;
const ARM64_KVM_REG_CORE: u64 = 0x0000_0000_0010_0000;
const ARM64_KVM_REG_DEMUX: u64 = 0x0000_0000_0011_0000;
const ARM64_KVM_REG_SYSTEM: u64 = 0x0000_0000_0013_0000;
const ARM64_KVM_REG_FIRMWARE: u64 = 0x0000_0000_0014_0000;
const ARM64_KVM_REG_SVE: u64 = 0x0000_0000_0015_0000;
const ARM64_KVM_REG_FIRMWARE_FEATURE: u64 = 0x0000_0000_0016_0000;
const ARM64_KVM_REG_PAYLOAD_MASK: u64 = 0xffff;
const ARM64_KVM_REG_CANONICAL_MASK: u64 = ARM64_KVM_REG_ARCH_MASK
    | ARM64_KVM_REG_SIZE_MASK
    | ARM64_KVM_REG_COPROC_MASK
    | ARM64_KVM_REG_PAYLOAD_MASK;
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
                let mut modifiers = Vec::with_capacity(self.reg_modifiers.len());
                for input in self.reg_modifiers {
                    let modifier = input.into_executable()?;
                    if modifiers
                        .iter()
                        .any(|existing| modifier.has_same_target(*existing))
                    {
                        return Err(CpuConfigError::DuplicateIdentity {
                            collection: CpuConfigCollection::RegisterModifiers,
                        });
                    }
                    modifiers.push(modifier);
                }
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
        if arm64_register_width(self.id) != Some(self.width) {
            return Err(CpuConfigError::InvalidRegisterWidth);
        }
        if self.id & !ARM64_KVM_REG_CANONICAL_MASK != 0 {
            return Err(CpuConfigError::InvalidRegisterIdentity);
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

    fn into_executable(self) -> Result<ArmRegisterModifier, CpuConfigError> {
        match self.width {
            CpuConfigArmRegisterWidth::U32 => {
                let register = match core_class_index(self.id) {
                    Some(212) => ArmRegister32::Fpsr,
                    Some(213) => ArmRegister32::Fpcr,
                    Some(index) if expected_core_width(index).is_some() => {
                        return Err(CpuConfigError::InvalidRegisterWidth);
                    }
                    Some(_) => return Err(CpuConfigError::InvalidRegisterIdentity),
                    None => return Err(classify_unaccepted_register(self.id)),
                };
                Ok(ArmRegisterModifier::U32 {
                    register,
                    filter: u32::try_from(self.filter)
                        .map_err(|_| CpuConfigError::ValueOutsideRegisterWidth)?,
                    value: u32::try_from(self.value)
                        .map_err(|_| CpuConfigError::ValueOutsideRegisterWidth)?,
                })
            }
            CpuConfigArmRegisterWidth::U64 => {
                let register = classify_u64_register(self.id)?;
                let filter = u64::try_from(self.filter)
                    .map_err(|_| CpuConfigError::ValueOutsideRegisterWidth)?;
                if accepted_system_register(self.id)
                    .is_some_and(|entry| filter & !entry.allowed_filter != 0)
                {
                    return Err(CpuConfigError::ActlrFilterUnsupported);
                }
                Ok(ArmRegisterModifier::U64 {
                    register,
                    filter,
                    value: u64::try_from(self.value)
                        .map_err(|_| CpuConfigError::ValueOutsideRegisterWidth)?,
                })
            }
            CpuConfigArmRegisterWidth::U128 => {
                let Some(index) = core_class_index(self.id) else {
                    return Err(classify_unaccepted_register(self.id));
                };
                if !(84..=208).contains(&index) || !(index - 84).is_multiple_of(4) {
                    return if expected_core_width(index).is_some() {
                        Err(CpuConfigError::InvalidRegisterWidth)
                    } else {
                        Err(CpuConfigError::InvalidRegisterIdentity)
                    };
                }
                Ok(ArmRegisterModifier::U128 {
                    register: ArmRegister128::Q(ArmQRegister(((index - 84) / 4) as u8)),
                    filter: self.filter,
                    value: self.value,
                })
            }
        }
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
    modifiers: Vec<ArmRegisterModifier>,
}

impl CustomCpuTemplate {
    pub fn modifiers(&self) -> &[ArmRegisterModifier] {
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
    Pfr1,
    Zfr0,
    Smfr0,
    Dfr0,
    Dfr1,
    Isar0,
    Isar1,
    Mmfr0,
    Mmfr1,
    Mmfr2,
}

impl fmt::Debug for ArmIdRegister {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(CPU_CONFIG_VALUE_REDACTED)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ArmRegister32 {
    Fpcr,
    Fpsr,
}

impl fmt::Debug for ArmRegister32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(CPU_CONFIG_VALUE_REDACTED)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ArmGeneralRegister(u8);

impl ArmGeneralRegister {
    /// Return the validated architectural X-register index.
    pub const fn index(self) -> u8 {
        self.0
    }
}

impl fmt::Debug for ArmGeneralRegister {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(CPU_CONFIG_VALUE_REDACTED)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ArmRegister64 {
    X(ArmGeneralRegister),
    Pc,
    Pstate,
    SpEl0,
    SpEl1,
    ElrEl1,
    SpsrEl1,
    Id(ArmIdRegister),
    Actlr,
}

impl ArmRegister64 {
    pub const fn boot_disposition(self) -> ArmRegisterBootDisposition {
        match self {
            Self::X(register) if register.index() == 0 => {
                ArmRegisterBootDisposition::AppliedThenBootOverridden
            }
            Self::Pc | Self::Pstate => ArmRegisterBootDisposition::AppliedThenBootOverridden,
            Self::X(_)
            | Self::SpEl0
            | Self::SpEl1
            | Self::ElrEl1
            | Self::SpsrEl1
            | Self::Id(_)
            | Self::Actlr => ArmRegisterBootDisposition::Retained,
        }
    }

    /// Return the public Hypervisor.framework availability tier required by
    /// this closed register target.
    pub const fn availability(self) -> ArmRegisterAvailability {
        match self {
            Self::Id(ArmIdRegister::Zfr0 | ArmIdRegister::Smfr0) => {
                ArmRegisterAvailability::MacOs15_2
            }
            Self::Actlr => ArmRegisterAvailability::MacOs15_0,
            Self::X(_)
            | Self::Pc
            | Self::Pstate
            | Self::SpEl0
            | Self::SpEl1
            | Self::ElrEl1
            | Self::SpsrEl1
            | Self::Id(_) => ArmRegisterAvailability::Baseline,
        }
    }
}

impl fmt::Debug for ArmRegister64 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(CPU_CONFIG_VALUE_REDACTED)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ArmQRegister(u8);

impl ArmQRegister {
    /// Return the validated architectural Q-register index.
    pub const fn index(self) -> u8 {
        self.0
    }
}

impl fmt::Debug for ArmQRegister {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(CPU_CONFIG_VALUE_REDACTED)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ArmRegister128 {
    Q(ArmQRegister),
}

impl fmt::Debug for ArmRegister128 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(CPU_CONFIG_VALUE_REDACTED)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmRegisterBootDisposition {
    Retained,
    AppliedThenBootOverridden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ArmRegisterAvailability {
    /// Available at the baseline public Hypervisor.framework register boundary.
    Baseline,
    /// Requires the repository's existing public macOS 15 VM boundary.
    MacOs15_0,
    /// Requires the public macOS 15.2 system-register boundary.
    MacOs15_2,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ArmRegisterModifier {
    U32 {
        register: ArmRegister32,
        filter: u32,
        value: u32,
    },
    U64 {
        register: ArmRegister64,
        filter: u64,
        value: u64,
    },
    U128 {
        register: ArmRegister128,
        filter: u128,
        value: u128,
    },
}

impl ArmRegisterModifier {
    pub const fn width(self) -> CpuConfigArmRegisterWidth {
        match self {
            Self::U32 { .. } => CpuConfigArmRegisterWidth::U32,
            Self::U64 { .. } => CpuConfigArmRegisterWidth::U64,
            Self::U128 { .. } => CpuConfigArmRegisterWidth::U128,
        }
    }

    fn has_same_target(self, other: Self) -> bool {
        match (self, other) {
            (
                Self::U32 { register: left, .. },
                Self::U32 {
                    register: right, ..
                },
            ) => left == right,
            (
                Self::U64 { register: left, .. },
                Self::U64 {
                    register: right, ..
                },
            ) => left == right,
            (
                Self::U128 { register: left, .. },
                Self::U128 {
                    register: right, ..
                },
            ) => left == right,
            _ => false,
        }
    }
}

impl fmt::Debug for ArmRegisterModifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ArmRegisterModifier")
            .field("width", &self.width())
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
    InvalidRegisterWidth,
    ValueOutsideFilter { collection: CpuConfigCollection },
    ValueOutsideRegisterWidth,
    KvmCapabilitiesUnsupported,
    VcpuFeaturesUnsupported,
    MixedUnsupported,
    BootReservedRegister,
    Aarch32BankedRegisterUnavailable,
    InvalidRegisterIdentity,
    KvmDemuxRegisterUnsupported,
    KvmFirmwareRegisterUnsupported,
    KvmFirmwareFeatureRegisterUnsupported,
    KvmSveRegisterUnsupported,
    UnknownKvmRegisterClass,
    TopologyRegisterUnsupported,
    RegisterAliasUnsupported,
    LifecycleRegisterUnsupported,
    SecuritySensitiveRegisterUnsupported,
    TimeDependentRegisterUnsupported,
    SeparatelyOwnedRegisterUnsupported,
    MutableSmeRegisterUnsupported,
    DisabledEl2RegisterUnsupported,
    UnnamedSystemRegisterUnsupported,
    ActlrFilterUnsupported,
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
            Self::InvalidRegisterWidth => f.write_str(
                "cpu-config reg_modifiers contains an invalid register-width encoding",
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
            Self::BootReservedRegister => f.write_str(
                "cpu-config reg_modifiers contains an arm64 register reserved by the boot protocol",
            ),
            Self::Aarch32BankedRegisterUnavailable => f.write_str(
                "cpu-config reg_modifiers contains AArch32 banked state unavailable through public arm64 HVF",
            ),
            Self::InvalidRegisterIdentity => f.write_str(
                "cpu-config reg_modifiers contains a noncanonical arm64 register identity",
            ),
            Self::KvmDemuxRegisterUnsupported => f.write_str(
                "cpu-config reg_modifiers contains KVM demultiplexed cache state without a public HVF CPU-template equivalent",
            ),
            Self::KvmFirmwareRegisterUnsupported => f.write_str(
                "cpu-config reg_modifiers contains KVM firmware pseudo-register state without a public HVF equivalent",
            ),
            Self::KvmFirmwareFeatureRegisterUnsupported => f.write_str(
                "cpu-config reg_modifiers contains KVM firmware-feature state without a public HVF equivalent",
            ),
            Self::KvmSveRegisterUnsupported => f.write_str(
                "cpu-config reg_modifiers contains KVM SVE state owned by a separate arm64 HVF interface",
            ),
            Self::UnknownKvmRegisterClass => f.write_str(
                "cpu-config reg_modifiers contains an unknown arm64 KVM register class",
            ),
            Self::TopologyRegisterUnsupported => f.write_str(
                "cpu-config reg_modifiers contains host identity or per-vCPU topology state",
            ),
            Self::RegisterAliasUnsupported => f.write_str(
                "cpu-config reg_modifiers contains an alias of an existing typed arm64 register target",
            ),
            Self::LifecycleRegisterUnsupported => f.write_str(
                "cpu-config reg_modifiers contains arm64 boot or lifecycle-dependent system state",
            ),
            Self::SecuritySensitiveRegisterUnsupported => f.write_str(
                "cpu-config reg_modifiers contains security-sensitive arm64 system state",
            ),
            Self::TimeDependentRegisterUnsupported => f.write_str(
                "cpu-config reg_modifiers contains time-dependent arm64 system state",
            ),
            Self::SeparatelyOwnedRegisterUnsupported => f.write_str(
                "cpu-config reg_modifiers contains arm64 state owned by a separate device or lifecycle interface",
            ),
            Self::MutableSmeRegisterUnsupported => f.write_str(
                "cpu-config reg_modifiers contains mutable SME state outside the static CPU-template profile",
            ),
            Self::DisabledEl2RegisterUnsupported => f.write_str(
                "cpu-config reg_modifiers contains EL2 state while the VM feature is disabled",
            ),
            Self::UnnamedSystemRegisterUnsupported => f.write_str(
                "cpu-config reg_modifiers contains an arm64 system encoding outside the finite public HVF profile",
            ),
            Self::ActlrFilterUnsupported => f.write_str(
                "cpu-config reg_modifiers contains ACTLR bits outside the public EnTSO boundary",
            ),
        }
    }
}

impl std::error::Error for CpuConfigError {}

#[derive(Clone, Copy)]
struct Arm64SystemRegisterProfile {
    identity: u64,
    register: ArmRegister64,
    width: CpuConfigArmRegisterWidth,
    boot_disposition: ArmRegisterBootDisposition,
    availability: ArmRegisterAvailability,
    allowed_filter: u64,
}

impl Arm64SystemRegisterProfile {
    const fn accepted(identity: u64, register: ArmRegister64, allowed_filter: u64) -> Self {
        Self {
            identity,
            register,
            width: CpuConfigArmRegisterWidth::U64,
            boot_disposition: register.boot_disposition(),
            availability: register.availability(),
            allowed_filter,
        }
    }
}

const ARM64_SYSTEM_REGISTER_PROFILE: [Arm64SystemRegisterProfile; 12] = [
    Arm64SystemRegisterProfile::accepted(
        KVM_REG_ARM64_ID_AA64PFR0_EL1,
        ArmRegister64::Id(ArmIdRegister::Pfr0),
        u64::MAX,
    ),
    Arm64SystemRegisterProfile::accepted(
        KVM_REG_ARM64_ID_AA64PFR1_EL1,
        ArmRegister64::Id(ArmIdRegister::Pfr1),
        u64::MAX,
    ),
    Arm64SystemRegisterProfile::accepted(
        KVM_REG_ARM64_ID_AA64DFR0_EL1,
        ArmRegister64::Id(ArmIdRegister::Dfr0),
        u64::MAX,
    ),
    Arm64SystemRegisterProfile::accepted(
        KVM_REG_ARM64_ID_AA64DFR1_EL1,
        ArmRegister64::Id(ArmIdRegister::Dfr1),
        u64::MAX,
    ),
    Arm64SystemRegisterProfile::accepted(
        KVM_REG_ARM64_ID_AA64ISAR0_EL1,
        ArmRegister64::Id(ArmIdRegister::Isar0),
        u64::MAX,
    ),
    Arm64SystemRegisterProfile::accepted(
        KVM_REG_ARM64_ID_AA64ISAR1_EL1,
        ArmRegister64::Id(ArmIdRegister::Isar1),
        u64::MAX,
    ),
    Arm64SystemRegisterProfile::accepted(
        KVM_REG_ARM64_ID_AA64MMFR0_EL1,
        ArmRegister64::Id(ArmIdRegister::Mmfr0),
        u64::MAX,
    ),
    Arm64SystemRegisterProfile::accepted(
        KVM_REG_ARM64_ID_AA64MMFR1_EL1,
        ArmRegister64::Id(ArmIdRegister::Mmfr1),
        u64::MAX,
    ),
    Arm64SystemRegisterProfile::accepted(
        KVM_REG_ARM64_ID_AA64MMFR2_EL1,
        ArmRegister64::Id(ArmIdRegister::Mmfr2),
        u64::MAX,
    ),
    Arm64SystemRegisterProfile::accepted(
        KVM_REG_ARM64_ID_AA64ZFR0_EL1,
        ArmRegister64::Id(ArmIdRegister::Zfr0),
        u64::MAX,
    ),
    Arm64SystemRegisterProfile::accepted(
        KVM_REG_ARM64_ID_AA64SMFR0_EL1,
        ArmRegister64::Id(ArmIdRegister::Smfr0),
        u64::MAX,
    ),
    Arm64SystemRegisterProfile::accepted(
        KVM_REG_ARM64_ACTLR_EL1,
        ArmRegister64::Actlr,
        ARM64_ACTLR_ENTSO_MASK,
    ),
];

fn accepted_system_register(id: u64) -> Option<Arm64SystemRegisterProfile> {
    ARM64_SYSTEM_REGISTER_PROFILE
        .iter()
        .copied()
        .find(|entry| entry.identity == id)
}

fn classify_u64_register(id: u64) -> Result<ArmRegister64, CpuConfigError> {
    if let Some(entry) = accepted_system_register(id) {
        debug_assert_eq!(entry.width, CpuConfigArmRegisterWidth::U64);
        debug_assert_eq!(entry.boot_disposition, entry.register.boot_disposition());
        debug_assert_eq!(entry.availability, entry.register.availability());
        return Ok(entry.register);
    }
    if matches!(
        id,
        KVM_REG_ARM64_CORE_SPSR_ABT
            | KVM_REG_ARM64_CORE_SPSR_UND
            | KVM_REG_ARM64_CORE_SPSR_IRQ
            | KVM_REG_ARM64_CORE_SPSR_FIQ
    ) {
        return Err(CpuConfigError::Aarch32BankedRegisterUnavailable);
    }

    let Some(index) = core_index(id, KVM_REG_ARM64_CORE_U64_BASE) else {
        return Err(classify_unaccepted_register(id));
    };
    match index {
        0..=60 if index.is_multiple_of(2) => {
            let register_index = (index / 2) as u8;
            if (1..=3).contains(&register_index) {
                Err(CpuConfigError::BootReservedRegister)
            } else {
                Ok(ArmRegister64::X(ArmGeneralRegister(register_index)))
            }
        }
        62 => Ok(ArmRegister64::SpEl0),
        64 => Ok(ArmRegister64::Pc),
        66 => Ok(ArmRegister64::Pstate),
        68 => Ok(ArmRegister64::SpEl1),
        70 => Ok(ArmRegister64::ElrEl1),
        72 => Ok(ArmRegister64::SpsrEl1),
        _ if expected_core_width(index).is_some() => Err(CpuConfigError::InvalidRegisterWidth),
        _ => Err(CpuConfigError::InvalidRegisterIdentity),
    }
}

fn classify_unaccepted_register(id: u64) -> CpuConfigError {
    if id & !ARM64_KVM_REG_CANONICAL_MASK != 0 {
        return CpuConfigError::InvalidRegisterIdentity;
    }

    match id & ARM64_KVM_REG_COPROC_MASK {
        ARM64_KVM_REG_CORE => match core_class_index(id) {
            Some(index) if expected_core_width(index).is_some() => {
                CpuConfigError::InvalidRegisterWidth
            }
            _ => CpuConfigError::InvalidRegisterIdentity,
        },
        ARM64_KVM_REG_DEMUX => {
            if id & ARM64_KVM_REG_PAYLOAD_MASK > 0xff {
                CpuConfigError::InvalidRegisterIdentity
            } else if arm64_register_width(id) != Some(CpuConfigArmRegisterWidth::U32) {
                CpuConfigError::InvalidRegisterWidth
            } else {
                CpuConfigError::KvmDemuxRegisterUnsupported
            }
        }
        ARM64_KVM_REG_SYSTEM => {
            if arm64_register_width(id) != Some(CpuConfigArmRegisterWidth::U64) {
                CpuConfigError::InvalidRegisterWidth
            } else {
                classify_unaccepted_system_register(id as u16)
            }
        }
        ARM64_KVM_REG_FIRMWARE => {
            if arm64_register_width(id) != Some(CpuConfigArmRegisterWidth::U64) {
                CpuConfigError::InvalidRegisterWidth
            } else {
                CpuConfigError::KvmFirmwareRegisterUnsupported
            }
        }
        ARM64_KVM_REG_SVE => CpuConfigError::KvmSveRegisterUnsupported,
        ARM64_KVM_REG_FIRMWARE_FEATURE => {
            if arm64_register_width(id) != Some(CpuConfigArmRegisterWidth::U64) {
                CpuConfigError::InvalidRegisterWidth
            } else {
                CpuConfigError::KvmFirmwareFeatureRegisterUnsupported
            }
        }
        _ => CpuConfigError::UnknownKvmRegisterClass,
    }
}

fn classify_unaccepted_system_register(register: u16) -> CpuConfigError {
    if ARM64_SYSTEM_REGISTER_PROFILE
        .iter()
        .any(|entry| entry.identity as u16 == register)
    {
        return CpuConfigError::InvalidRegisterWidth;
    }
    if is_debug_system_register(register) || is_pointer_authentication_key(register) {
        return CpuConfigError::SecuritySensitiveRegisterUnsupported;
    }

    match register {
        0xc000 | 0xc005 => CpuConfigError::TopologyRegisterUnsupported,
        0xc200 | 0xc201 | 0xc208 | 0xe208 => CpuConfigError::RegisterAliasUnsupported,
        0xc094 | 0xc096 | 0xda12 | 0xde85 => CpuConfigError::MutableSmeRegisterUnsupported,
        0xc080 | 0xc082 | 0xc100 | 0xc101 | 0xc102 | 0xc288 | 0xc289 | 0xc290 | 0xc300 | 0xc3a0
        | 0xc510 | 0xc518 | 0xc600 | 0xc681 | 0xc684 | 0xc687 | 0xde82 | 0xde83 | 0xde87 => {
            CpuConfigError::LifecycleRegisterUnsupported
        }
        0xc708 | 0xdf00 | 0xdf01 | 0xdf02 | 0xdf10 | 0xdf11 | 0xdf12 | 0xdf18 | 0xdf19 | 0xdf1a => {
            CpuConfigError::TimeDependentRegisterUnsupported
        }
        0xd000 | 0xc230 | 0xc643 | 0xc644..=0xc64b | 0xc663..=0xc667 => {
            CpuConfigError::SeparatelyOwnedRegisterUnsupported
        }
        0xe000 | 0xe005 | 0xe080 | 0xe088 | 0xe089 | 0xe08a | 0xe100 | 0xe101 | 0xe102 | 0xe108
        | 0xe10a | 0xe200 | 0xe201 | 0xe290 | 0xe300 | 0xe304 | 0xe510 | 0xe600 | 0xe682
        | 0xe703 | 0xe708 | 0xe710 | 0xe711 | 0xe712 | 0xf208 => {
            CpuConfigError::DisabledEl2RegisterUnsupported
        }
        _ => CpuConfigError::UnnamedSystemRegisterUnsupported,
    }
}

fn is_debug_system_register(register: u16) -> bool {
    matches!(register, 0x8010 | 0x8012)
        || ((0x8004..=0x807f).contains(&register) && (4..=7).contains(&(register & 7)))
}

fn is_pointer_authentication_key(register: u16) -> bool {
    matches!(
        register,
        0xc108 | 0xc109 | 0xc10a | 0xc10b | 0xc110 | 0xc111 | 0xc112 | 0xc113 | 0xc118 | 0xc119
    )
}

const fn core_class_index(id: u64) -> Option<u64> {
    if id & ARM64_KVM_REG_COPROC_MASK == ARM64_KVM_REG_CORE {
        Some(id & KVM_REG_ARM64_CORE_INDEX_MASK)
    } else {
        None
    }
}

const fn expected_core_width(index: u64) -> Option<CpuConfigArmRegisterWidth> {
    match index {
        0..=80 if index.is_multiple_of(2) => Some(CpuConfigArmRegisterWidth::U64),
        84..=208 if (index - 84).is_multiple_of(4) => Some(CpuConfigArmRegisterWidth::U128),
        212 | 213 => Some(CpuConfigArmRegisterWidth::U32),
        _ => None,
    }
}

const fn core_index(id: u64, base: u64) -> Option<u64> {
    if id & !KVM_REG_ARM64_CORE_INDEX_MASK == base {
        Some(id & KVM_REG_ARM64_CORE_INDEX_MASK)
    } else {
        None
    }
}

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

const fn arm64_register_width(id: u64) -> Option<CpuConfigArmRegisterWidth> {
    match (id & ARM64_KVM_REG_SIZE_MASK) >> ARM64_KVM_REG_SIZE_SHIFT {
        2 => Some(CpuConfigArmRegisterWidth::U32),
        3 => Some(CpuConfigArmRegisterWidth::U64),
        4 => Some(CpuConfigArmRegisterWidth::U128),
        _ => None,
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

    const fn arm_x(index: u8) -> ArmRegister64 {
        ArmRegister64::X(ArmGeneralRegister(index))
    }

    const fn arm_q(index: u8) -> ArmRegister128 {
        ArmRegister128::Q(ArmQRegister(index))
    }

    const fn system_register(raw: u16) -> u64 {
        0x6030_0000_0013_0000 | raw as u64
    }

    fn u32_modifier_parts(modifier: ArmRegisterModifier) -> (ArmRegister32, u32, u32) {
        let ArmRegisterModifier::U32 {
            register,
            filter,
            value,
        } = modifier
        else {
            panic!("expected U32 register modifier")
        };
        (register, filter, value)
    }

    fn u64_modifier_parts(modifier: ArmRegisterModifier) -> (ArmRegister64, u64, u64) {
        let ArmRegisterModifier::U64 {
            register,
            filter,
            value,
        } = modifier
        else {
            panic!("expected U64 register modifier")
        };
        (register, filter, value)
    }

    fn u128_modifier_parts(modifier: ArmRegisterModifier) -> (ArmRegister128, u128, u128) {
        let ArmRegisterModifier::U128 {
            register,
            filter,
            value,
        } = modifier
        else {
            panic!("expected U128 register modifier")
        };
        (register, filter, value)
    }

    fn executable_modifier(
        id: u64,
        width: CpuConfigArmRegisterWidth,
        filter: u128,
        value: u128,
    ) -> Result<ArmRegisterModifier, CpuConfigError> {
        let template = CpuConfigInput::new(
            Vec::new(),
            vec![modifier(id, width, filter, value)],
            Vec::new(),
        )
        .into_custom_template()?;
        Ok(template
            .expect("one modifier should produce a custom template")
            .modifiers()[0])
    }

    #[test]
    fn empty_input_clears_template() {
        assert_eq!(CpuConfigInput::noop().category(), None);
        assert_eq!(CpuConfigInput::noop().into_custom_template(), Ok(None));
    }

    #[test]
    fn prepares_the_complete_system_register_profile_in_order() {
        let input = CpuConfigInput::new(
            Vec::new(),
            ARM64_SYSTEM_REGISTER_PROFILE
                .iter()
                .map(|entry| {
                    modifier(
                        entry.identity,
                        CpuConfigArmRegisterWidth::U64,
                        entry.allowed_filter.into(),
                        0,
                    )
                })
                .collect(),
            Vec::new(),
        );

        let template = input
            .into_custom_template()
            .expect("supported template should validate")
            .expect("nonempty template should be retained");
        assert_eq!(template.modifiers().len(), 12);
        for (modifier, entry) in template
            .modifiers()
            .iter()
            .copied()
            .zip(ARM64_SYSTEM_REGISTER_PROFILE)
        {
            let (register, filter, value) = u64_modifier_parts(modifier);
            assert_eq!(register, entry.register);
            assert_eq!(filter, entry.allowed_filter);
            assert_eq!(value, 0);
            assert_eq!(entry.width, CpuConfigArmRegisterWidth::U64);
            assert_eq!(entry.boot_disposition, ArmRegisterBootDisposition::Retained);
            assert_eq!(register.boot_disposition(), entry.boot_disposition);
            assert_eq!(register.availability(), entry.availability);
        }

        assert_eq!(
            ArmRegister64::Id(ArmIdRegister::Zfr0).availability(),
            ArmRegisterAvailability::MacOs15_2
        );
        assert_eq!(
            ArmRegister64::Id(ArmIdRegister::Smfr0).availability(),
            ArmRegisterAvailability::MacOs15_2
        );
        assert_eq!(
            ArmRegister64::Actlr.availability(),
            ArmRegisterAvailability::MacOs15_0
        );
        for register in [
            ArmIdRegister::Pfr0,
            ArmIdRegister::Pfr1,
            ArmIdRegister::Dfr0,
            ArmIdRegister::Dfr1,
            ArmIdRegister::Isar0,
            ArmIdRegister::Isar1,
            ArmIdRegister::Mmfr0,
            ArmIdRegister::Mmfr1,
            ArmIdRegister::Mmfr2,
        ] {
            assert_eq!(
                ArmRegister64::Id(register).availability(),
                ArmRegisterAvailability::Baseline
            );
        }

        for (index, left) in ARM64_SYSTEM_REGISTER_PROFILE.iter().enumerate() {
            for right in &ARM64_SYSTEM_REGISTER_PROFILE[index + 1..] {
                assert_ne!(left.identity, right.identity);
                assert_ne!(left.register, right.register);
            }
        }
    }

    #[test]
    fn actlr_accepts_only_the_public_entso_filter() {
        for (filter, value) in [(0, 0), (ARM64_ACTLR_ENTSO_MASK, 0), (2, 2)] {
            assert_eq!(
                u64_modifier_parts(
                    executable_modifier(
                        KVM_REG_ARM64_ACTLR_EL1,
                        CpuConfigArmRegisterWidth::U64,
                        filter.into(),
                        value.into(),
                    )
                    .expect("public ACTLR.EnTSO operation should execute")
                ),
                (ArmRegister64::Actlr, filter, value)
            );
        }

        for bit in 0..u64::BITS {
            if bit == 1 {
                continue;
            }
            let filter = 1_u64 << bit;
            assert_eq!(
                executable_modifier(
                    KVM_REG_ARM64_ACTLR_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    filter.into(),
                    0,
                ),
                Err(CpuConfigError::ActlrFilterUnsupported)
            );
        }
        assert_eq!(
            executable_modifier(
                KVM_REG_ARM64_ACTLR_EL1,
                CpuConfigArmRegisterWidth::U64,
                u128::from(u64::MAX),
                ARM64_ACTLR_ENTSO_MASK.into(),
            ),
            Err(CpuConfigError::ActlrFilterUnsupported)
        );
        assert_eq!(
            executable_modifier(
                KVM_REG_ARM64_ACTLR_EL1,
                CpuConfigArmRegisterWidth::U64,
                0,
                ARM64_ACTLR_ENTSO_MASK.into(),
            ),
            Err(CpuConfigError::ValueOutsideFilter {
                collection: CpuConfigCollection::RegisterModifiers,
            })
        );
    }

    #[test]
    fn terminally_classifies_kvm_classes_and_public_hvf_system_families() {
        for (id, width, error) in [
            (
                0x6020_0000_0011_0000,
                CpuConfigArmRegisterWidth::U32,
                CpuConfigError::KvmDemuxRegisterUnsupported,
            ),
            (
                0x6030_0000_0014_0000,
                CpuConfigArmRegisterWidth::U64,
                CpuConfigError::KvmFirmwareRegisterUnsupported,
            ),
            (
                0x6030_0000_0015_0000,
                CpuConfigArmRegisterWidth::U64,
                CpuConfigError::KvmSveRegisterUnsupported,
            ),
            (
                0x6030_0000_0016_0000,
                CpuConfigArmRegisterWidth::U64,
                CpuConfigError::KvmFirmwareFeatureRegisterUnsupported,
            ),
            (
                0x6030_0000_0012_0000,
                CpuConfigArmRegisterWidth::U64,
                CpuConfigError::UnknownKvmRegisterClass,
            ),
            (
                0x6030_0001_0013_c020,
                CpuConfigArmRegisterWidth::U64,
                CpuConfigError::InvalidRegisterIdentity,
            ),
        ] {
            assert_eq!(executable_modifier(id, width, 0, 0), Err(error));
        }

        let public_families: &[(&[u16], CpuConfigError)] = &[
            (
                &[0xc000, 0xc005],
                CpuConfigError::TopologyRegisterUnsupported,
            ),
            (
                &[0xc200, 0xc201, 0xc208, 0xe208],
                CpuConfigError::RegisterAliasUnsupported,
            ),
            (
                &[
                    0xc080, 0xc082, 0xc100, 0xc101, 0xc102, 0xc288, 0xc289, 0xc290, 0xc300, 0xc3a0,
                    0xc510, 0xc518, 0xc600, 0xc681, 0xc684, 0xc687, 0xde82, 0xde83, 0xde87,
                ],
                CpuConfigError::LifecycleRegisterUnsupported,
            ),
            (
                &[
                    0xc108, 0xc109, 0xc10a, 0xc10b, 0xc110, 0xc111, 0xc112, 0xc113, 0xc118, 0xc119,
                ],
                CpuConfigError::SecuritySensitiveRegisterUnsupported,
            ),
            (
                &[
                    0xc708, 0xdf00, 0xdf01, 0xdf02, 0xdf10, 0xdf11, 0xdf12, 0xdf18, 0xdf19, 0xdf1a,
                ],
                CpuConfigError::TimeDependentRegisterUnsupported,
            ),
            (
                &[
                    0xd000, 0xc230, 0xc643, 0xc644, 0xc645, 0xc646, 0xc647, 0xc648, 0xc649, 0xc64a,
                    0xc64b, 0xc663, 0xc664, 0xc665, 0xc666, 0xc667,
                ],
                CpuConfigError::SeparatelyOwnedRegisterUnsupported,
            ),
            (
                &[0xc094, 0xc096, 0xda12, 0xde85],
                CpuConfigError::MutableSmeRegisterUnsupported,
            ),
            (
                &[
                    0xe000, 0xe005, 0xe080, 0xe088, 0xe089, 0xe08a, 0xe100, 0xe101, 0xe102, 0xe108,
                    0xe10a, 0xe200, 0xe201, 0xe290, 0xe300, 0xe304, 0xe510, 0xe600, 0xe682, 0xe703,
                    0xe708, 0xe710, 0xe711, 0xe712, 0xf208,
                ],
                CpuConfigError::DisabledEl2RegisterUnsupported,
            ),
            (&[0xc0ff], CpuConfigError::UnnamedSystemRegisterUnsupported),
        ];
        for (registers, expected) in public_families {
            for register in *registers {
                assert_eq!(
                    executable_modifier(
                        system_register(*register),
                        CpuConfigArmRegisterWidth::U64,
                        0,
                        0,
                    ),
                    Err(*expected)
                );
            }
        }

        for index in 0_u16..16 {
            for field in 4_u16..=7 {
                assert_eq!(
                    executable_modifier(
                        system_register(0x8000 + index * 8 + field),
                        CpuConfigArmRegisterWidth::U64,
                        0,
                        0,
                    ),
                    Err(CpuConfigError::SecuritySensitiveRegisterUnsupported)
                );
            }
        }
        for register in [0x8010, 0x8012] {
            assert_eq!(
                executable_modifier(
                    system_register(register),
                    CpuConfigArmRegisterWidth::U64,
                    0,
                    0,
                ),
                Err(CpuConfigError::SecuritySensitiveRegisterUnsupported)
            );
        }
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
            Err(CpuConfigError::InvalidRegisterWidth)
        );
        assert_eq!(
            CpuConfigInput::new(
                Vec::new(),
                vec![modifier(
                    0x6030_0000_0010_0001,
                    CpuConfigArmRegisterWidth::U64,
                    1,
                    1,
                )],
                Vec::new(),
            )
            .into_custom_template(),
            Err(CpuConfigError::InvalidRegisterIdentity)
        );
    }

    #[test]
    fn classifies_every_reviewed_core_register_at_its_exact_width() {
        for index in 0..=30 {
            let id = kvm_reg_arm64_core_x(index).expect("X0-X30 should have KVM identities");
            let result = executable_modifier(id, CpuConfigArmRegisterWidth::U64, 1, 1);
            if (1..=3).contains(&index) {
                assert_eq!(result, Err(CpuConfigError::BootReservedRegister));
            } else {
                assert_eq!(
                    u64_modifier_parts(result.expect("reviewed X register should execute")).0,
                    arm_x(index)
                );
            }
        }
        assert_eq!(kvm_reg_arm64_core_x(31), None);

        for (id, register) in [
            (KVM_REG_ARM64_CORE_SP_EL0, ArmRegister64::SpEl0),
            (KVM_REG_ARM64_CORE_PC, ArmRegister64::Pc),
            (KVM_REG_ARM64_CORE_PSTATE, ArmRegister64::Pstate),
            (KVM_REG_ARM64_CORE_SP_EL1, ArmRegister64::SpEl1),
            (KVM_REG_ARM64_CORE_ELR_EL1, ArmRegister64::ElrEl1),
            (KVM_REG_ARM64_CORE_SPSR_EL1, ArmRegister64::SpsrEl1),
        ] {
            assert_eq!(
                u64_modifier_parts(
                    executable_modifier(id, CpuConfigArmRegisterWidth::U64, 1, 1)
                        .expect("reviewed core register should execute")
                )
                .0,
                register
            );
        }
        assert_eq!(
            arm_x(0).boot_disposition(),
            ArmRegisterBootDisposition::AppliedThenBootOverridden
        );
        assert_eq!(
            ArmRegister64::Pc.boot_disposition(),
            ArmRegisterBootDisposition::AppliedThenBootOverridden
        );
        assert_eq!(
            ArmRegister64::Pstate.boot_disposition(),
            ArmRegisterBootDisposition::AppliedThenBootOverridden
        );
        assert_eq!(
            arm_x(4).boot_disposition(),
            ArmRegisterBootDisposition::Retained
        );

        for index in 0..=31 {
            let id = kvm_reg_arm64_core_q(index).expect("Q0-Q31 should have KVM identities");
            assert_eq!(
                u128_modifier_parts(
                    executable_modifier(id, CpuConfigArmRegisterWidth::U128, u128::MAX, 1 << 127)
                        .expect("reviewed Q register should execute")
                ),
                (arm_q(index), u128::MAX, 1 << 127)
            );
        }
        assert_eq!(kvm_reg_arm64_core_q(32), None);

        assert_eq!(
            u32_modifier_parts(
                executable_modifier(
                    KVM_REG_ARM64_CORE_FPCR,
                    CpuConfigArmRegisterWidth::U32,
                    u32::MAX.into(),
                    1,
                )
                .expect("FPCR should execute")
            ),
            (ArmRegister32::Fpcr, u32::MAX, 1)
        );
        assert_eq!(
            u32_modifier_parts(
                executable_modifier(
                    KVM_REG_ARM64_CORE_FPSR,
                    CpuConfigArmRegisterWidth::U32,
                    u32::MAX.into(),
                    1 << 31,
                )
                .expect("FPSR should execute")
            ),
            (ArmRegister32::Fpsr, u32::MAX, 1 << 31)
        );
    }

    #[test]
    fn exhausts_core_layout_rejections_wrong_widths_and_system_aliases() {
        for index in 0..=213 {
            let u32_result = executable_modifier(
                KVM_REG_ARM64_CORE_U32_BASE | index,
                CpuConfigArmRegisterWidth::U32,
                1,
                1,
            );
            match expected_core_width(index) {
                Some(CpuConfigArmRegisterWidth::U32) => {
                    assert!(u32_result.is_ok(), "reviewed U32 core index should map");
                }
                Some(_) => assert_eq!(u32_result, Err(CpuConfigError::InvalidRegisterWidth)),
                None => assert_eq!(u32_result, Err(CpuConfigError::InvalidRegisterIdentity)),
            }

            let expected_u64_error = match index {
                0..=60 if index.is_multiple_of(2) => {
                    if matches!(index, 2 | 4 | 6) {
                        Some(CpuConfigError::BootReservedRegister)
                    } else {
                        None
                    }
                }
                62 | 64 | 66 | 68 | 70 | 72 => None,
                74 | 76 | 78 | 80 => Some(CpuConfigError::Aarch32BankedRegisterUnavailable),
                _ if expected_core_width(index).is_some() => {
                    Some(CpuConfigError::InvalidRegisterWidth)
                }
                _ => Some(CpuConfigError::InvalidRegisterIdentity),
            };
            let u64_result = executable_modifier(
                KVM_REG_ARM64_CORE_U64_BASE | index,
                CpuConfigArmRegisterWidth::U64,
                1,
                1,
            );
            if let Some(error) = expected_u64_error {
                assert_eq!(u64_result, Err(error));
            } else {
                assert!(u64_result.is_ok(), "reviewed U64 core index should map");
            }

            let u128_result = executable_modifier(
                KVM_REG_ARM64_CORE_U128_BASE | index,
                CpuConfigArmRegisterWidth::U128,
                1,
                1,
            );
            match expected_core_width(index) {
                Some(CpuConfigArmRegisterWidth::U128) => {
                    assert!(u128_result.is_ok(), "reviewed U128 core index should map");
                }
                Some(_) => assert_eq!(u128_result, Err(CpuConfigError::InvalidRegisterWidth)),
                None => assert_eq!(u128_result, Err(CpuConfigError::InvalidRegisterIdentity)),
            }
        }

        for (id, width) in [
            (
                KVM_REG_ARM64_CORE_U32_BASE | 214,
                CpuConfigArmRegisterWidth::U32,
            ),
            (
                KVM_REG_ARM64_CORE_U64_BASE | 214,
                CpuConfigArmRegisterWidth::U64,
            ),
            (
                KVM_REG_ARM64_CORE_U128_BASE | 214,
                CpuConfigArmRegisterWidth::U128,
            ),
        ] {
            assert_eq!(
                executable_modifier(id, width, 1, 1),
                Err(CpuConfigError::InvalidRegisterIdentity)
            );
        }
        for (id, width, error) in [
            (
                0x6020_0000_0011_00d5,
                CpuConfigArmRegisterWidth::U32,
                CpuConfigError::KvmDemuxRegisterUnsupported,
            ),
            (
                0x6020_0000_0011_0100,
                CpuConfigArmRegisterWidth::U32,
                CpuConfigError::InvalidRegisterIdentity,
            ),
            (
                0x6030_0000_0011_0008,
                CpuConfigArmRegisterWidth::U64,
                CpuConfigError::InvalidRegisterWidth,
            ),
            (
                0x6040_0000_0011_0054,
                CpuConfigArmRegisterWidth::U128,
                CpuConfigError::InvalidRegisterWidth,
            ),
        ] {
            assert_eq!(executable_modifier(id, width, 1, 1), Err(error));
        }

        // Architectural system encodings of the four accepted core-system
        // fields must not create a second route to the same HVF target.
        for alias in [
            0x6030_0000_0013_c208, // SP_EL0
            0x6030_0000_0013_e208, // SP_EL1
            0x6030_0000_0013_c201, // ELR_EL1
            0x6030_0000_0013_c200, // SPSR_EL1
        ] {
            assert_eq!(
                executable_modifier(alias, CpuConfigArmRegisterWidth::U64, 1, 1),
                Err(CpuConfigError::RegisterAliasUnsupported)
            );
        }

        let mut accepted = vec![
            (KVM_REG_ARM64_CORE_FPCR, CpuConfigArmRegisterWidth::U32),
            (KVM_REG_ARM64_CORE_FPSR, CpuConfigArmRegisterWidth::U32),
            (KVM_REG_ARM64_CORE_SP_EL0, CpuConfigArmRegisterWidth::U64),
            (KVM_REG_ARM64_CORE_PC, CpuConfigArmRegisterWidth::U64),
            (KVM_REG_ARM64_CORE_PSTATE, CpuConfigArmRegisterWidth::U64),
            (KVM_REG_ARM64_CORE_SP_EL1, CpuConfigArmRegisterWidth::U64),
            (KVM_REG_ARM64_CORE_ELR_EL1, CpuConfigArmRegisterWidth::U64),
            (KVM_REG_ARM64_CORE_SPSR_EL1, CpuConfigArmRegisterWidth::U64),
        ];
        accepted.extend([0_u8].into_iter().chain(4..=30).map(|index| {
            (
                kvm_reg_arm64_core_x(index).expect("reviewed X index should map"),
                CpuConfigArmRegisterWidth::U64,
            )
        }));
        accepted.extend((0..=31).map(|index| {
            (
                kvm_reg_arm64_core_q(index).expect("reviewed Q index should map"),
                CpuConfigArmRegisterWidth::U128,
            )
        }));
        accepted.extend(
            ARM64_SYSTEM_REGISTER_PROFILE
                .iter()
                .map(|entry| (entry.identity, CpuConfigArmRegisterWidth::U64)),
        );
        for (id, correct_width) in accepted {
            for wrong_width in [
                CpuConfigArmRegisterWidth::U32,
                CpuConfigArmRegisterWidth::U64,
                CpuConfigArmRegisterWidth::U128,
            ] {
                if wrong_width != correct_width {
                    assert_eq!(
                        executable_modifier(id, wrong_width, 1, 1),
                        Err(CpuConfigError::InvalidRegisterWidth)
                    );
                }
            }
        }
    }

    #[test]
    fn preserves_mixed_width_order_and_exact_boundary_values() {
        let template = CpuConfigInput::new(
            Vec::new(),
            vec![
                modifier(
                    KVM_REG_ARM64_CORE_FPCR,
                    CpuConfigArmRegisterWidth::U32,
                    u32::MAX.into(),
                    0x8000_0001,
                ),
                modifier(
                    kvm_reg_arm64_core_x(4).expect("X4 should map"),
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    0x8000_0000_0000_0001,
                ),
                modifier(
                    kvm_reg_arm64_core_q(31).expect("Q31 should map"),
                    CpuConfigArmRegisterWidth::U128,
                    u128::MAX,
                    (1 << 127) | 1,
                ),
                modifier(
                    KVM_REG_ARM64_ID_AA64PFR0_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    1,
                    1,
                ),
            ],
            Vec::new(),
        )
        .into_custom_template()
        .expect("mixed-width template should validate")
        .expect("mixed-width template should be retained");

        assert_eq!(
            template
                .modifiers()
                .iter()
                .copied()
                .map(ArmRegisterModifier::width)
                .collect::<Vec<_>>(),
            [
                CpuConfigArmRegisterWidth::U32,
                CpuConfigArmRegisterWidth::U64,
                CpuConfigArmRegisterWidth::U128,
                CpuConfigArmRegisterWidth::U64,
            ]
        );
        assert_eq!(
            u32_modifier_parts(template.modifiers()[0]),
            (ArmRegister32::Fpcr, u32::MAX, 0x8000_0001)
        );
        assert_eq!(
            u64_modifier_parts(template.modifiers()[1]),
            (arm_x(4), u64::MAX, 0x8000_0000_0000_0001)
        );
        assert_eq!(
            u128_modifier_parts(template.modifiers()[2]),
            (arm_q(31), u128::MAX, (1 << 127) | 1)
        );

        assert_eq!(
            executable_modifier(
                KVM_REG_ARM64_CORE_FPCR,
                CpuConfigArmRegisterWidth::U32,
                1_u128 << 32,
                0,
            ),
            Err(CpuConfigError::ValueOutsideRegisterWidth)
        );
    }

    #[test]
    fn executable_duplicate_guard_compares_semantic_targets_only() {
        let x4_a = ArmRegisterModifier::U64 {
            register: arm_x(4),
            filter: 1,
            value: 1,
        };
        let x4_b = ArmRegisterModifier::U64 {
            register: arm_x(4),
            filter: 2,
            value: 2,
        };
        let x5 = ArmRegisterModifier::U64 {
            register: arm_x(5),
            filter: 1,
            value: 1,
        };
        let fpcr = ArmRegisterModifier::U32 {
            register: ArmRegister32::Fpcr,
            filter: 1,
            value: 1,
        };
        let q4 = ArmRegisterModifier::U128 {
            register: arm_q(4),
            filter: 1,
            value: 1,
        };

        assert!(x4_a.has_same_target(x4_b));
        assert!(!x4_a.has_same_target(x5));
        assert!(!x4_a.has_same_target(fpcr));
        assert!(!x4_a.has_same_target(q4));
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

        let mismatched_width = CpuConfigInput::new(
            Vec::new(),
            vec![modifier(
                KVM_REG_ARM64_ID_AA64PFR0_EL1,
                CpuConfigArmRegisterWidth::U32,
                1,
                1,
            )],
            Vec::new(),
        );
        assert_eq!(
            mismatched_width.into_custom_template(),
            Err(CpuConfigError::InvalidRegisterWidth)
        );

        let invalid_width_encoding = CpuConfigInput::new(
            Vec::new(),
            vec![modifier(
                0x6010_0000_0013_c020,
                CpuConfigArmRegisterWidth::U64,
                1,
                1,
            )],
            Vec::new(),
        );
        assert_eq!(
            invalid_width_encoding.into_custom_template(),
            Err(CpuConfigError::InvalidRegisterWidth)
        );
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
