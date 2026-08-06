//! CPU-inspection projection over strict API requests.

use std::fmt;

use bangbang_api::config::parse_config_document;
use bangbang_api::http::{
    ApiRequest, CpuConfigArmRegisterWidth as ApiArmRegisterWidth,
    CpuConfigKvmCapability as ApiKvmCapability, CpuConfigRequest, MachineConfigCpuTemplate,
    MachineConfigHugePages, MachineConfigRequest,
};
use bangbang_runtime::cpu::{
    CpuConfigArmRegisterModifier, CpuConfigArmRegisterWidth, CpuConfigInput,
    CpuConfigKvmCapability, CpuConfigVcpuFeature, CustomCpuTemplate,
};
use bangbang_runtime::machine::{
    MachineConfig, MachineConfigCpuTemplate as RuntimeCpuTemplate,
    MachineConfigHugePages as RuntimeHugePages, MachineConfigInput,
};
use bangbang_runtime::{VmmAction, VmmController};

use crate::document::{CpuTemplateDocumentError, decode_cpu_template_document};

const HELPER_INSTANCE_ID: &str = "cpu-template-helper";
const HELPER_APP_NAME: &str = "cpu-template-helper";

/// Failure while preparing one backend-neutral CPU inspection request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectionPreparationError {
    ConfigDocument,
    TemplateDocument(CpuTemplateDocumentError),
    Configuration,
}

impl fmt::Display for InspectionPreparationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ConfigDocument => "invalid helper configuration document",
            Self::TemplateDocument(_) => "invalid helper CPU-template document",
            Self::Configuration => "CPU inspection configuration could not be applied",
        })
    }
}

impl std::error::Error for InspectionPreparationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::TemplateDocument(source) => Some(source),
            Self::ConfigDocument | Self::Configuration => None,
        }
    }
}

/// Fully validated machine and CPU-template selection for an effective
/// provider.
#[derive(Clone, PartialEq, Eq)]
pub struct PreparedCpuTemplateInspection {
    machine_config: MachineConfig,
    custom_template: Option<CustomCpuTemplate>,
}

impl PreparedCpuTemplateInspection {
    /// Return the validated machine configuration.
    pub const fn machine_config(&self) -> MachineConfig {
        self.machine_config
    }

    /// Return the final custom template after config and explicit precedence.
    pub const fn custom_template(&self) -> Option<&CustomCpuTemplate> {
        self.custom_template.as_ref()
    }
}

impl fmt::Debug for PreparedCpuTemplateInspection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedCpuTemplateInspection")
            .field("vcpu_count", &self.machine_config.vcpu_count())
            .field("static_template", &self.machine_config.cpu_template())
            .field(
                "custom_modifier_count",
                &self
                    .custom_template
                    .as_ref()
                    .map_or(0, |template| template.modifiers().len()),
            )
            .finish()
    }
}

/// Parse and apply the CPU-relevant projection of an optional complete config
/// document, then apply an optional explicit custom template last.
pub fn prepare_inspection_request(
    config_document: Option<&str>,
    explicit_template_document: Option<&str>,
) -> Result<PreparedCpuTemplateInspection, InspectionPreparationError> {
    let mut controller = VmmController::new(
        HELPER_INSTANCE_ID,
        env!("CARGO_PKG_VERSION"),
        HELPER_APP_NAME,
    );

    if let Some(contents) = config_document {
        let requests = parse_config_document(contents)
            .map_err(|_| InspectionPreparationError::ConfigDocument)?;
        for request in requests {
            if let Some(action) = inspection_action_from_api_request(request.request()) {
                controller
                    .handle_action(action)
                    .map_err(|_| InspectionPreparationError::Configuration)?;
            }
        }
    }

    if let Some(contents) = explicit_template_document {
        let document = decode_cpu_template_document(contents)
            .map_err(InspectionPreparationError::TemplateDocument)?;
        controller
            .handle_action(VmmAction::PutCpuConfig(document.to_runtime_input()))
            .map_err(|_| InspectionPreparationError::Configuration)?;
    }

    Ok(PreparedCpuTemplateInspection {
        machine_config: controller.machine_config(),
        custom_template: controller.custom_cpu_template().cloned(),
    })
}

/// Project a strict API request when it affects CPU inspection state.
pub fn inspection_action_from_api_request(request: &ApiRequest) -> Option<VmmAction> {
    match request {
        ApiRequest::PutMachineConfig(config) => Some(VmmAction::PutMachineConfig(
            machine_config_input_from_request(config),
        )),
        ApiRequest::PutCpuConfig(config) => Some(VmmAction::PutCpuConfig(
            cpu_config_input_from_request(config),
        )),
        _ => None,
    }
}

