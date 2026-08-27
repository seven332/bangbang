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

pub(crate) const JAILER_CHROOT_BASE_DIR_ID: &str = "tool-argument:jailer/chroot-base-dir";

pub(crate) const JAILER_AGGREGATE_IDS: [&str; 2] = ["corpus:jailer", "tool-operation:jailer/run"];

pub(crate) const MULTIPROCESS_ISOLATION_ID: &str =
    "semantic.isolation:multiprocess-concurrency-redaction-and-failure-atomicity";

pub(crate) const HOST_RESOURCE_AUTHORITY_ID: &str =
    "semantic.isolation:host-resource-authority-and-brokerage";

pub(crate) const JAILER_SECCOMP_CONTAINMENT_ID: &str =
    "semantic.isolation:jailer-seccomp-and-macos-containment-outcomes";

pub(crate) const PRODUCTION_HOST_ID: &str = "corpus:production-host";

pub(crate) const NETWORK_VMNET_FEASIBLE_IDS: [&str; 2] = [
    "corpus:network-setup",
    "semantic.network:virtio-net-vmnet-policy-and-connectivity",
];

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
    JailerChrootPlatformLimit,
    JailerAggregate,
    MultiprocessIsolation,
    HostResourceAuthority,
    JailerSeccompContainment,
    ProductionHost,
    NetworkVmnetFeasibility,
}

impl InventoryPhase {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::SpecificationBenchmark => "specification-benchmark 371/14/3/30",
            Self::Wave7 => "Wave 7 376/9/3/30",
            Self::Wave8 => "Wave 8 377/8/3/30",
            Self::JailerUidGidPlatformLimit => "post-Wave-8 jailer uid/gid 377/6/3/32",
            Self::JailerChrootPlatformLimit => "post-uid/gid jailer chroot-base-dir 377/5/3/33",
            Self::JailerAggregate => "aggregate jailer 379/3/3/33",
            Self::MultiprocessIsolation => "multiprocess isolation 380/3/2/33",
            Self::HostResourceAuthority => "host-resource authority 381/3/1/33",
            Self::JailerSeccompContainment => "jailer/seccomp containment 382/3/0/33",
            Self::ProductionHost => "production-host corpus 383/2/0/33",
            Self::NetworkVmnetFeasibility => "network/vmnet feasibility 383/0/2/33",
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
        (377, 5, 3, 33) => InventoryPhase::JailerChrootPlatformLimit,
        (379, 3, 3, 33) => InventoryPhase::JailerAggregate,
        (380, 3, 2, 33) => InventoryPhase::MultiprocessIsolation,
        (381, 3, 1, 33) => InventoryPhase::HostResourceAuthority,
        (382, 3, 0, 33) => InventoryPhase::JailerSeccompContainment,
        (383, 2, 0, 33) => InventoryPhase::ProductionHost,
        (383, 0, 2, 33) => InventoryPhase::NetworkVmnetFeasibility,
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
            expected_feasible_ids(phase),
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
    } else if expected_feasible_ids(phase).contains(id) {
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
        .chain(expected_feasible_ids(phase))
        .collect()
}

pub(crate) fn expected_impossible_ids(phase: InventoryPhase) -> BTreeSet<&'static str> {
    let mut ids = wave8_historical_impossible_ids();
    if matches!(
        phase,
        InventoryPhase::JailerUidGidPlatformLimit
            | InventoryPhase::JailerChrootPlatformLimit
            | InventoryPhase::JailerAggregate
            | InventoryPhase::MultiprocessIsolation
            | InventoryPhase::HostResourceAuthority
            | InventoryPhase::JailerSeccompContainment
            | InventoryPhase::ProductionHost
            | InventoryPhase::NetworkVmnetFeasibility
    ) {
        ids.extend(JAILER_UID_GID_IDS);
    }
    if matches!(
        phase,
        InventoryPhase::JailerChrootPlatformLimit
            | InventoryPhase::JailerAggregate
            | InventoryPhase::MultiprocessIsolation
            | InventoryPhase::HostResourceAuthority
            | InventoryPhase::JailerSeccompContainment
            | InventoryPhase::ProductionHost
            | InventoryPhase::NetworkVmnetFeasibility
    ) {
        ids.insert(JAILER_CHROOT_BASE_DIR_ID);
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
        InventoryPhase::JailerChrootPlatformLimit => {
            for id in JAILER_UID_GID_IDS {
                ids.remove(id);
            }
            ids.remove(JAILER_CHROOT_BASE_DIR_ID);
        }
        InventoryPhase::JailerAggregate
        | InventoryPhase::MultiprocessIsolation
        | InventoryPhase::HostResourceAuthority
        | InventoryPhase::JailerSeccompContainment
        | InventoryPhase::ProductionHost
        | InventoryPhase::NetworkVmnetFeasibility => {
            for id in JAILER_UID_GID_IDS {
                ids.remove(id);
            }
            ids.remove(JAILER_CHROOT_BASE_DIR_ID);
            for id in JAILER_AGGREGATE_IDS {
                ids.remove(id);
            }
            if matches!(
                phase,
                InventoryPhase::ProductionHost | InventoryPhase::NetworkVmnetFeasibility
            ) {
                ids.remove(PRODUCTION_HOST_ID);
            }
            if phase == InventoryPhase::NetworkVmnetFeasibility {
                for id in NETWORK_VMNET_FEASIBLE_IDS {
                    ids.remove(id);
                }
            }
        }
    }
    ids
}

