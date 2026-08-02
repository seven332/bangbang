use super::*;
use bangbang_runtime::cpu::{
    KVM_REG_ARM64_ACTLR_EL1, KVM_REG_ARM64_CORE_PC, KVM_REG_ARM64_CORE_PSTATE,
    KVM_REG_ARM64_ID_AA64DFR0_EL1, KVM_REG_ARM64_ID_AA64PFR0_EL1, kvm_reg_arm64_core_q,
};

use crate::optional_state::{
    HvfArm64DebugRegisterRestoreState, HvfArm64OptionalStateValue,
    HvfArm64ReviewedOptionalStateRestore, HvfArm64SmeRestoreState, HvfArm64SmeRestoreStateInput,
};
use crate::snapshot_document::tests::{
    inspection_document_fixtures, inspection_native_v1_document,
};
use crate::snapshot_v2::tests::{platform_fixture, platform_fixture_with_count};

use super::super::{HvfNativeSnapshotDocumentState, HvfNativeSnapshotVcpuRef};

const DBGBVR0: u64 = 0x6030_0000_0013_8004;
const DBGBVR1: u64 = 0x6030_0000_0013_800c;
const DBGBCR0: u64 = 0x6030_0000_0013_8005;
const SMCR_EL1: u64 = 0x6030_0000_0013_c096;
const SMPRI_EL1: u64 = 0x6030_0000_0013_c094;
const TPIDR2_EL0: u64 = 0x6030_0000_0013_de85;
const KVM_SVE_Z0: u64 = 0x6080_0000_0015_0000;
const KVM_SVE_P0: u64 = 0x6050_0000_0015_0400;
const KVM_SVE_FFR: u64 = 0x6050_0000_0015_0600;
const KVM_SVE_VLS: u64 = 0x6060_0000_0015_ffff;
const KVM_CNTV_CVAL_EL0: u64 = 0x6030_0000_0013_df1a;
const KVM_CNTPCT_EL0_OR_PTIMER_CNT: u64 = 0x6030_0000_0013_df01;
const KVM_ICC_PMR_EL1: u64 = 0x6030_0000_0013_c230;

fn legacy_document(with_sme: bool) -> HvfNativeSnapshotDocument {
    HvfNativeSnapshotDocument {
        state: HvfNativeSnapshotDocumentState::V2LegacyPlatform(platform_fixture(with_sme)),
    }
}

fn kvm_u64_sysreg(op0: u64, op1: u64, crn: u64, crm: u64, op2: u64) -> u64 {
    0x6000_0000_0000_0000
        | 0x0030_0000_0000_0000
        | 0x0013_0000
        | (op0 << 14)
        | (op1 << 11)
        | (crn << 7)
        | (crm << 3)
        | op2
}

#[test]
fn registry_is_exact_unique_and_pinned_to_firecracker_v1_16() {
    assert_eq!(
        REVIEWED_KVM_REGISTERS.len(),
        HVF_NATIVE_SNAPSHOT_REVIEWED_KVM_REGISTER_COUNT
    );
    for (index, register) in REVIEWED_KVM_REGISTERS.iter().enumerate() {
        assert!(
            REVIEWED_KVM_REGISTERS[..index]
                .iter()
                .all(|previous| previous.id != register.id && previous.target != register.target)
        );
    }

    for index in 0..16_u64 {
        assert_eq!(
            REVIEWED_KVM_REGISTERS[index as usize].id,
            kvm_u64_sysreg(2, 0, 0, index, 4)
        );
        assert_eq!(
            REVIEWED_KVM_REGISTERS[16 + index as usize].id,
            kvm_u64_sysreg(2, 0, 0, index, 5)
        );
        assert_eq!(
            REVIEWED_KVM_REGISTERS[32 + index as usize].id,
            kvm_u64_sysreg(2, 0, 0, index, 6)
        );
        assert_eq!(
            REVIEWED_KVM_REGISTERS[48 + index as usize].id,
            kvm_u64_sysreg(2, 0, 0, index, 7)
        );
    }
    assert_eq!(
        [
            REVIEWED_KVM_REGISTERS[0].id,
            REVIEWED_KVM_REGISTERS[15].id,
            REVIEWED_KVM_REGISTERS[16].id,
            REVIEWED_KVM_REGISTERS[31].id,
            REVIEWED_KVM_REGISTERS[32].id,
            REVIEWED_KVM_REGISTERS[47].id,
            REVIEWED_KVM_REGISTERS[48].id,
            REVIEWED_KVM_REGISTERS[63].id,
        ],
        [
            0x6030_0000_0013_8004,
            0x6030_0000_0013_807c,
            0x6030_0000_0013_8005,
            0x6030_0000_0013_807d,
            0x6030_0000_0013_8006,
            0x6030_0000_0013_807e,
            0x6030_0000_0013_8007,
            0x6030_0000_0013_807f,
        ]
    );
    assert_eq!(
        REVIEWED_KVM_REGISTERS[64..]
            .iter()
            .map(|register| register.id)
            .collect::<Vec<_>>(),
        [SMCR_EL1, SMPRI_EL1, TPIDR2_EL0]
    );
    assert_eq!(SMCR_EL1, kvm_u64_sysreg(3, 0, 1, 2, 6));
    assert_eq!(SMPRI_EL1, kvm_u64_sysreg(3, 0, 1, 2, 4));
    assert_eq!(TPIDR2_EL0, kvm_u64_sysreg(3, 3, 13, 0, 5));

    let ids = REVIEWED_KVM_REGISTERS
        .iter()
        .map(|register| register.id)
        .collect::<Vec<_>>();
    let request = HvfNativeSnapshotRegisterRemovalRequest::try_new(&ids)
        .expect("all 67 exact IDs should be accepted");
    assert_eq!(request.targets(), request.targets.as_slice());
}

