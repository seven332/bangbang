use serde::{Deserialize, Serialize};

use crate::{Baseline, Reference};

/// Delivery stage represented by the guest-workflow authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GuestWorkflowDeliveryState {
    Preparation,
}

/// Issues that own the two-slice guest-workflow delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestWorkflowDelivery {
    pub parent_issue: String,
    pub preparation_issue: String,
    pub completion_issue: String,
    pub state: GuestWorkflowDeliveryState,
}

/// Firecracker CI namespace used for the downloadable guest artifacts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestSourceNamespace {
    pub release: String,
    pub architecture: String,
    pub provider: String,
    pub provenance_url: String,
    pub redistribution: String,
}

/// Closed downloadable artifact kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GuestArtifactKind {
    LinuxKernel,
    SquashfsRootfs,
}

/// Closed output ownership and publication classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GuestOutputClass {
    VerifiedRepairableCache,
    DeterministicGeneratedCache,
    CallerOwnedAbsentOnly,
    UniqueEphemeralSession,
}

/// One exact downloadable guest artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestArtifact {
    pub id: String,
    pub kind: GuestArtifactKind,
    pub filename: String,
    pub url: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub cache_path: String,
    pub output_class: GuestOutputClass,
    pub provenance: String,
    pub redistribution: String,
}

/// Determinism class for a checked generated artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GeneratedDeterminism {
    ByteIdentical,
}

/// One generated artifact identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedGuestArtifact {
    pub id: String,
    pub generator_path: String,
    pub cache_path: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub output_class: GuestOutputClass,
    pub determinism: GeneratedDeterminism,
}

/// Ext4 output classification; bytes are deliberately not reproducible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Ext4Classification {
    RecipeDeterministic,
}

/// Sidecar contract used as the commit marker for an ext4 cache entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ext4SidecarPolicy {
    pub schema_version: u32,
    pub suffix: String,
    pub fields: Vec<String>,
    pub filesystem_check: String,
}

/// One bounded rootless ext4 preparation recipe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ext4Recipe {
    pub id: String,
    pub source_artifact: String,
    pub variant: String,
    pub filename_template: String,
    pub default_size: String,
    pub minimum_size_bytes: u64,
    pub classification: Ext4Classification,
    pub output_class: GuestOutputClass,
    pub tool_roles: Vec<String>,
    pub tracked_inputs: Vec<String>,
    pub sidecar: Ext4SidecarPolicy,
}

/// Exact operational semantics for an output class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestOutputPolicy {
    pub id: GuestOutputClass,
    pub reuse: String,
    pub repair: String,
    pub publication: String,
    pub collision: String,
    pub locking: String,
}

/// Planned workflow readiness in this delivery slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GuestWorkflowProfileState {
    Planned,
}

/// Closed public workflow modes reserved for the completion slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GuestWorkflowMode {
    Api,
    NoApi,
}

/// Guest-owned shutdown behavior required by the planned smoke.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GuestShutdown {
    GuestPoweroff,
}

/// Networking boundary of the planned workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GuestNetworking {
    None,
}

/// One exact planned public workflow profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestWorkflowProfile {
    pub id: String,
    pub state: GuestWorkflowProfileState,
    pub mode: GuestWorkflowMode,
    pub kernel_artifact: String,
    pub rootfs_artifact: String,
    pub initrd_artifact: String,
    pub boot_args: String,
    pub rootfs_read_only: bool,
    pub success_marker: String,
    pub shutdown: GuestShutdown,
    pub networking: GuestNetworking,
    pub platform: String,
    pub implementation: Vec<Reference>,
    pub validation: Vec<Reference>,
}

/// Categorized checked evidence for the preparation slice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestWorkflowEvidence {
    pub implementation: Vec<Reference>,
    pub validation: Vec<Reference>,
    pub documentation: Vec<Reference>,
}

/// Claims intentionally excluded from the guest-workflow authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GuestWorkflowNonclaim {
    ByteReproducibleExt4,
    HostileParentTraversalSafety,
    ArtifactRedistributionOrAuthentication,
    ArbitraryUrlOrProfileInput,
    ProductionWorkflow,
    ExternalGuestNetworking,
    ArbitraryDistroOrFreebsdGuestSupport,
    CrashAtomicImageSidecarPair,
}

/// Human-owned authority for guest artifacts and planned macOS workflows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestWorkflowAudit {
    pub schema_version: u32,
    pub baseline: Baseline,
    pub delivery: GuestWorkflowDelivery,
    pub source_namespace: GuestSourceNamespace,
    pub artifacts: Vec<GuestArtifact>,
    pub generated: Vec<GeneratedGuestArtifact>,
    pub ext4_recipes: Vec<Ext4Recipe>,
    pub output_classes: Vec<GuestOutputPolicy>,
    pub profiles: Vec<GuestWorkflowProfile>,
    pub evidence: GuestWorkflowEvidence,
    pub nonclaims: Vec<GuestWorkflowNonclaim>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_fields_and_open_enums() {
        let unknown = serde_json::from_str::<GuestWorkflowDelivery>(
            r##"{"parent_issue":"#1","preparation_issue":"#2","completion_issue":"#3","state":"preparation","final":true}"##,
        )
        .expect_err("unknown delivery fields must fail");
        assert!(unknown.to_string().contains("unknown field"));

        let state = serde_json::from_str::<GuestWorkflowProfileState>(r#""complete""#)
            .expect_err("terminal profile state must not parse in the preparation schema");
        assert!(state.to_string().contains("unknown variant"));
    }
}