/// Convert the strict transport CPU request into the backend-neutral input.
pub fn cpu_config_input_from_request(config: &CpuConfigRequest) -> CpuConfigInput {
    let kvm_capabilities = config
        .kvm_capabilities()
        .iter()
        .copied()
        .map(|capability| match capability {
            ApiKvmCapability::Add(value) => CpuConfigKvmCapability::Add(value),
            ApiKvmCapability::Remove(value) => CpuConfigKvmCapability::Remove(value),
        })
        .collect();
    let reg_modifiers = config
        .reg_modifiers()
        .iter()
        .copied()
        .map(|modifier| {
            CpuConfigArmRegisterModifier::new(
                modifier.id(),
                match modifier.width() {
                    ApiArmRegisterWidth::U32 => CpuConfigArmRegisterWidth::U32,
                    ApiArmRegisterWidth::U64 => CpuConfigArmRegisterWidth::U64,
                    ApiArmRegisterWidth::U128 => CpuConfigArmRegisterWidth::U128,
                },
                modifier.filter(),
                modifier.value(),
            )
        })
        .collect();
    let vcpu_features = config
        .vcpu_features()
        .iter()
        .copied()
        .map(|feature| {
            CpuConfigVcpuFeature::new(feature.index(), feature.filter(), feature.value())
        })
        .collect();

    CpuConfigInput::new(kvm_capabilities, reg_modifiers, vcpu_features)
}

/// Convert the strict transport machine request into the backend-neutral
/// input used by the production controller.
pub fn machine_config_input_from_request(config: &MachineConfigRequest) -> MachineConfigInput {
    let mut input = MachineConfigInput::new(config.vcpu_count(), config.mem_size_mib())
        .with_smt(config.smt())
        .with_track_dirty_pages(config.track_dirty_pages())
        .with_huge_pages(match config.huge_pages() {
            MachineConfigHugePages::None => RuntimeHugePages::None,
            MachineConfigHugePages::TwoM => RuntimeHugePages::TwoM,
        });

    if let Some(cpu_template) = config.cpu_template() {
        input = input.with_cpu_template(machine_cpu_template_from_request(cpu_template));
    }

    input
}

const fn machine_cpu_template_from_request(
    cpu_template: MachineConfigCpuTemplate,
) -> RuntimeCpuTemplate {
    match cpu_template {
        MachineConfigCpuTemplate::C3 => RuntimeCpuTemplate::C3,
        MachineConfigCpuTemplate::T2 => RuntimeCpuTemplate::T2,
        MachineConfigCpuTemplate::T2S => RuntimeCpuTemplate::T2S,
        MachineConfigCpuTemplate::T2CL => RuntimeCpuTemplate::T2CL,
        MachineConfigCpuTemplate::T2A => RuntimeCpuTemplate::T2A,
        MachineConfigCpuTemplate::V1N1 => RuntimeCpuTemplate::V1N1,
        MachineConfigCpuTemplate::None => RuntimeCpuTemplate::None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use bangbang_runtime::cpu::{
        ArmCpuTemplateRegister, ArmRegister64, KVM_REG_ARM64_CORE_PC, KVM_REG_ARM64_CORE_SP_EL0,
    };

    use super::*;

    #[test]
    fn explicit_template_replaces_config_selection_after_valid_projection() {
        let config = format!(
            r#"{{
                "machine-config": {{
                    "vcpu_count": 2,
                    "mem_size_mib": 256,
                    "cpu_template": "V1N1"
                }},
                "boot-source": {{"kernel_image_path":"/missing/private-kernel"}},
                "drives": [{{
                    "drive_id":"root",
                    "path_on_host":"/missing/private-root",
                    "is_root_device":true,
                    "is_read_only":true
                }}],
                "cpu-config": {{"reg_modifiers":[{{
                    "addr":"0x{KVM_REG_ARM64_CORE_SP_EL0:016x}",
                    "bitmap":"0b1"
                }}]}}
            }}"#
        );
        let explicit = format!(
            r#"{{"reg_modifiers":[{{
                "addr":"0x{KVM_REG_ARM64_CORE_PC:016x}",
                "bitmap":"0b1"
            }}]}}"#
        );
        let prepared = prepare_inspection_request(Some(&config), Some(&explicit))
            .expect("CPU-only projection must not open unrelated resource paths");

        assert_eq!(prepared.machine_config().vcpu_count(), 2);
        assert_eq!(prepared.machine_config().cpu_template(), None);
        let modifiers = prepared
            .custom_template()
            .expect("explicit custom template should win")
            .modifiers();
        assert_eq!(modifiers.len(), 1);
        assert_eq!(
            modifiers[0].register(),
            ArmCpuTemplateRegister::U64(ArmRegister64::Pc)
        );
    }

    #[test]
    fn invalid_earlier_machine_config_fails_before_explicit_override() {
        let config = r#"{
            "machine-config":{"vcpu_count":0,"mem_size_mib":128},
            "boot-source":{"kernel_image_path":"kernel"}
        }"#;
        let explicit = format!(
            r#"{{"reg_modifiers":[{{
                "addr":"0x{KVM_REG_ARM64_CORE_PC:016x}",
                "bitmap":"0b1"
            }}]}}"#
        );
        assert_eq!(
            prepare_inspection_request(Some(config), Some(&explicit)),
            Err(InspectionPreparationError::Configuration)
        );
    }

    #[test]
    fn errors_and_debug_output_are_value_and_path_redacted() {
        let error = prepare_inspection_request(
            Some(r#"{"private-unknown-section":{},"boot-source":{}}"#),
            None,
        )
        .expect_err("unknown config section should fail");
        assert_eq!(error, InspectionPreparationError::ConfigDocument);
        assert!(!error.to_string().contains("private-unknown-section"));

        let prepared = prepare_inspection_request(None, None).expect("default should prepare");
        let debug = format!("{prepared:?}");
        assert!(!debug.contains("kernel"));
        assert!(!debug.contains("0x"));
    }
}