#[test]
fn request_validation_rejects_empty_unknown_and_duplicate_ids_without_values() {
    assert!(matches!(
        HvfNativeSnapshotRegisterRemovalRequest::try_new(&[]),
        Err(HvfNativeSnapshotRegisterRemovalError::EmptyRequest)
    ));

    let unsupported = 0xdead_beef_cafe_babe;
    let error = request_error(&[DBGBVR0, unsupported]);
    assert!(matches!(
        &error,
        HvfNativeSnapshotRegisterRemovalError::UnsupportedRegister { request_index: 1 }
    ));
    let rendered = format!("{error:?} / {error}");
    assert!(!rendered.contains("dead"));
    assert!(!rendered.contains(&unsupported.to_string()));

    for (ids, duplicate_request_index) in [
        ([TPIDR2_EL0, TPIDR2_EL0, DBGBVR0], 1),
        ([TPIDR2_EL0, DBGBVR0, TPIDR2_EL0], 2),
    ] {
        let error = request_error(&ids);
        assert!(matches!(
            &error,
            HvfNativeSnapshotRegisterRemovalError::DuplicateRegister {
                first_request_index: 0,
                duplicate_request_index: actual,
            } if *actual == duplicate_request_index
        ));
        let rendered = format!("{error:?} / {error}");
        assert!(!rendered.contains(&TPIDR2_EL0.to_string()));
        assert!(!rendered.contains("6030"));
    }

    let request = HvfNativeSnapshotRegisterRemovalRequest::try_new(&[DBGBVR0, TPIDR2_EL0])
        .expect("reviewed request should validate");
    assert_eq!(request.request_count(), 2);
    let rendered = format!("{request:?}");
    assert!(!rendered.contains("6030"));
    assert!(!rendered.contains(&DBGBVR0.to_string()));
    assert!(!rendered.contains(&TPIDR2_EL0.to_string()));

    let document = legacy_document(true);
    let convenience = document
        .clone()
        .try_remove_reviewed_kvm_registers(&[DBGBVR0, TPIDR2_EL0])
        .expect("slice convenience should transform");
    let prevalidated = document
        .try_remove_reviewed_kvm_register_request(&request)
        .expect("prevalidated request should transform");
    assert_eq!(prevalidated, convenience);
}

