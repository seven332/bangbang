//! Firecracker-shaped common-bit stripping for normalized arm64 templates.

use std::collections::BTreeMap;
use std::fmt;

use crate::document::{CpuTemplateDocument, CpuTemplateModifier};

/// Failure while stripping common CPU-template state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuTemplateStripError {
    TooFewInputs,
}

impl fmt::Display for CpuTemplateStripError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TooFewInputs => "CPU-template strip requires at least two inputs",
        })
    }
}

impl std::error::Error for CpuTemplateStripError {}

/// Remove state that selects the same value in every normalized input.
pub fn strip_cpu_template_documents(
    documents: Vec<CpuTemplateDocument>,
) -> Result<Vec<CpuTemplateDocument>, CpuTemplateStripError> {
    if documents.len() < 2 {
        return Err(CpuTemplateStripError::TooFewInputs);
    }

    let mut maps = documents
        .into_iter()
        .map(|document| {
            document
                .modifiers()
                .iter()
                .copied()
                .map(|modifier| (modifier.identity(), modifier))
                .collect::<BTreeMap<_, _>>()
        })
        .collect::<Vec<_>>();
    let first = maps
        .first()
        .cloned()
        .ok_or(CpuTemplateStripError::TooFewInputs)?;

    for (identity, common) in first {
        let mut difference = 0_u128;
        let mut present_in_all = true;
        for map in maps.iter().skip(1) {
            let Some(peer) = map.get(&identity) else {
                present_in_all = false;
                break;
            };
            difference |= (peer.value() & peer.filter()) ^ (common.value() & common.filter());
        }
        if !present_in_all {
            continue;
        }
        if difference == 0 {
            for map in &mut maps {
                map.remove(&identity);
            }
            continue;
        }
        for map in &mut maps {
            let Some(original) = map.get(&identity).copied() else {
                continue;
            };
            let filter = original.filter() & difference;
            map.insert(
                identity,
                CpuTemplateModifier::new(
                    original.identity(),
                    original.width(),
                    filter,
                    original.value() & filter,
                ),
            );
        }
    }

    Ok(maps
        .into_iter()
        .map(|map| CpuTemplateDocument::from_modifiers(map.into_values().collect()))
        .collect())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing)]

    use bangbang_runtime::cpu::{
        CpuConfigArmRegisterWidth, KVM_REG_ARM64_CORE_FPCR, KVM_REG_ARM64_CORE_SP_EL0,
        kvm_reg_arm64_core_q,
    };

    use super::*;
    use crate::document::decode_cpu_template_document;

    fn modifier(
        identity: u64,
        width: CpuConfigArmRegisterWidth,
        filter: u128,
        value: u128,
    ) -> CpuTemplateModifier {
        CpuTemplateModifier::new(identity, width, filter, value)
    }

    fn document(modifiers: Vec<CpuTemplateModifier>) -> CpuTemplateDocument {
        CpuTemplateDocument::from_modifiers(modifiers)
    }

    #[test]
    fn rejects_fewer_than_two_inputs_without_retaining_values() {
        let error = strip_cpu_template_documents(Vec::new()).expect_err("empty batch should fail");
        assert_eq!(error, CpuTemplateStripError::TooFewInputs);
        assert!(!error.to_string().contains("private"));

        assert_eq!(
            strip_cpu_template_documents(vec![document(Vec::new())]),
            Err(CpuTemplateStripError::TooFewInputs)
        );
    }

    #[test]
    fn fully_common_inputs_become_repeatable_canonical_empty_documents() {
        let inputs = vec![
            document(vec![modifier(
                KVM_REG_ARM64_CORE_FPCR,
                CpuConfigArmRegisterWidth::U32,
                u32::MAX.into(),
                0x12,
            )]),
            document(vec![modifier(
                KVM_REG_ARM64_CORE_FPCR,
                CpuConfigArmRegisterWidth::U32,
                u32::MAX.into(),
                0x12,
            )]),
        ];

        let outputs = strip_cpu_template_documents(inputs).expect("strip should succeed");
        assert_eq!(outputs.len(), 2);
        assert!(
            outputs
                .iter()
                .all(|document| document.modifiers().is_empty())
        );
        for output in outputs {
            let bytes = output
                .canonical_bytes()
                .expect("empty output should encode");
            let text = std::str::from_utf8(&bytes).expect("canonical output should be UTF-8");
            let reparsed =
                decode_cpu_template_document(text).expect("output should strictly parse");
            assert_eq!(reparsed, output);
            assert_eq!(reparsed.canonical_bytes().as_deref(), Ok(bytes.as_slice()));
        }
    }

    #[test]
    fn strips_native_width_differences_and_preserves_missing_entries() {
        let q0 = kvm_reg_arm64_core_q(0).expect("Q0 should have an identity");
        let inputs = vec![
            document(vec![
                modifier(
                    KVM_REG_ARM64_CORE_FPCR,
                    CpuConfigArmRegisterWidth::U32,
                    u32::MAX.into(),
                    0,
                ),
                modifier(
                    KVM_REG_ARM64_CORE_SP_EL0,
                    CpuConfigArmRegisterWidth::U64,
                    0b1111,
                    0,
                ),
                modifier(q0, CpuConfigArmRegisterWidth::U128, u128::MAX, 1 << 100),
            ]),
            document(vec![
                modifier(
                    KVM_REG_ARM64_CORE_FPCR,
                    CpuConfigArmRegisterWidth::U32,
                    u32::MAX.into(),
                    1,
                ),
                modifier(
                    KVM_REG_ARM64_CORE_SP_EL0,
                    CpuConfigArmRegisterWidth::U64,
                    0b0011,
                    0b0011,
                ),
            ]),
            document(vec![
                modifier(
                    KVM_REG_ARM64_CORE_FPCR,
                    CpuConfigArmRegisterWidth::U32,
                    u32::MAX.into(),
                    2,
                ),
                modifier(
                    KVM_REG_ARM64_CORE_SP_EL0,
                    CpuConfigArmRegisterWidth::U64,
                    0b1111,
                    0b0101,
                ),
                modifier(q0, CpuConfigArmRegisterWidth::U128, u128::MAX, 1 << 101),
            ]),
        ];

        let outputs = strip_cpu_template_documents(inputs).expect("strip should succeed");
        assert_eq!(outputs.len(), 3);

        let fpcr = outputs[0].modifiers()[0];
        assert_eq!(fpcr.width(), CpuConfigArmRegisterWidth::U32);
        assert_eq!(fpcr.filter(), 0b11);
        assert_eq!(fpcr.value(), 0);
        let sp = outputs[0].modifiers()[1];
        assert_eq!(sp.filter(), 0b0111);
        assert_eq!(sp.value(), 0);
        let q = outputs[0].modifiers()[2];
        assert_eq!(q.identity(), q0);
        assert_eq!(q.filter(), u128::MAX);
        assert_eq!(q.value(), 1 << 100);

        assert_eq!(outputs[1].modifiers().len(), 2);
        assert_eq!(outputs[1].modifiers()[0].filter(), 0b11);
        assert_eq!(outputs[1].modifiers()[0].value(), 1);
        assert_eq!(outputs[1].modifiers()[1].filter(), 0b0011);
        assert_eq!(outputs[1].modifiers()[1].value(), 0b0011);

        assert_eq!(outputs[2].modifiers()[0].filter(), 0b11);
        assert_eq!(outputs[2].modifiers()[0].value(), 2);
        assert_eq!(outputs[2].modifiers()[1].filter(), 0b0111);
        assert_eq!(outputs[2].modifiers()[1].value(), 0b0101);
        assert_eq!(outputs[2].modifiers()[2].filter(), u128::MAX);
        assert_eq!(outputs[2].modifiers()[2].value(), 1 << 101);

        for output in outputs {
            let bytes = output.canonical_bytes().expect("output should encode");
            let text = std::str::from_utf8(&bytes).expect("output should be UTF-8");
            let reparsed =
                decode_cpu_template_document(text).expect("output should strictly parse");
            assert_eq!(reparsed, output);
            assert_eq!(reparsed.canonical_bytes().as_deref(), Ok(bytes.as_slice()));
        }
    }

    #[test]
    fn retains_all_unknown_modifier_when_only_peers_select_the_difference() {
        let inputs = vec![
            document(vec![modifier(
                KVM_REG_ARM64_CORE_SP_EL0,
                CpuConfigArmRegisterWidth::U64,
                0b0011,
                0,
            )]),
            document(vec![modifier(
                KVM_REG_ARM64_CORE_SP_EL0,
                CpuConfigArmRegisterWidth::U64,
                0b0100,
                0b0100,
            )]),
        ];

        let outputs = strip_cpu_template_documents(inputs).expect("strip should succeed");
        assert_eq!(outputs[0].modifiers()[0].filter(), 0);
        assert_eq!(outputs[0].modifiers()[0].value(), 0);
        assert_eq!(outputs[1].modifiers()[0].filter(), 0b0100);
        assert_eq!(outputs[1].modifiers()[0].value(), 0b0100);
        for output in outputs {
            let bytes = output.canonical_bytes().expect("output should encode");
            let text = std::str::from_utf8(&bytes).expect("output should be UTF-8");
            let reparsed = decode_cpu_template_document(text).expect("all-x bitmap should parse");
            assert_eq!(reparsed, output);
        }
    }
}
