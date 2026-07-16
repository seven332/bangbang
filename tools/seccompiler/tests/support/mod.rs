#![allow(
    dead_code,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use bangbang_seccompiler::CompiledFilters;

pub const AUDIT_ARCH_X86_64: u32 = 0xc000_003e;
pub const AUDIT_ARCH_AARCH64: u32 = 0xc000_00b7;

pub const ACTION_ALLOW: u32 = 0x7fff_0000;
pub const ACTION_KILL_THREAD: u32 = 0x0000_0000;
pub const ACTION_KILL_PROCESS: u32 = 0x8000_0000;
pub const ACTION_TRAP: u32 = 0x0003_0000;

const BPF_LD_W_ABS: u16 = 0x20;
const BPF_ALU_AND_K: u16 = 0x54;
const BPF_JMP_JA: u16 = 0x05;
const BPF_JMP_JEQ_K: u16 = 0x15;
const BPF_JMP_JGT_K: u16 = 0x25;
const BPF_JMP_JGE_K: u16 = 0x35;
const BPF_RET_K: u16 = 0x06;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Instruction {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

impl Instruction {
    fn decode(word: u64) -> Self {
        Self {
            code: word as u16,
            jt: (word >> 16) as u8,
            jf: (word >> 24) as u8,
            k: (word >> 32) as u32,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeccompData {
    pub nr: u32,
    pub arch: u32,
    pub args: [u64; 6],
}

impl SeccompData {
    pub const fn new(nr: u32, arch: u32) -> Self {
        Self {
            nr,
            arch,
            args: [0; 6],
        }
    }

    pub fn with_arg(mut self, index: usize, value: u64) -> Self {
        self.args[index] = value;
        self
    }

    fn load_word(self, offset: u32) -> u32 {
        match offset {
            0 => self.nr,
            4 => self.arch,
            16..=60 if offset.is_multiple_of(4) => {
                let relative = offset - 16;
                let argument = usize::try_from(relative / 8).expect("argument index should fit");
                let value = self.args[argument];
                if relative.is_multiple_of(8) {
                    value as u32
                } else {
                    (value >> 32) as u32
                }
            }
            _ => panic!("interpreter received an invalid load offset"),
        }
    }
}

pub fn program<'a>(filters: &'a CompiledFilters, category: &str) -> &'a [u64] {
    filters
        .get(category)
        .map(Vec::as_slice)
        .expect("compiled category should exist")
}

/// Test-only classic-BPF interpreter independent of production lowering.
pub fn execute(program: &[u64], data: SeccompData) -> u32 {
    let mut accumulator = 0_u32;
    let mut pc = 0_usize;
    let mut steps = 0_usize;

    loop {
        assert!(steps <= program.len(), "program did not terminate");
        steps += 1;
        let instruction = Instruction::decode(program[pc]);
        match instruction.code {
            BPF_LD_W_ABS => {
                accumulator = data.load_word(instruction.k);
                pc += 1;
            }
            BPF_ALU_AND_K => {
                accumulator &= instruction.k;
                pc += 1;
            }
            BPF_JMP_JA => {
                pc += 1 + usize::try_from(instruction.k).expect("jump should fit");
            }
            BPF_JMP_JEQ_K | BPF_JMP_JGT_K | BPF_JMP_JGE_K => {
                let matched = match instruction.code {
                    BPF_JMP_JEQ_K => accumulator == instruction.k,
                    BPF_JMP_JGT_K => accumulator > instruction.k,
                    BPF_JMP_JGE_K => accumulator >= instruction.k,
                    _ => false,
                };
                let offset = if matched {
                    instruction.jt
                } else {
                    instruction.jf
                };
                pc += 1 + usize::from(offset);
            }
            BPF_RET_K => return instruction.k,
            _ => panic!("interpreter received an unsupported opcode"),
        }
        assert!(pc < program.len(), "program jumped out of range");
    }
}

pub fn policy(filter: &str) -> String {
    format!(r#"{{"vmm":{filter},"api":{filter},"vcpu":{filter}}}"#)
}

pub fn filter(default_action: &str, filter_action: &str, rules: &str) -> String {
    format!(
        r#"{{"default_action":{default_action},"filter_action":{filter_action},"filter":{rules}}}"#
    )
}