#[test]
fn exact_lookup_rejects_other_kvm_families_sizes_and_neighbors() {
    let rejected = [
        0x5030_0000_0013_8004,
        0x6020_0000_0013_8004,
        0x6040_0000_0013_8004,
        KVM_REG_ARM64_CORE_PC,
        KVM_REG_ARM64_CORE_PSTATE,
        kvm_reg_arm64_core_q(0).expect("Q0 should have a KVM identity"),
        KVM_REG_ARM64_ID_AA64PFR0_EL1,
        KVM_REG_ARM64_ID_AA64DFR0_EL1,
        KVM_REG_ARM64_ACTLR_EL1,
        KVM_SVE_Z0,
        KVM_SVE_P0,
        KVM_SVE_FFR,
        KVM_SVE_VLS,
        KVM_CNTV_CVAL_EL0,
        KVM_CNTPCT_EL0_OR_PTIMER_CNT,
        KVM_ICC_PMR_EL1,
        DBGBVR0 ^ 0x100,
        0x6030_0000_0013_8084,
        SMCR_EL1 ^ 1,
        TPIDR2_EL0 ^ 0x8,
        0,
        u64::MAX,
    ];
    for (request_index, id) in rejected.into_iter().enumerate() {
        let error = request_error(&[id]);
        assert!(matches!(
            &error,
            HvfNativeSnapshotRegisterRemovalError::UnsupportedRegister { request_index: 0 }
        ));
        assert_eq!(
            error.to_string(),
            "register-removal request position 0 is not reviewed",
            "case {request_index}"
        );
    }
}

#[test]
fn all_67_semantic_targets_remove_fully_explicit_optional_state() {
    let original = fully_explicit_optional_state();
    let targets = REVIEWED_KVM_REGISTERS
        .iter()
        .map(|register| register.target)
        .collect::<Vec<_>>();
    let mut removed = [false; HVF_NATIVE_SNAPSHOT_REVIEWED_KVM_REGISTER_COUNT];
    let transformed = original
        .clone()
        .try_with_destination_defaults(&targets, &mut removed)
        .expect("every exact semantic target should rebuild");

    assert!(removed.into_iter().all(|removed| removed));
    assert!(
        transformed
            .breakpoints()
            .values()
            .iter()
            .all(|value| { *value == HvfArm64OptionalStateValue::DestinationDefault })
    );
    assert!(
        transformed
            .breakpoints()
            .controls()
            .iter()
            .all(|value| { *value == HvfArm64OptionalStateValue::DestinationDefault })
    );
    assert!(
        transformed
            .watchpoints()
            .values()
            .iter()
            .all(|value| { *value == HvfArm64OptionalStateValue::DestinationDefault })
    );
    assert!(
        transformed
            .watchpoints()
            .controls()
            .iter()
            .all(|value| { *value == HvfArm64OptionalStateValue::DestinationDefault })
    );
    let before_sme = original.sme().expect("fixture should contain SME state");
    let after_sme = transformed
        .sme()
        .expect("transformed fixture should retain SME state");
    assert_eq!(
        after_sme.system_registers(),
        &[HvfArm64OptionalStateValue::DestinationDefault; 3]
    );
    assert_eq!(after_sme.version(), before_sme.version());
    assert_eq!(after_sme.identification(), before_sme.identification());
    assert_eq!(
        after_sme.maximum_svl_bytes(),
        before_sme.maximum_svl_bytes()
    );
    assert_eq!(after_sme.pstate(), before_sme.pstate());
    assert_eq!(after_sme.z_registers(), before_sme.z_registers());
    assert_eq!(after_sme.p_registers(), before_sme.p_registers());
    assert_eq!(after_sme.za_register(), before_sme.za_register());
    assert_eq!(after_sme.zt0_register(), before_sme.zt0_register());
    assert_eq!(
        transformed.expected_id_aa64dfr0_el1(),
        original.expected_id_aa64dfr0_el1()
    );
    assert_eq!(
        transformed.expected_sme_version(),
        original.expected_sme_version()
    );
    assert_eq!(transformed.simd_fp(), original.simd_fp());
}

#[test]
fn native_v1_is_unchanged_and_reports_every_request_not_present() {
    let document = inspection_native_v1_document(1);
    let original = document.clone();
    let original_bytes = original
        .encode()
        .expect("native-v1 fixture should encode canonically");
    let outcome = document
        .try_remove_reviewed_kvm_registers(&[DBGBVR0, SMCR_EL1])
        .expect("native-v1 should accept a reviewed no-op request");

    assert_eq!(outcome.document(), &original);
    assert_eq!(
        outcome
            .document()
            .encode()
            .expect("native-v1 no-op should encode canonically"),
        original_bytes
    );
    assert_eq!(outcome.report().request_count(), 2);
    assert_eq!(outcome.report().removed_count(), 0);
    assert_eq!(outcome.report().not_present_count(), 2);
    assert_eq!(outcome.report().vcpus().len(), 1);
    assert_eq!(outcome.report().vcpus()[0].vcpu_index(), 0);
    assert_eq!(
        outcome.report().vcpus()[0].statuses(),
        [
            HvfNativeSnapshotRegisterRemovalStatus::NotPresent,
            HvfNativeSnapshotRegisterRemovalStatus::NotPresent,
        ]
    );
}

