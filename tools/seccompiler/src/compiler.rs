// Copyright 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Adapted in 2026 for bangbang from the former Firecracker/rust-vmm pure-Rust
// seccompiler comparison and action semantics. This version is offline-only,
// normalizes Firecracker v1.16's current libseccomp input semantics, and uses
// the checked label assembler in this crate.

use std::collections::BTreeMap;

use crate::bpf::{
    Assembler, BPF_JMP_JEQ_K, BPF_JMP_JGE_K, BPF_JMP_JGT_K, Label, SECCOMP_DATA_ARCH_OFFSET,
    SECCOMP_DATA_ARG_SIZE, SECCOMP_DATA_ARGS_OFFSET, SECCOMP_DATA_NR_OFFSET, to_words,
};
use crate::schema::{ArgumentLength, CompareOperator, Condition, Filter, Policy, Rule};
use crate::syscalls::{Resolution, SyscallTable};
use crate::{CompileError, CompileOptions, CompiledFilters, TargetArch};

const AUDIT_ARCH_X86_64: u32 = 0xc000_003e;
const AUDIT_ARCH_AARCH64: u32 = 0xc000_00b7;
const X32_SYSCALL_BIT: u32 = 0x4000_0000;
// Firecracker v1.16 leaves libseccomp's bad-architecture action at its
// default, which is SCMP_ACT_KILL_THREAD.
const BAD_ARCH_ACTION: u32 = 0x0000_0000;