fn expected_feasible_ids(phase: InventoryPhase) -> BTreeSet<&'static str> {
    let mut ids = MISSING_PLATFORM_FEASIBLE_IDS
        .into_iter()
        .collect::<BTreeSet<_>>();
    if matches!(
        phase,
        InventoryPhase::MultiprocessIsolation
            | InventoryPhase::HostResourceAuthority
            | InventoryPhase::JailerSeccompContainment
            | InventoryPhase::ProductionHost
            | InventoryPhase::NetworkVmnetFeasibility
    ) {
        ids.remove(MULTIPROCESS_ISOLATION_ID);
    }
    if matches!(
        phase,
        InventoryPhase::HostResourceAuthority
            | InventoryPhase::JailerSeccompContainment
            | InventoryPhase::ProductionHost
            | InventoryPhase::NetworkVmnetFeasibility
    ) {
        ids.remove(HOST_RESOURCE_AUTHORITY_ID);
    }
    if matches!(
        phase,
        InventoryPhase::JailerSeccompContainment
            | InventoryPhase::ProductionHost
            | InventoryPhase::NetworkVmnetFeasibility
    ) {
        ids.remove(JAILER_SECCOMP_CONTAINMENT_ID);
    }
    if phase == InventoryPhase::NetworkVmnetFeasibility {
        ids.extend(NETWORK_VMNET_FEASIBLE_IDS);
    }
    ids
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

    fn restore_containment_handoff(inventory: &mut CapabilityInventory) {
        let capability = inventory
            .capabilities
            .iter_mut()
            .find(|capability| capability.id == JAILER_SECCOMP_CONTAINMENT_ID)
            .expect("containment successor capability must exist");
        capability.source_refs = vec![
            "corpus:jailer".to_string(),
            "corpus:production-host".to_string(),
            "corpus:seccomp".to_string(),
            "corpus:seccompiler".to_string(),
        ];
        capability.disposition = Disposition::MissingPlatformFeasible;
        capability.implementation.clear();
        capability.validation.clear();
        capability.delivery_issue =
            Some("https://github.com/seven332/bangbang/issues/1351".to_string());
        capability.exclusion = None;
    }

    #[test]
    fn exact_inventory_phases_are_closed() {
        let current = current_inventory();
        assert_eq!(
            classify_inventory_phase(&current),
            Ok(InventoryPhase::NetworkVmnetFeasibility)
        );

        let mut production_host = current.clone();
        for id in NETWORK_VMNET_FEASIBLE_IDS {
            set_disposition(&mut production_host, id, Disposition::AuditRequired);
        }
        assert_eq!(
            classify_inventory_phase(&production_host),
            Ok(InventoryPhase::ProductionHost)
        );

        let mut containment = production_host;
        set_disposition(
            &mut containment,
            PRODUCTION_HOST_ID,
            Disposition::AuditRequired,
        );
        assert_eq!(
            classify_inventory_phase(&containment),
            Ok(InventoryPhase::JailerSeccompContainment)
        );

        let mut host_resource = containment;
        restore_containment_handoff(&mut host_resource);
        assert_eq!(
            classify_inventory_phase(&host_resource),
            Ok(InventoryPhase::HostResourceAuthority)
        );

        let mut multiprocess = host_resource;
        set_disposition(
            &mut multiprocess,
            HOST_RESOURCE_AUTHORITY_ID,
            Disposition::MissingPlatformFeasible,
        );
        assert_eq!(
            classify_inventory_phase(&multiprocess),
            Ok(InventoryPhase::MultiprocessIsolation)
        );

        let mut aggregate = multiprocess;
        set_disposition(
            &mut aggregate,
            MULTIPROCESS_ISOLATION_ID,
            Disposition::MissingPlatformFeasible,
        );
        assert_eq!(
            classify_inventory_phase(&aggregate),
            Ok(InventoryPhase::JailerAggregate)
        );

        let mut chroot = aggregate;
        for id in JAILER_AGGREGATE_IDS {
            set_disposition(&mut chroot, id, Disposition::AuditRequired);
        }
        assert_eq!(
            classify_inventory_phase(&chroot),
            Ok(InventoryPhase::JailerChrootPlatformLimit)
        );

        let mut uid_gid = chroot;
        set_disposition(
            &mut uid_gid,
            JAILER_CHROOT_BASE_DIR_ID,
            Disposition::AuditRequired,
        );
        assert_eq!(
            classify_inventory_phase(&uid_gid),
            Ok(InventoryPhase::JailerUidGidPlatformLimit)
        );

        let mut wave8 = uid_gid;
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
            "corpus:network-setup",
            Disposition::ImplementedAndVerified,
        );
        set_disposition(
            &mut inventory,
            "api-operation:GET /",
            Disposition::MissingPlatformFeasible,
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
        for id in NETWORK_VMNET_FEASIBLE_IDS {
            set_disposition(&mut aggregate_swap, id, Disposition::AuditRequired);
        }
        set_disposition(
            &mut aggregate_swap,
            "corpus:jailer",
            Disposition::AuditRequired,
        );
        set_disposition(
            &mut aggregate_swap,
            "corpus:production-host",
            Disposition::ImplementedAndVerified,
        );
        assert!(
            classify_inventory_phase(&aggregate_swap)
                .expect_err("aggregate equal-count movement must fail")
                .contains("identity partition")
        );
    }

    #[test]
    fn partial_uid_gid_successors_do_not_classify() {
        let mut wave8 = current_inventory();
        restore_containment_handoff(&mut wave8);
        set_disposition(
            &mut wave8,
            HOST_RESOURCE_AUTHORITY_ID,
            Disposition::MissingPlatformFeasible,
        );
        set_disposition(
            &mut wave8,
            MULTIPROCESS_ISOLATION_ID,
            Disposition::MissingPlatformFeasible,
        );
        set_disposition(
            &mut wave8,
            JAILER_CHROOT_BASE_DIR_ID,
            Disposition::AuditRequired,
        );
        for id in JAILER_UID_GID_IDS {
            set_disposition(&mut wave8, id, Disposition::AuditRequired);
        }
        for id in JAILER_UID_GID_IDS {
            let mut partial = wave8.clone();
            set_disposition(&mut partial, id, Disposition::ProvenPlatformImpossible);
            assert!(
                classify_inventory_phase(&partial)
                    .expect_err("one terminal jailer identity alone must not classify")
                    .contains("inventory does not match an exact accepted phase"),
                "partial successor unexpectedly classified: {id}"
            );
        }
    }

    #[test]
    fn chroot_without_uid_gid_predecessor_does_not_classify() {
        let mut partial = current_inventory();
        restore_containment_handoff(&mut partial);
        set_disposition(
            &mut partial,
            HOST_RESOURCE_AUTHORITY_ID,
            Disposition::MissingPlatformFeasible,
        );
        set_disposition(
            &mut partial,
            MULTIPROCESS_ISOLATION_ID,
            Disposition::MissingPlatformFeasible,
        );
        for id in JAILER_AGGREGATE_IDS {
            set_disposition(&mut partial, id, Disposition::AuditRequired);
        }
        for id in JAILER_UID_GID_IDS {
            set_disposition(&mut partial, id, Disposition::AuditRequired);
        }
        assert!(
            classify_inventory_phase(&partial)
                .expect_err("chroot without terminal uid/gid must not classify")
                .contains("inventory does not match an exact accepted phase")
        );
    }
}