#[test]
fn debug_removal_is_ordered_per_vcpu_and_idempotent() {
    let original = mixed_debug_presence_document();
    let outcome = original
        .clone()
        .try_remove_reviewed_kvm_registers(&[DBGBVR1, DBGBCR0, SMCR_EL1, DBGBVR0])
        .expect("reviewed debug registers should transform");

    assert_eq!(outcome.document().profile(), original.profile());
    assert_eq!(outcome.report().request_count(), 4);
    assert_eq!(outcome.report().removed_count(), 2);
    assert_eq!(outcome.report().not_present_count(), 6);
    for (expected_index, report) in outcome.report().vcpus().iter().enumerate() {
        assert_eq!(report.vcpu_index(), expected_index as u32);
        assert_eq!(
            report.statuses(),
            [
                HvfNativeSnapshotRegisterRemovalStatus::NotPresent,
                if expected_index == 0 {
                    HvfNativeSnapshotRegisterRemovalStatus::Removed
                } else {
                    HvfNativeSnapshotRegisterRemovalStatus::NotPresent
                },
                HvfNativeSnapshotRegisterRemovalStatus::NotPresent,
                if expected_index == 0 {
                    HvfNativeSnapshotRegisterRemovalStatus::Removed
                } else {
                    HvfNativeSnapshotRegisterRemovalStatus::NotPresent
                },
            ]
        );
    }

    for (before, after) in original.vcpus().zip(outcome.document().vcpus()) {
        let (HvfNativeSnapshotVcpuRef::V2(before), HvfNativeSnapshotVcpuRef::V2(after)) =
            (before, after)
        else {
            panic!("legacy native-v2 fixture should expose native-v2 vCPUs");
        };
        assert_non_optional_vcpu_state_preserved(before, after);
        assert_eq!(
            after.reviewed_optional().breakpoints().values()[0],
            HvfArm64OptionalStateValue::DestinationDefault
        );
        assert_eq!(
            after.reviewed_optional().breakpoints().controls()[0],
            HvfArm64OptionalStateValue::DestinationDefault
        );
        assert_eq!(
            after.reviewed_optional().breakpoints().values()[1..],
            before.reviewed_optional().breakpoints().values()[1..]
        );
        assert_eq!(
            after.reviewed_optional().breakpoints().controls()[1..],
            before.reviewed_optional().breakpoints().controls()[1..]
        );
        assert_eq!(
            after.reviewed_optional().watchpoints(),
            before.reviewed_optional().watchpoints()
        );
        assert_eq!(after.reviewed_optional().sme(), None);
    }

    let transformed = outcome.document().clone();
    let second = transformed
        .clone()
        .try_remove_reviewed_kvm_registers(&[DBGBVR1, DBGBCR0, SMCR_EL1, DBGBVR0])
        .expect("repeating a reviewed request should be a successful no-op");
    assert_eq!(second.document(), &transformed);
    assert_eq!(second.report().removed_count(), 0);
    assert_eq!(second.report().not_present_count(), 8);
    assert!(second.report().vcpus().iter().all(|report| {
        report
            .statuses()
            .iter()
            .all(|status| *status == HvfNativeSnapshotRegisterRemovalStatus::NotPresent)
    }));
}