#[derive(Debug)]
struct CompiledRule {
    conditions: Vec<NormalizedCondition>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NormalizedCondition {
    index: u8,
    operator: NormalizedOperator,
    value: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NormalizedOperator {
    Eq,
    Ge,
    Gt,
    Le,
    Lt,
    MaskedEq(u64),
    Ne,
}

#[derive(Debug)]
struct SyscallBlock {
    number: u32,
    rules: Vec<CompiledRule>,
    label: Label,
}

pub(crate) fn compile(
    policy: Policy,
    target_arch: TargetArch,
    options: CompileOptions,
    syscall_table: &SyscallTable,
) -> Result<CompiledFilters, CompileError> {
    let mut compiled = BTreeMap::new();
    for (category, filter) in policy.into_filters() {
        let program = compile_filter(filter, target_arch, options, syscall_table)?;
        compiled.insert(category.to_owned(), program);
    }
    Ok(compiled)
}

fn compile_filter(
    filter: Filter,
    target_arch: TargetArch,
    options: CompileOptions,
    syscall_table: &SyscallTable,
) -> Result<Vec<u64>, CompileError> {
    let Filter {
        default_action,
        filter_action,
        rules,
    } = filter;
    let grouped_rules = group_rules(rules, target_arch, options, syscall_table)?;

    let mut assembler = Assembler::new();
    emit_architecture_guard(&mut assembler, target_arch)?;

    let blocks = grouped_rules
        .into_iter()
        .map(|(number, rules)| SyscallBlock {
            number,
            rules,
            label: assembler.new_label(),
        })
        .collect::<Vec<_>>();

    for block in &blocks {
        assembler.dispatch_equal(block.number, block.label);
    }
    assembler.return_action(default_action.encode());

    for block in blocks {
        assembler.bind(block.label)?;
        emit_syscall_block(
            &mut assembler,
            block.rules,
            filter_action.encode(),
            default_action.encode(),
        )?;
    }

    assembler.finish().map(to_words)
}

fn group_rules(
    rules: Vec<Rule>,
    target_arch: TargetArch,
    options: CompileOptions,
    syscall_table: &SyscallTable,
) -> Result<BTreeMap<u32, Vec<CompiledRule>>, CompileError> {
    let mut grouped = BTreeMap::<u32, Vec<CompiledRule>>::new();

    for rule in rules {
        let resolution = syscall_table
            .resolve(&rule.syscall, target_arch)
            .ok_or(CompileError::UnknownSyscall)?;
        let Resolution::Number(number) = resolution else {
            continue;
        };

        let conditions = if options.is_basic() {
            Vec::new()
        } else {
            rule.conditions
                .unwrap_or_default()
                .into_iter()
                .map(normalize_condition)
                .collect()
        };

        let accumulated = grouped.entry(number).or_default();
        if accumulated
            .iter()
            .any(|existing| existing.conditions.is_empty())
        {
            continue;
        }
        if conditions.is_empty() {
            accumulated.clear();
        }
        accumulated.push(CompiledRule { conditions });
    }

    Ok(grouped)
}

fn normalize_condition(condition: Condition) -> NormalizedCondition {
    let operator = match (condition.value_length, condition.operator) {
        (ArgumentLength::Dword, CompareOperator::Eq) => {
            NormalizedOperator::MaskedEq(0x0000_0000_ffff_ffff)
        }
        (_, CompareOperator::Eq) => NormalizedOperator::Eq,
        (_, CompareOperator::Ge) => NormalizedOperator::Ge,
        (_, CompareOperator::Gt) => NormalizedOperator::Gt,
        (_, CompareOperator::Le) => NormalizedOperator::Le,
        (_, CompareOperator::Lt) => NormalizedOperator::Lt,
        (_, CompareOperator::MaskedEq(mask)) => NormalizedOperator::MaskedEq(mask),
        (_, CompareOperator::Ne) => NormalizedOperator::Ne,
    };

    NormalizedCondition {
        index: condition.index,
        operator,
        value: condition.value,
    }
}

fn emit_architecture_guard(
    assembler: &mut Assembler,
    target_arch: TargetArch,
) -> Result<(), CompileError> {
    let valid_architecture = assembler.new_label();
    let bad_architecture = assembler.new_label();
    assembler.load(SECCOMP_DATA_ARCH_OFFSET);
    assembler.branch(
        BPF_JMP_JEQ_K,
        match target_arch {
            TargetArch::X86_64 => AUDIT_ARCH_X86_64,
            TargetArch::Aarch64 => AUDIT_ARCH_AARCH64,
        },
        valid_architecture,
        bad_architecture,
    );
    assembler.bind(bad_architecture)?;
    assembler.return_action(BAD_ARCH_ACTION);
    assembler.bind(valid_architecture)?;
    assembler.load(SECCOMP_DATA_NR_OFFSET);

    if target_arch == TargetArch::X86_64 {
        let check_tracing_sentinel = assembler.new_label();
        let dispatch = assembler.new_label();
        let bad_abi = assembler.new_label();
        assembler.branch(
            BPF_JMP_JGE_K,
            X32_SYSCALL_BIT,
            check_tracing_sentinel,
            dispatch,
        );
        assembler.bind(check_tracing_sentinel)?;
        assembler.branch(BPF_JMP_JEQ_K, u32::MAX, dispatch, bad_abi);
        assembler.bind(bad_abi)?;
        assembler.return_action(BAD_ARCH_ACTION);
        assembler.bind(dispatch)?;
    }

    Ok(())
}

fn emit_syscall_block(
    assembler: &mut Assembler,
    rules: Vec<CompiledRule>,
    match_action: u32,
    default_action: u32,
) -> Result<(), CompileError> {
    if rules.iter().any(|rule| rule.conditions.is_empty()) {
        assembler.return_action(match_action);
        return Ok(());
    }

    let matched = assembler.new_label();
    for rule in rules {
        let failed = assembler.new_label();
        emit_rule(assembler, rule, matched, failed)?;
        assembler.bind(failed)?;
    }
    assembler.return_action(default_action);
    assembler.bind(matched)?;
    assembler.return_action(match_action);
    Ok(())
}

fn emit_rule(
    assembler: &mut Assembler,
    rule: CompiledRule,
    matched: Label,
    failed: Label,
) -> Result<(), CompileError> {
    let mut conditions = rule.conditions.into_iter().peekable();
    while let Some(condition) = conditions.next() {
        let has_more = conditions.peek().is_some();
        let condition_matched = if has_more {
            assembler.new_label()
        } else {
            matched
        };
        emit_condition(assembler, condition, condition_matched, failed)?;
        if has_more {
            assembler.bind(condition_matched)?;
        }
    }
    Ok(())
}

fn emit_condition(
    assembler: &mut Assembler,
    condition: NormalizedCondition,
    matched: Label,
    failed: Label,
) -> Result<(), CompileError> {
    let low_offset = SECCOMP_DATA_ARGS_OFFSET
        .checked_add(u32::from(condition.index) * SECCOMP_DATA_ARG_SIZE)
        .ok_or(CompileError::InvalidProgram)?;
    let high_offset = low_offset
        .checked_add(4)
        .ok_or(CompileError::InvalidProgram)?;
    let high_value = (condition.value >> 32) as u32;
    let low_value = condition.value as u32;

    match condition.operator {
        NormalizedOperator::Eq => emit_equal(
            assembler,
            high_offset,
            low_offset,
            high_value,
            low_value,
            matched,
            failed,
        )?,
        NormalizedOperator::Ne => emit_not_equal(
            assembler,
            high_offset,
            low_offset,
            high_value,
            low_value,
            matched,
            failed,
        )?,
        NormalizedOperator::Ge => emit_greater_or_equal(
            assembler,
            high_offset,
            low_offset,
            high_value,
            low_value,
            matched,
            failed,
        )?,
        NormalizedOperator::Gt => emit_greater_than(
            assembler,
            high_offset,
            low_offset,
            high_value,
            low_value,
            matched,
            failed,
        )?,
        NormalizedOperator::Le => emit_less_or_equal(
            assembler,
            high_offset,
            low_offset,
            high_value,
            low_value,
            matched,
            failed,
        )?,
        NormalizedOperator::Lt => emit_less_than(
            assembler,
            high_offset,
            low_offset,
            high_value,
            low_value,
            matched,
            failed,
        )?,
        NormalizedOperator::MaskedEq(mask) => emit_masked_equal(
            assembler,
            high_offset,
            low_offset,
            condition.value,
            mask,
            matched,
            failed,
        )?,
    }
    Ok(())
}

fn emit_equal(
    assembler: &mut Assembler,
    high_offset: u32,
    low_offset: u32,
    high_value: u32,
    low_value: u32,
    matched: Label,
    failed: Label,
) -> Result<(), CompileError> {
    let compare_low = assembler.new_label();
    assembler.load(high_offset);
    assembler.branch(BPF_JMP_JEQ_K, high_value, compare_low, failed);
    assembler.bind(compare_low)?;
    assembler.load(low_offset);
    assembler.branch(BPF_JMP_JEQ_K, low_value, matched, failed);
    Ok(())
}

fn emit_not_equal(
    assembler: &mut Assembler,
    high_offset: u32,
    low_offset: u32,
    high_value: u32,
    low_value: u32,
    matched: Label,
    failed: Label,
) -> Result<(), CompileError> {
    let compare_low = assembler.new_label();
    assembler.load(high_offset);
    assembler.branch(BPF_JMP_JEQ_K, high_value, compare_low, matched);
    assembler.bind(compare_low)?;
    assembler.load(low_offset);
    assembler.branch(BPF_JMP_JEQ_K, low_value, failed, matched);
    Ok(())
}

fn emit_greater_or_equal(
    assembler: &mut Assembler,
    high_offset: u32,
    low_offset: u32,
    high_value: u32,
    low_value: u32,
    matched: Label,
    failed: Label,
) -> Result<(), CompileError> {
    let high_not_greater = assembler.new_label();
    let compare_low = assembler.new_label();
    assembler.load(high_offset);
    assembler.branch(BPF_JMP_JGT_K, high_value, matched, high_not_greater);
    assembler.bind(high_not_greater)?;
    assembler.branch(BPF_JMP_JEQ_K, high_value, compare_low, failed);
    assembler.bind(compare_low)?;
    assembler.load(low_offset);
    assembler.branch(BPF_JMP_JGE_K, low_value, matched, failed);
    Ok(())
}

fn emit_greater_than(
    assembler: &mut Assembler,
    high_offset: u32,
    low_offset: u32,
    high_value: u32,
    low_value: u32,
    matched: Label,
    failed: Label,
) -> Result<(), CompileError> {
    let high_not_greater = assembler.new_label();
    let compare_low = assembler.new_label();
    assembler.load(high_offset);
    assembler.branch(BPF_JMP_JGT_K, high_value, matched, high_not_greater);
    assembler.bind(high_not_greater)?;
    assembler.branch(BPF_JMP_JEQ_K, high_value, compare_low, failed);
    assembler.bind(compare_low)?;
    assembler.load(low_offset);
    assembler.branch(BPF_JMP_JGT_K, low_value, matched, failed);
    Ok(())
}

fn emit_less_or_equal(
    assembler: &mut Assembler,
    high_offset: u32,
    low_offset: u32,
    high_value: u32,
    low_value: u32,
    matched: Label,
    failed: Label,
) -> Result<(), CompileError> {
    let high_not_greater = assembler.new_label();
    let compare_low = assembler.new_label();
    assembler.load(high_offset);
    assembler.branch(BPF_JMP_JGT_K, high_value, failed, high_not_greater);
    assembler.bind(high_not_greater)?;
    assembler.branch(BPF_JMP_JEQ_K, high_value, compare_low, matched);
    assembler.bind(compare_low)?;
    assembler.load(low_offset);
    assembler.branch(BPF_JMP_JGT_K, low_value, failed, matched);
    Ok(())
}

fn emit_less_than(
    assembler: &mut Assembler,
    high_offset: u32,
    low_offset: u32,
    high_value: u32,
    low_value: u32,
    matched: Label,
    failed: Label,
) -> Result<(), CompileError> {
    let high_not_greater = assembler.new_label();
    let compare_low = assembler.new_label();
    assembler.load(high_offset);
    assembler.branch(BPF_JMP_JGT_K, high_value, failed, high_not_greater);
    assembler.bind(high_not_greater)?;
    assembler.branch(BPF_JMP_JEQ_K, high_value, compare_low, matched);
    assembler.bind(compare_low)?;
    assembler.load(low_offset);
    assembler.branch(BPF_JMP_JGE_K, low_value, failed, matched);
    Ok(())
}

fn emit_masked_equal(
    assembler: &mut Assembler,
    high_offset: u32,
    low_offset: u32,
    value: u64,
    mask: u64,
    matched: Label,
    failed: Label,
) -> Result<(), CompileError> {
    let masked_value = value & mask;
    let high_mask = (mask >> 32) as u32;
    let low_mask = mask as u32;
    let high_value = (masked_value >> 32) as u32;
    let low_value = masked_value as u32;

    if high_mask == 0 && low_mask == 0 {
        assembler.jump(matched);
        return Ok(());
    }

    if high_mask != 0 {
        let high_matches = if low_mask == 0 {
            matched
        } else {
            assembler.new_label()
        };
        assembler.load(high_offset);
        if high_mask != u32::MAX {
            assembler.bitwise_and(high_mask);
        }
        assembler.branch(BPF_JMP_JEQ_K, high_value, high_matches, failed);
        if low_mask != 0 {
            assembler.bind(high_matches)?;
        }
    }

    if low_mask != 0 {
        assembler.load(low_offset);
        if low_mask != u32::MAX {
            assembler.bitwise_and(low_mask);
        }
        assembler.branch(BPF_JMP_JEQ_K, low_value, matched, failed);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Action, ArgumentLength, CompareOperator};

    #[test]
    fn normalizes_only_dword_equality() {
        let dword_eq = normalize_condition(Condition {
            index: 1,
            operator: CompareOperator::Eq,
            value: 7,
            value_length: ArgumentLength::Dword,
        });
        assert_eq!(
            dword_eq.operator,
            NormalizedOperator::MaskedEq(0x0000_0000_ffff_ffff)
        );

        let dword_ne = normalize_condition(Condition {
            index: 1,
            operator: CompareOperator::Ne,
            value: 7,
            value_length: ArgumentLength::Dword,
        });
        assert_eq!(dword_ne.operator, NormalizedOperator::Ne);

        let qword_eq = normalize_condition(Condition {
            index: 1,
            operator: CompareOperator::Eq,
            value: 7,
            value_length: ArgumentLength::Qword,
        });
        assert_eq!(qword_eq.operator, NormalizedOperator::Eq);
    }

    #[test]
    fn action_encodings_match_linux_seccomp() {
        assert_eq!(Action::Allow.encode(), 0x7fff_0000);
        assert_eq!(Action::Errno(42).encode(), 0x0005_002a);
        assert_eq!(Action::KillThread.encode(), 0x0000_0000);
        assert_eq!(Action::KillProcess.encode(), 0x8000_0000);
        assert_eq!(Action::Log.encode(), 0x7ffc_0000);
        assert_eq!(Action::Trace(42).encode(), 0x7ff0_002a);
        assert_eq!(Action::Trap.encode(), 0x0003_0000);
    }
}
