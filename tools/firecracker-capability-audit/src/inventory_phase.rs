use std::collections::BTreeSet;

use crate::{CapabilityInventory, Disposition};

const WAVE7_SUCCESSOR_IDS: [&str; 5] = [
    "corpus:design",
    "corpus:device-api",
    "corpus:release-changelog",
    "semantic.tools:packaging-help-errors-and-applicable-operations",
    "semantic.transport:virtio-mmio-activation",
];

pub(crate) const WAVE8_SUCCESSOR_ID: &str =
    "semantic.cross-capability:state-errors-metrics-security-and-snapshots";

pub(crate) const JAILER_UID_GID_IDS: [&str; 2] =
    ["tool-argument:jailer/gid", "tool-argument:jailer/uid"];

const WAVE8_AUDIT_IDS: [&str; 8] = [
    "corpus:jailer",
    "corpus:network-setup",
    "corpus:production-host",
    "semantic.network:virtio-net-vmnet-policy-and-connectivity",
    "tool-argument:jailer/chroot-base-dir",
    "tool-argument:jailer/gid",
    "tool-argument:jailer/uid",
    "tool-operation:jailer/run",
];

const MISSING_PLATFORM_FEASIBLE_IDS: [&str; 3] = [
    "semantic.isolation:host-resource-authority-and-brokerage",
    "semantic.isolation:jailer-seccomp-and-macos-containment-outcomes",
    "semantic.isolation:multiprocess-concurrency-redaction-and-failure-atomicity",
];

pub(crate) const WAVE8_X86_CPUID_MSR_IDS: [&str; 13] = [
    "api-property:CpuConfig.cpuid_modifiers",
    "api-property:CpuConfig.msr_modifiers",
    "api-property:CpuidLeafModifier.flags",
    "api-property:CpuidLeafModifier.leaf",
    "api-property:CpuidLeafModifier.modifiers",
    "api-property:CpuidLeafModifier.subleaf",
    "api-property:CpuidRegisterModifier.bitmap",
    "api-property:CpuidRegisterModifier.register",
    "api-property:MsrModifier.addr",
    "api-property:MsrModifier.bitmap",
    "api-schema:CpuidLeafModifier",
    "api-schema:CpuidRegisterModifier",
    "api-schema:MsrModifier",
];

pub(crate) const WAVE8_ARM_KVM_TEMPLATE_IDS: [&str; 7] = [
    "api-property:CpuConfig.kvm_capabilities",
    "api-property:CpuConfig.vcpu_features",
    "api-property:MachineConfiguration.cpu_template",
    "api-property:VcpuFeatures.bitmap",
    "api-property:VcpuFeatures.index",
    "api-schema:CpuTemplate",
    "api-schema:VcpuFeatures",
];

pub(crate) const WAVE8_HUGETLBFS_IDS: [&str; 2] = [
    "api-property:MachineConfiguration.huge_pages",
    "corpus:hugepages",
];

pub(crate) const WAVE8_LINUX_ISOLATION_IDS: [&str; 8] = [
    "corpus:seccomp",
    "firecracker-argument:no-seccomp",
    "firecracker-argument:seccomp-filter",
    "tool-argument:jailer/cgroup",
    "tool-argument:jailer/cgroup-version",
    "tool-argument:jailer/netns",
    "tool-argument:jailer/new-pid-ns",
    "tool-argument:jailer/parent-cgroup",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InventoryPhase {
    SpecificationBenchmark,
    Wave7,
    Wave8,
    JailerUidGidPlatformLimit,
}

impl InventoryPhase {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::SpecificationBenchmark => "specification-benchmark 371/14/3/30",
            Self::Wave7 => "Wave 7 376/9/3/30",
            Self::Wave8 => "Wave 8 377/8/3/30",
            Self::JailerUidGidPlatformLimit => "post-Wave-8 jailer uid/gid 377/6/3/32",
        }
    }
}