#[test]
fn sme_removal_preserves_conditional_state_and_reports_absent_values() {
    let original = legacy_document(true);
    let outcome = original
        .clone()
        .try_remove_reviewed_kvm_registers(&[TPIDR2_EL0, SMPRI_EL1, SMCR_EL1])
        .expect("reviewed SME system registers should transform");

    assert_eq!(outcome.report().removed_count(), 4);
    assert_eq!(outcome.report().not_present_count(), 2);
    for report in outcome.report().vcpus() {
        assert_eq!(
            report.statuses(),
            [
                HvfNativeSnapshotRegisterRemovalStatus::Removed,
                HvfNativeSnapshotRegisterRemovalStatus::NotPresent,
                HvfNativeSnapshotRegisterRemovalStatus::Removed,
            ]
        );
    }

    for (before, after) in original.vcpus().zip(outcome.document().vcpus()) {
        let (HvfNativeSnapshotVcpuRef::V2(before), HvfNativeSnapshotVcpuRef::V2(after)) =
            (before, after)
        else {
            panic!("SME fixture should expose native-v2 vCPUs");
        };
        assert_non_optional_vcpu_state_preserved(before, after);
        assert_eq!(
            after.reviewed_optional().breakpoints(),
            before.reviewed_optional().breakpoints()
        );
        assert_eq!(
            after.reviewed_optional().watchpoints(),
            before.reviewed_optional().watchpoints()
        );
        let before = before
            .reviewed_optional()
            .sme()
            .expect("fixture should carry SME state");
        let after = after
            .reviewed_optional()
            .sme()
            .expect("transformed fixture should retain SME state");
        assert_eq!(
            after.system_registers(),
            &[HvfArm64OptionalStateValue::DestinationDefault; 3]
        );
        assert_eq!(after.version(), before.version());
        assert_eq!(after.identification(), before.identification());
        assert_eq!(after.maximum_svl_bytes(), before.maximum_svl_bytes());
        assert_eq!(after.pstate(), before.pstate());
        assert_eq!(after.z_registers(), before.z_registers());
        assert_eq!(after.p_registers(), before.p_registers());
        assert_eq!(after.za_register(), before.za_register());
        assert_eq!(after.zt0_register(), before.zt0_register());
    }
}

#[test]
fn every_exact_profile_rebuilds_and_round_trips_without_outer_profile_drift() {
    for document in inspection_document_fixtures() {
        let family = document.family();
        let version = document.version();
        let profile = document.profile();
        let vcpu_count = document.vcpu_count();
        let outcome = document
            .try_remove_reviewed_kvm_registers(&[DBGBVR0])
            .expect("every exact native profile should rebuild");
        assert_eq!(outcome.document().family(), family);
        assert_eq!(outcome.document().version(), version);
        assert_eq!(outcome.document().profile(), profile);
        assert_eq!(outcome.document().vcpu_count(), vcpu_count);

        let bytes = outcome
            .document()
            .encode()
            .expect("transformed profile should encode");
        let decoded =
            HvfNativeSnapshotDocument::decode(&bytes).expect("transformed profile should decode");
        assert_eq!(&decoded, outcome.document());
    }
}

#[test]
fn diff_rebuild_preserves_layer_memory_devices_and_non_target_vcpu_state() {
    let original = inspection_document_fixtures()
        .into_iter()
        .find(|document| matches!(&document.state, HvfNativeSnapshotDocumentState::V2Diff(_)))
        .expect("profile matrix should contain exact 2.13 Diff");
    let outcome = original
        .clone()
        .try_remove_reviewed_kvm_registers(&[DBGBVR0])
        .expect("Diff breakpoint removal should rebuild");
    let (
        HvfNativeSnapshotDocumentState::V2Diff(before),
        HvfNativeSnapshotDocumentState::V2Diff(after),
    ) = (&original.state, &outcome.document().state)
    else {
        panic!("Diff transform must retain its exact state variant");
    };

    assert_eq!(after.device_graph(), before.device_graph());
    assert_eq!(after.serial(), before.serial());
    assert_eq!(after.entropy(), before.entropy());
    assert_eq!(after.balloon(), before.balloon());
    assert_eq!(after.memory_hotplug(), before.memory_hotplug());
    assert_eq!(after.network(), before.network());
    assert_eq!(after.vsock(), before.vsock());
    assert_eq!(after.layer(), before.layer());
    assert_eq!(after.platform().memory(), before.platform().memory());
    assert_eq!(after.platform().machine(), before.platform().machine());
    assert_eq!(after.platform().global(), before.platform().global());
    assert_eq!(after.platform().topology(), before.platform().topology());
    assert_eq!(after.platform().time(), before.platform().time());
    for (before, after) in before
        .platform()
        .vcpus()
        .iter()
        .zip(after.platform().vcpus())
    {
        assert_non_optional_vcpu_state_preserved(before, after);
        assert_eq!(
            after.reviewed_optional().breakpoints().values()[0],
            HvfArm64OptionalStateValue::DestinationDefault
        );
        assert_eq!(
            after.reviewed_optional().breakpoints().values()[1..],
            before.reviewed_optional().breakpoints().values()[1..]
        );
        assert_eq!(
            after.reviewed_optional().breakpoints().controls(),
            before.reviewed_optional().breakpoints().controls()
        );
        assert_eq!(
            after.reviewed_optional().watchpoints(),
            before.reviewed_optional().watchpoints()
        );
        assert_eq!(
            after.reviewed_optional().sme(),
            before.reviewed_optional().sme()
        );
    }
}

