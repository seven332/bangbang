// Copyright 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Adapted in 2026 for bangbang's offline-only compiler. The seccomp-data
// layout, classic-BPF constants, and instruction representation originate in
// the former Firecracker/rust-vmm pure-Rust seccompiler backend. The label
// assembler and structural validator are new to this adaptation.

use crate::{CompileError, MAX_BPF_INSTRUCTIONS};

pub(crate) const BPF_LD_W_ABS: u16 = 0x20;
pub(crate) const BPF_ALU_AND_K: u16 = 0x54;
pub(crate) const BPF_JMP_JA: u16 = 0x05;
pub(crate) const BPF_JMP_JEQ_K: u16 = 0x15;
pub(crate) const BPF_JMP_JGT_K: u16 = 0x25;
pub(crate) const BPF_JMP_JGE_K: u16 = 0x35;
pub(crate) const BPF_RET_K: u16 = 0x06;

pub(crate) const SECCOMP_DATA_NR_OFFSET: u32 = 0;
pub(crate) const SECCOMP_DATA_ARCH_OFFSET: u32 = 4;
pub(crate) const SECCOMP_DATA_ARGS_OFFSET: u32 = 16;
pub(crate) const SECCOMP_DATA_ARG_SIZE: u32 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Instruction {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

impl Instruction {
    const fn statement(code: u16, k: u32) -> Self {
        Self {
            code,
            jt: 0,
            jf: 0,
            k,
        }
    }

    const fn conditional(code: u16, k: u32) -> Self {
        Self {
            code,
            jt: 0,
            jf: 1,
            k,
        }
    }

    /// Returns the numeric value whose little-endian bytes have Linux's
    /// `sock_filter { code, jt, jf, k }` layout.
    fn to_u64(self) -> u64 {
        u64::from(self.code)
            | (u64::from(self.jt) << 16)
            | (u64::from(self.jf) << 24)
            | (u64::from(self.k) << 32)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Label(usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Fixup {
    instruction: usize,
    label: Label,
}

/// Forward-only classic-BPF assembler with checked long-jump fixups.
#[derive(Debug, Default)]
pub(crate) struct Assembler {
    instructions: Vec<Instruction>,
    labels: Vec<Option<usize>>,
    fixups: Vec<Fixup>,
}

impl Assembler {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn new_label(&mut self) -> Label {
        let id = self.labels.len();
        self.labels.push(None);
        Label(id)
    }

    pub(crate) fn bind(&mut self, label: Label) -> Result<(), CompileError> {
        let position = self.instructions.len();
        let slot = self
            .labels
            .get_mut(label.0)
            .ok_or(CompileError::InvalidProgram)?;
        if slot.is_some() {
            return Err(CompileError::InvalidProgram);
        }
        *slot = Some(position);
        Ok(())
    }

    pub(crate) fn load(&mut self, offset: u32) {
        self.instructions
            .push(Instruction::statement(BPF_LD_W_ABS, offset));
    }

    pub(crate) fn bitwise_and(&mut self, mask: u32) {
        self.instructions
            .push(Instruction::statement(BPF_ALU_AND_K, mask));
    }

    pub(crate) fn return_action(&mut self, action: u32) {
        self.instructions
            .push(Instruction::statement(BPF_RET_K, action));
    }

    pub(crate) fn jump(&mut self, label: Label) {
        let instruction = self.instructions.len();
        self.instructions
            .push(Instruction::statement(BPF_JMP_JA, 0));
        self.fixups.push(Fixup { instruction, label });
    }

    /// Emits a comparison whose two one-byte branches select adjacent long
    /// jump trampolines. The labels may be arbitrarily far forward.
    pub(crate) fn branch(&mut self, code: u16, k: u32, if_true: Label, if_false: Label) {
        self.instructions.push(Instruction::conditional(code, k));
        self.jump(if_true);
        self.jump(if_false);
    }

    /// Emits the compact syscall-dispatch shape: equality falls into a long
    /// jump and inequality skips it to the next dispatch comparison.
    pub(crate) fn dispatch_equal(&mut self, k: u32, if_equal: Label) {
        self.instructions
            .push(Instruction::conditional(BPF_JMP_JEQ_K, k));
        let instruction = self.instructions.len();
        self.instructions
            .push(Instruction::statement(BPF_JMP_JA, 0));
        self.fixups.push(Fixup {
            instruction,
            label: if_equal,
        });
    }

    pub(crate) fn finish(mut self) -> Result<Vec<Instruction>, CompileError> {
        if self.instructions.len() > MAX_BPF_INSTRUCTIONS {
            return Err(CompileError::ProgramTooLarge);
        }

        for fixup in self.fixups {
            let target = self
                .labels
                .get(fixup.label.0)
                .and_then(|position| *position)
                .ok_or(CompileError::InvalidProgram)?;
            let next = fixup
                .instruction
                .checked_add(1)
                .ok_or(CompileError::InvalidProgram)?;
            let distance = target
                .checked_sub(next)
                .ok_or(CompileError::InvalidProgram)?;
            let distance =
                u32::try_from(distance).map_err(|_error| CompileError::InvalidProgram)?;
            let instruction = self
                .instructions
                .get_mut(fixup.instruction)
                .ok_or(CompileError::InvalidProgram)?;
            if instruction.code != BPF_JMP_JA {
                return Err(CompileError::InvalidProgram);
            }
            instruction.k = distance;
        }

        validate_program(&self.instructions)?;
        Ok(self.instructions)
    }
}

pub(crate) fn to_words(program: Vec<Instruction>) -> Vec<u64> {
    program.into_iter().map(Instruction::to_u64).collect()
}

fn validate_program(program: &[Instruction]) -> Result<(), CompileError> {
    if program.is_empty() {
        return Err(CompileError::InvalidProgram);
    }
    if program.len() > MAX_BPF_INSTRUCTIONS {
        return Err(CompileError::ProgramTooLarge);
    }

    for (index, instruction) in program.iter().enumerate() {
        match instruction.code {
            BPF_LD_W_ABS => {
                if instruction.jt != 0 || instruction.jf != 0 || !valid_load_offset(instruction.k) {
                    return Err(CompileError::InvalidProgram);
                }
            }
            BPF_ALU_AND_K | BPF_RET_K => {
                if instruction.jt != 0 || instruction.jf != 0 {
                    return Err(CompileError::InvalidProgram);
                }
            }
            BPF_JMP_JA => {
                if instruction.jt != 0 || instruction.jf != 0 {
                    return Err(CompileError::InvalidProgram);
                }
                checked_target(program.len(), index, instruction.k)?;
            }
            BPF_JMP_JEQ_K | BPF_JMP_JGT_K | BPF_JMP_JGE_K => {
                checked_target(program.len(), index, u32::from(instruction.jt))?;
                checked_target(program.len(), index, u32::from(instruction.jf))?;
            }
            _ => return Err(CompileError::InvalidProgram),
        }
    }

    let mut terminates = vec![false; program.len()];
    for index in (0..program.len()).rev() {
        let instruction = program.get(index).ok_or(CompileError::InvalidProgram)?;
        let terminates_here = match instruction.code {
            BPF_RET_K => true,
            BPF_LD_W_ABS | BPF_ALU_AND_K => termination_at(&terminates, index + 1)?,
            BPF_JMP_JA => {
                let target = checked_target(program.len(), index, instruction.k)?;
                termination_at(&terminates, target)?
            }
            BPF_JMP_JEQ_K | BPF_JMP_JGT_K | BPF_JMP_JGE_K => {
                let true_target = checked_target(program.len(), index, u32::from(instruction.jt))?;
                let false_target = checked_target(program.len(), index, u32::from(instruction.jf))?;
                termination_at(&terminates, true_target)?
                    && termination_at(&terminates, false_target)?
            }
            _ => return Err(CompileError::InvalidProgram),
        };
        let slot = terminates
            .get_mut(index)
            .ok_or(CompileError::InvalidProgram)?;
        *slot = terminates_here;
    }

    if terminates.iter().any(|terminates| !terminates) {
        return Err(CompileError::InvalidProgram);
    }
    Ok(())
}

fn valid_load_offset(offset: u32) -> bool {
    offset == SECCOMP_DATA_NR_OFFSET
        || offset == SECCOMP_DATA_ARCH_OFFSET
        || ((SECCOMP_DATA_ARGS_OFFSET..=60).contains(&offset) && offset.is_multiple_of(4))
}

fn checked_target(length: usize, index: usize, offset: u32) -> Result<usize, CompileError> {
    let offset = usize::try_from(offset).map_err(|_error| CompileError::InvalidProgram)?;
    let target = index
        .checked_add(1)
        .and_then(|next| next.checked_add(offset))
        .ok_or(CompileError::InvalidProgram)?;
    if target >= length {
        return Err(CompileError::InvalidProgram);
    }
    Ok(target)
}

fn termination_at(terminates: &[bool], index: usize) -> Result<bool, CompileError> {
    terminates
        .get(index)
        .copied()
        .ok_or(CompileError::InvalidProgram)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patches_long_forward_jumps() {
        let mut assembler = Assembler::new();
        let target = assembler.new_label();
        assembler.jump(target);
        for _ in 0..300 {
            assembler.load(SECCOMP_DATA_NR_OFFSET);
        }
        assembler.bind(target).expect("label should bind");
        assembler.return_action(0x7fff_0000);

        let program = assembler.finish().expect("program should validate");
        assert_eq!(program.first().map(|instruction| instruction.k), Some(300));
    }

    #[test]
    fn rejects_missing_duplicate_and_backward_labels() {
        let mut missing = Assembler::new();
        let label = missing.new_label();
        missing.jump(label);
        missing.return_action(0);
        assert_eq!(missing.finish(), Err(CompileError::InvalidProgram));

        let mut duplicate = Assembler::new();
        let label = duplicate.new_label();
        duplicate.bind(label).expect("first bind should work");
        assert_eq!(duplicate.bind(label), Err(CompileError::InvalidProgram));

        let mut backward = Assembler::new();
        let label = backward.new_label();
        backward.bind(label).expect("label should bind");
        backward.jump(label);
        backward.return_action(0);
        assert_eq!(backward.finish(), Err(CompileError::InvalidProgram));
    }

    #[test]
    fn validates_inclusive_kernel_length_limit() {
        let mut maximum = vec![
            Instruction::statement(BPF_LD_W_ABS, SECCOMP_DATA_NR_OFFSET);
            MAX_BPF_INSTRUCTIONS - 1
        ];
        maximum.push(Instruction::statement(BPF_RET_K, 0));
        assert_eq!(maximum.len(), MAX_BPF_INSTRUCTIONS);
        assert_eq!(validate_program(&maximum), Ok(()));

        let mut oversized = maximum;
        oversized.insert(
            oversized.len() - 1,
            Instruction::statement(BPF_LD_W_ABS, SECCOMP_DATA_NR_OFFSET),
        );
        assert_eq!(
            validate_program(&oversized),
            Err(CompileError::ProgramTooLarge)
        );
    }

    #[test]
    fn rejects_invalid_opcode_load_jump_and_fallthrough() {
        assert_eq!(
            validate_program(&[Instruction::statement(0xffff, 0)]),
            Err(CompileError::InvalidProgram)
        );
        assert_eq!(
            validate_program(&[
                Instruction::statement(BPF_LD_W_ABS, 12),
                Instruction::statement(BPF_RET_K, 0),
            ]),
            Err(CompileError::InvalidProgram)
        );
        assert_eq!(
            validate_program(&[Instruction::statement(BPF_JMP_JA, 1)]),
            Err(CompileError::InvalidProgram)
        );
        assert_eq!(
            validate_program(&[Instruction::statement(BPF_LD_W_ABS, SECCOMP_DATA_NR_OFFSET,)]),
            Err(CompileError::InvalidProgram)
        );
    }

    #[test]
    fn encodes_linux_sock_filter_as_explicit_little_endian_u64() {
        let instruction = Instruction {
            code: 0x1234,
            jt: 0x56,
            jf: 0x78,
            k: 0x9abc_def0,
        };
        assert_eq!(instruction.to_u64(), 0x9abc_def0_7856_1234);
        assert_eq!(
            instruction.to_u64().to_le_bytes(),
            [0x34, 0x12, 0x56, 0x78, 0xf0, 0xde, 0xbc, 0x9a,]
        );
    }
}