pub(crate) fn classify_inventory_phase(
    inventory: &CapabilityInventory,
) -> Result<InventoryPhase, String> {
    let counts = disposition_counts(inventory);
    let phase = match counts {
        (371, 14, 3, 30) => InventoryPhase::SpecificationBenchmark,
        (376, 9, 3, 30) => InventoryPhase::Wave7,
        (377, 8, 3, 30) => InventoryPhase::Wave8,
        (377, 6, 3, 32) => InventoryPhase::JailerUidGidPlatformLimit,
        (implemented, audit, feasible, impossible) => {
            return Err(format!(
                "inventory does not match an exact accepted phase: found {implemented}/{audit}/{feasible}/{impossible}"
            ));
        }
    };

    for (disposition, expected) in [
        (Disposition::AuditRequired, expected_audit_ids(phase)),
        (
            Disposition::MissingPlatformFeasible,
            expected_feasible_ids(),
        ),
        (
            Disposition::ProvenPlatformImpossible,
            expected_impossible_ids(phase),
        ),
    ] {
        let actual = disposition_ids(inventory, disposition);
        if actual != expected {
            return Err(format!(
                "{} has an inexact {disposition:?} identity partition: expected {expected:?}, found {actual:?}",
                phase.name()
            ));
        }
    }

    Ok(phase)
}

pub(crate) fn expected_disposition(phase: InventoryPhase, id: &str) -> Disposition {
    if expected_audit_ids(phase).contains(id) {
        Disposition::AuditRequired
    } else if expected_feasible_ids().contains(id) {
        Disposition::MissingPlatformFeasible
    } else if expected_impossible_ids(phase).contains(id) {
        Disposition::ProvenPlatformImpossible
    } else {
        Disposition::ImplementedAndVerified
    }
}

pub(crate) fn expected_nonterminal_ids(phase: InventoryPhase) -> BTreeSet<&'static str> {
    expected_audit_ids(phase)
        .into_iter()
        .chain(expected_feasible_ids())
        .collect()
}

pub(crate) fn expected_impossible_ids(phase: InventoryPhase) -> BTreeSet<&'static str> {
    let mut ids = wave8_historical_impossible_ids();
    if phase == InventoryPhase::JailerUidGidPlatformLimit {
        ids.extend(JAILER_UID_GID_IDS);
    }
    ids
}

pub(crate) fn wave8_historical_impossible_ids() -> BTreeSet<&'static str> {
    WAVE8_X86_CPUID_MSR_IDS
        .into_iter()
        .chain(WAVE8_ARM_KVM_TEMPLATE_IDS)
        .chain(WAVE8_HUGETLBFS_IDS)
        .chain(WAVE8_LINUX_ISOLATION_IDS)
        .collect()
}

pub(crate) fn disposition_counts(inventory: &CapabilityInventory) -> (usize, usize, usize, usize) {
    inventory.capabilities.iter().fold(
        (0, 0, 0, 0),
        |(implemented, audit, feasible, impossible), capability| match capability.disposition {
            Disposition::ImplementedAndVerified => (implemented + 1, audit, feasible, impossible),
            Disposition::AuditRequired => (implemented, audit + 1, feasible, impossible),
            Disposition::MissingPlatformFeasible => (implemented, audit, feasible + 1, impossible),
            Disposition::ProvenPlatformImpossible => (implemented, audit, feasible, impossible + 1),
        },
    )
}

fn expected_audit_ids(phase: InventoryPhase) -> BTreeSet<&'static str> {
    let mut ids = WAVE8_AUDIT_IDS.into_iter().collect::<BTreeSet<_>>();
    match phase {
        InventoryPhase::SpecificationBenchmark => {
            ids.extend(WAVE7_SUCCESSOR_IDS);
            ids.insert(WAVE8_SUCCESSOR_ID);
        }
        InventoryPhase::Wave7 => {
            ids.insert(WAVE8_SUCCESSOR_ID);
        }
        InventoryPhase::Wave8 => {}
        InventoryPhase::JailerUidGidPlatformLimit => {
            for id in JAILER_UID_GID_IDS {
                ids.remove(id);
            }
        }
    }
    ids
}