#[test]
fn maximum_vcpu_report_is_bounded_canonical_and_complete() {
    let document = HvfNativeSnapshotDocument {
        state: HvfNativeSnapshotDocumentState::V2LegacyPlatform(platform_fixture_with_count(
            32, false,
        )),
    };
    let outcome = document
        .try_remove_reviewed_kvm_registers(&[DBGBVR0])
        .expect("maximum checked vCPU inventory should transform");
    assert_eq!(outcome.report().vcpus().len(), 32);
    assert_eq!(outcome.report().removed_count(), 32);
    assert_eq!(outcome.report().not_present_count(), 0);
    for (expected_index, report) in outcome.report().vcpus().iter().enumerate() {
        assert_eq!(report.vcpu_index(), expected_index as u32);
        assert_eq!(
            report.statuses(),
            [HvfNativeSnapshotRegisterRemovalStatus::Removed]
        );
        assert_eq!(report.removed_count(), 1);
        assert_eq!(report.not_present_count(), 0);
    }
}

#[test]
fn outcome_debug_is_value_free() {
    let outcome = legacy_document(true)
        .try_remove_reviewed_kvm_registers(&[DBGBVR0, TPIDR2_EL0])
        .expect("reviewed request should transform");
    let debug = format!("{outcome:?}");
    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("1111"));
    assert!(!debug.contains("fixture"));
    assert!(!debug.contains("6030"));
}

fn request_error(ids: &[u64]) -> HvfNativeSnapshotRegisterRemovalError {
    match HvfNativeSnapshotRegisterRemovalRequest::try_new(ids) {
        Ok(_) => panic!("request should have been rejected"),
        Err(error) => error,
    }
}

fn fully_explicit_optional_state() -> HvfArm64ReviewedOptionalStateRestore {
    let platform = platform_fixture(true);
    let vcpu = &platform.vcpus()[0];
    let source = vcpu.reviewed_optional();
    let simd_fp = vcpu.mandatory().simd_fp.clone();
    let expected_dfr0 = (source.expected_id_aa64dfr0_el1() & !((0xf << 12) | (0xf << 20)))
        | (0xf << 12)
        | (0xf << 20);
    let breakpoints = HvfArm64DebugRegisterRestoreState::try_new(
        16,
        std::array::from_fn(|index| HvfArm64OptionalStateValue::Explicit(index as u64 + 1)),
        std::array::from_fn(|index| HvfArm64OptionalStateValue::Explicit(index as u64 + 101)),
    )
    .expect("fully explicit breakpoint state should validate");
    let watchpoints = HvfArm64DebugRegisterRestoreState::try_new(
        16,
        std::array::from_fn(|index| HvfArm64OptionalStateValue::Explicit(index as u64 + 201)),
        std::array::from_fn(|index| HvfArm64OptionalStateValue::Explicit(index as u64 + 301)),
    )
    .expect("fully explicit watchpoint state should validate");
    let source_sme = source
        .sme()
        .expect("source fixture should contain SME state");
    let input = HvfArm64SmeRestoreStateInput::new(
        source_sme.version(),
        source_sme.identification(),
        source_sme.maximum_svl_bytes(),
        source_sme.pstate(),
        [
            HvfArm64OptionalStateValue::Explicit(401),
            HvfArm64OptionalStateValue::Explicit(402),
            HvfArm64OptionalStateValue::Explicit(403),
        ],
    );
    let input = match (source_sme.z_registers(), source_sme.p_registers()) {
        (Some(z_registers), Some(p_registers)) => {
            input.with_streaming_registers(z_registers.to_vec(), p_registers.to_vec())
        }
        (None, None) => input,
        _ => panic!("checked SME fixture must pair streaming inventories"),
    };
    let input = match (source_sme.za_register(), source_sme.zt0_register()) {
        (Some(za_register), zt0_register) => {
            input.with_za_register(za_register.clone(), zt0_register.copied())
        }
        (None, None) => input,
        (None, Some(_)) => panic!("checked SME fixture must pair ZA and ZT0"),
    };
    let sme = HvfArm64SmeRestoreState::try_new(input, &simd_fp)
        .expect("fully explicit SME state should validate");
    HvfArm64ReviewedOptionalStateRestore::try_new(
        expected_dfr0,
        source.expected_sme_version(),
        breakpoints,
        watchpoints,
        Some(sme),
        simd_fp,
    )
    .expect("fully explicit reviewed state should validate")
}

fn mixed_debug_presence_document() -> HvfNativeSnapshotDocument {
    let platform = platform_fixture(false);
    let mut vcpus = platform.vcpus().to_vec();
    let (index, mpidr, mandatory, timer, pending_interrupts, gic_icc, optional) =
        vcpus[1].clone().into_parts();
    let mut breakpoint_values = *optional.breakpoints().values();
    let mut breakpoint_controls = *optional.breakpoints().controls();
    breakpoint_values[0] = HvfArm64OptionalStateValue::DestinationDefault;
    breakpoint_controls[0] = HvfArm64OptionalStateValue::DestinationDefault;
    let breakpoints = HvfArm64DebugRegisterRestoreState::try_new(
        optional.breakpoints().implemented_count(),
        breakpoint_values,
        breakpoint_controls,
    )
    .expect("mixed-presence breakpoint fixture should validate");
    let optional = HvfArm64ReviewedOptionalStateRestore::try_new(
        optional.expected_id_aa64dfr0_el1(),
        optional.expected_sme_version(),
        breakpoints,
        optional.watchpoints().clone(),
        optional.sme().cloned(),
        optional.simd_fp().clone(),
    )
    .expect("mixed-presence reviewed state should validate");
    vcpus[1] = HvfSnapshotV2VcpuState::try_new(
        index,
        mpidr,
        mandatory,
        timer,
        pending_interrupts,
        gic_icc,
        optional,
    )
    .expect("mixed-presence vCPU should validate");
    let platform = platform
        .try_replace_vcpus(vcpus)
        .expect("mixed-presence platform should validate");
    HvfNativeSnapshotDocument {
        state: HvfNativeSnapshotDocumentState::V2LegacyPlatform(platform),
    }
}

fn assert_non_optional_vcpu_state_preserved(
    before: &HvfSnapshotV2VcpuState,
    after: &HvfSnapshotV2VcpuState,
) {
    assert_eq!(after.index(), before.index());
    assert_eq!(after.mpidr(), before.mpidr());
    assert_eq!(after.mandatory(), before.mandatory());
    assert_eq!(after.timer(), before.timer());
    assert_eq!(after.pending_interrupts(), before.pending_interrupts());
    assert_eq!(after.gic_icc(), before.gic_icc());
    assert_eq!(
        after.reviewed_optional().expected_id_aa64dfr0_el1(),
        before.reviewed_optional().expected_id_aa64dfr0_el1()
    );
    assert_eq!(
        after.reviewed_optional().expected_sme_version(),
        before.reviewed_optional().expected_sme_version()
    );
    assert_eq!(
        after.reviewed_optional().simd_fp(),
        before.reviewed_optional().simd_fp()
    );
}