fn expected_feasible_ids() -> BTreeSet<&'static str> {
    MISSING_PLATFORM_FEASIBLE_IDS.into_iter().collect()
}

fn disposition_ids(inventory: &CapabilityInventory, disposition: Disposition) -> BTreeSet<&str> {
    inventory
        .capabilities
        .iter()
        .filter(|capability| capability.disposition == disposition)
        .map(|capability| capability.id.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::{CAPABILITY_INVENTORY_PATH, read_capability_inventory};

    fn current_inventory() -> CapabilityInventory {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        read_capability_inventory(&root.join(CAPABILITY_INVENTORY_PATH))
            .expect("checked inventory must parse")
    }

    fn set_disposition(inventory: &mut CapabilityInventory, id: &str, disposition: Disposition) {
        inventory
            .capabilities
            .iter_mut()
            .find(|capability| capability.id == id)
            .expect("phase capability must exist")
            .disposition = disposition;
    }

    #[test]
    fn exact_inventory_phases_are_closed() {
        let current = current_inventory();
        assert_eq!(
            classify_inventory_phase(&current),
            Ok(InventoryPhase::JailerUidGidPlatformLimit)
        );

        let mut wave8 = current.clone();
        for id in JAILER_UID_GID_IDS {
            set_disposition(&mut wave8, id, Disposition::AuditRequired);
        }
        assert_eq!(classify_inventory_phase(&wave8), Ok(InventoryPhase::Wave8));

        let mut wave7 = wave8.clone();
        set_disposition(&mut wave7, WAVE8_SUCCESSOR_ID, Disposition::AuditRequired);
        assert_eq!(classify_inventory_phase(&wave7), Ok(InventoryPhase::Wave7));

        let mut specification = wave7;
        for id in WAVE7_SUCCESSOR_IDS {
            set_disposition(&mut specification, id, Disposition::AuditRequired);
        }
        assert_eq!(
            classify_inventory_phase(&specification),
            Ok(InventoryPhase::SpecificationBenchmark)
        );
    }

    #[test]
    fn equal_count_identity_swaps_do_not_classify() {
        let mut inventory = current_inventory();
        set_disposition(
            &mut inventory,
            "tool-argument:jailer/uid",
            Disposition::AuditRequired,
        );
        set_disposition(
            &mut inventory,
            "tool-argument:jailer/chroot-base-dir",
            Disposition::ProvenPlatformImpossible,
        );
        assert!(
            classify_inventory_phase(&inventory)
                .expect_err("equal-count wrong identities must fail")
                .contains("identity partition")
        );

        let mut terminal_swap = current_inventory();
        set_disposition(
            &mut terminal_swap,
            "tool-argument:jailer/uid",
            Disposition::ImplementedAndVerified,
        );
        set_disposition(
            &mut terminal_swap,
            "api-operation:GET /",
            Disposition::ProvenPlatformImpossible,
        );
        assert!(
            classify_inventory_phase(&terminal_swap)
                .expect_err("terminal equal-count swap must fail")
                .contains("identity partition")
        );

        let mut aggregate_swap = current_inventory();
        set_disposition(
            &mut aggregate_swap,
            "corpus:jailer",
            Disposition::ImplementedAndVerified,
        );
        set_disposition(
            &mut aggregate_swap,
            "api-operation:GET /",
            Disposition::AuditRequired,
        );
        assert!(
            classify_inventory_phase(&aggregate_swap)
                .expect_err("aggregate equal-count movement must fail")
                .contains("identity partition")
        );
    }

    #[test]
    fn partial_uid_gid_successors_do_not_classify() {
        for id in JAILER_UID_GID_IDS {
            let mut partial = current_inventory();
            set_disposition(&mut partial, id, Disposition::AuditRequired);
            assert!(
                classify_inventory_phase(&partial)
                    .expect_err("one terminal jailer identity alone must not classify")
                    .contains("inventory does not match an exact accepted phase"),
                "partial successor unexpectedly classified: {id}"
            );
        }
    }
}