#[cfg(unix)]
#[test]
fn portable_state_transaction_publishes_exact_v1_v2_and_diff_documents() {
    use std::fs;
    use std::os::unix::fs::MetadataExt;

    use bangbang_runtime::snapshot_state_edit::{
        SnapshotStateEditCommit, SnapshotStateEditPaths, publish_edited_snapshot_state_with,
    };

    let mut documents = vec![inspection_native_v1_document(1), legacy_document(true)];
    documents.push(
        inspection_document_fixtures()
            .into_iter()
            .find(|document| matches!(document.state, HvfNativeSnapshotDocumentState::V2Diff(_)))
            .expect("profile matrix should contain exact 2.13 Diff"),
    );

    for (index, document) in documents.into_iter().enumerate() {
        let directory = RegisterEditTestDirectory::new(index);
        let input = directory.path.join("input.state");
        let output = directory.path.join("output.state");
        let source_bytes = document.encode().expect("source document should encode");
        fs::write(&input, &source_bytes).expect("source should write");
        let source_facts = fs::metadata(&input).expect("source facts should read");
        let source_facts = (
            source_facts.dev(),
            source_facts.ino(),
            source_facts.mode(),
            source_facts.size(),
            source_facts.mtime(),
            source_facts.mtime_nsec(),
            source_facts.ctime(),
            source_facts.ctime_nsec(),
        );
        let request = HvfNativeSnapshotRegisterRemovalRequest::try_new(&[DBGBVR0, TPIDR2_EL0])
            .expect("request should validate before path access");
        let paths = SnapshotStateEditPaths::new(&input, &output);

        let outcome = publish_edited_snapshot_state_with(
            &paths,
            |bytes| {
                let document = HvfNativeSnapshotDocument::decode(bytes)
                    .map_err(RegisterEditPublicationTestError::Decode)?;
                document
                    .try_remove_reviewed_kvm_register_request(&request)
                    .map_err(RegisterEditPublicationTestError::Transform)
            },
            |outcome| {
                outcome
                    .document()
                    .encode()
                    .map_err(RegisterEditPublicationTestError::Encode)
            },
            |bytes, outcome| {
                let decoded = HvfNativeSnapshotDocument::decode(bytes)
                    .map_err(RegisterEditPublicationTestError::Decode)?;
                if &decoded == outcome.document() {
                    Ok(())
                } else {
                    Err(RegisterEditPublicationTestError::Mismatch)
                }
            },
        )
        .expect("exact edited state should publish");

        assert_eq!(outcome.commit(), SnapshotStateEditCommit::Durable);
        assert_eq!(outcome.product().report().request_count(), 2);
        let published_bytes = fs::read(&output).expect("published output should read");
        let published = HvfNativeSnapshotDocument::decode(&published_bytes)
            .expect("published document should decode");
        assert_eq!(&published, outcome.product().document());
        assert_eq!(
            fs::metadata(&output)
                .expect("published mode should read")
                .mode()
                & 0o7777,
            0o600
        );
        assert_eq!(fs::read(&input).expect("source should read"), source_bytes);
        let after = fs::metadata(&input).expect("source facts should reread");
        assert_eq!(
            (
                after.dev(),
                after.ino(),
                after.mode(),
                after.size(),
                after.mtime(),
                after.mtime_nsec(),
                after.ctime(),
                after.ctime_nsec(),
            ),
            source_facts
        );
        assert!(
            fs::read_dir(&directory.path)
                .expect("directory should enumerate")
                .map(|entry| entry.expect("entry should read"))
                .all(|entry| !entry
                    .file_name()
                    .as_encoded_bytes()
                    .starts_with(b".bangbang-snapshot-edit-"))
        );
    }
}

#[cfg(unix)]
#[derive(Debug)]
enum RegisterEditPublicationTestError {
    Decode(crate::snapshot_document::HvfNativeSnapshotDocumentDecodeError),
    Transform(HvfNativeSnapshotRegisterRemovalError),
    Encode(crate::snapshot_document::HvfNativeSnapshotDocumentEncodeError),
    Mismatch,
}

#[cfg(unix)]
impl fmt::Display for RegisterEditPublicationTestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Decode(_) => "test decode failure",
            Self::Transform(_) => "test transform failure",
            Self::Encode(_) => "test encode failure",
            Self::Mismatch => "test exact-document mismatch",
        })
    }
}

#[cfg(unix)]
impl std::error::Error for RegisterEditPublicationTestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Decode(source) => Some(source),
            Self::Transform(source) => Some(source),
            Self::Encode(source) => Some(source),
            Self::Mismatch => None,
        }
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct RegisterEditTestDirectory {
    path: std::path::PathBuf,
}

#[cfg(unix)]
impl RegisterEditTestDirectory {
    fn new(index: usize) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "bb-hvf-state-edit-{}-{index}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&path).expect("test directory should create");
        Self { path }
    }
}

#[cfg(unix)]
impl Drop for RegisterEditTestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
