use std::collections::BTreeSet;

use crate::{CompileError, TargetArch};

const DATA: &str = include_str!("../data/libseccomp-v2.6.0-syscalls.csv");
const SPDX_LINE: &str = "# SPDX-License-Identifier: LGPL-2.1-or-later";
const SOURCE_LINE: &str = "# Source: libseccomp v2.6.0 src/syscalls.csv";
const COMMIT_LINE: &str = "# Source commit: c7c0caed1d04292500ed4b9bb386566053eb9775";
const CHECKSUM_LINE: &str =
    "# Source SHA-256: 3fc607fffc9c3b0aca77fd6ffc3aa0f86c61b90dc255baedfc396e9a5e102fdc";
const HEADER: &str = "name,x86_64,aarch64";
const EXPECTED_ROWS: usize = 502;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Resolution {
    Number(u32),
    Pnr,
}

#[derive(Debug)]
struct Entry {
    name: &'static str,
    x86_64: Resolution,
    aarch64: Resolution,
}

/// Checked libseccomp v2.6.0 syscall-name table for the two supported targets.
#[derive(Debug)]
pub(crate) struct SyscallTable {
    entries: Vec<Entry>,
}

impl SyscallTable {
    pub(crate) fn new() -> Result<Self, CompileError> {
        Self::parse(DATA)
    }

    fn parse(data: &'static str) -> Result<Self, CompileError> {
        let mut lines = data.lines();
        for expected in [SPDX_LINE, SOURCE_LINE, COMMIT_LINE, CHECKSUM_LINE, HEADER] {
            if lines.next() != Some(expected) {
                return Err(CompileError::InvalidEmbeddedSyscallTable);
            }
        }

        let mut entries = Vec::with_capacity(EXPECTED_ROWS);
        let mut previous_name: Option<&str> = None;
        let mut x86_numbers = BTreeSet::new();
        let mut aarch64_numbers = BTreeSet::new();
        let mut x86_numeric_count = 0;
        let mut aarch64_numeric_count = 0;

        for line in lines {
            let mut fields = line.split(',');
            let name = fields
                .next()
                .filter(|name| {
                    !name.is_empty()
                        && name.bytes().all(|byte| {
                            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                        })
                })
                .ok_or(CompileError::InvalidEmbeddedSyscallTable)?;
            let x86_64 = parse_resolution(
                fields
                    .next()
                    .ok_or(CompileError::InvalidEmbeddedSyscallTable)?,
            )?;
            let aarch64 = parse_resolution(
                fields
                    .next()
                    .ok_or(CompileError::InvalidEmbeddedSyscallTable)?,
            )?;
            if fields.next().is_some() || previous_name.is_some_and(|previous| previous >= name) {
                return Err(CompileError::InvalidEmbeddedSyscallTable);
            }

            if let Resolution::Number(number) = x86_64 {
                if !x86_numbers.insert(number) {
                    return Err(CompileError::InvalidEmbeddedSyscallTable);
                }
                x86_numeric_count += 1;
            }
            if let Resolution::Number(number) = aarch64 {
                if !aarch64_numbers.insert(number) {
                    return Err(CompileError::InvalidEmbeddedSyscallTable);
                }
                aarch64_numeric_count += 1;
            }

            entries.push(Entry {
                name,
                x86_64,
                aarch64,
            });
            previous_name = Some(name);
        }

        if entries.len() != EXPECTED_ROWS
            || x86_numeric_count != 379
            || aarch64_numeric_count != 322
        {
            return Err(CompileError::InvalidEmbeddedSyscallTable);
        }

        Ok(Self { entries })
    }

    pub(crate) fn resolve(&self, name: &str, arch: TargetArch) -> Option<Resolution> {
        let index = self
            .entries
            .binary_search_by(|entry| entry.name.cmp(name))
            .ok()?;
        self.entries.get(index).map(|entry| match arch {
            TargetArch::X86_64 => entry.x86_64,
            TargetArch::Aarch64 => entry.aarch64,
        })
    }
}

fn parse_resolution(value: &str) -> Result<Resolution, CompileError> {
    if value == "PNR" {
        Ok(Resolution::Pnr)
    } else {
        value
            .parse::<u32>()
            .map(Resolution::Number)
            .map_err(|_error| CompileError::InvalidEmbeddedSyscallTable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_table_has_exact_counts_and_sentinels() {
        let table = SyscallTable::new().expect("embedded table should validate");
        assert_eq!(table.entries.len(), 502);
        assert_eq!(
            table.resolve("accept", TargetArch::X86_64),
            Some(Resolution::Number(43))
        );
        assert_eq!(
            table.resolve("accept", TargetArch::Aarch64),
            Some(Resolution::Number(202))
        );
        assert_eq!(
            table.resolve("close", TargetArch::X86_64),
            Some(Resolution::Number(3))
        );
        assert_eq!(
            table.resolve("close", TargetArch::Aarch64),
            Some(Resolution::Number(57))
        );
        assert_eq!(
            table.resolve("access", TargetArch::Aarch64),
            Some(Resolution::Pnr)
        );
        assert_eq!(table.resolve("private", TargetArch::X86_64), None);

        let x86_pnr = table
            .entries
            .iter()
            .filter(|entry| entry.x86_64 == Resolution::Pnr)
            .count();
        let aarch64_pnr = table
            .entries
            .iter()
            .filter(|entry| entry.aarch64 == Resolution::Pnr)
            .count();
        assert_eq!(x86_pnr, 123);
        assert_eq!(aarch64_pnr, 180);
    }

    #[test]
    fn malformed_tables_fail_without_retaining_values() {
        for malformed in [
            "",
            "# private",
            concat!(
                "# SPDX-License-Identifier: LGPL-2.1-or-later\n",
                "# Source: libseccomp v2.6.0 src/syscalls.csv\n",
                "# Source commit: c7c0caed1d04292500ed4b9bb386566053eb9775\n",
                "# Source SHA-256: 3fc607fffc9c3b0aca77fd6ffc3aa0f86c61b90dc255baedfc396e9a5e102fdc\n",
                "name,x86_64,aarch64\nprivate,not-a-number,1\n"
            ),
        ] {
            let error = SyscallTable::parse(malformed).unwrap_err();
            assert_eq!(error, CompileError::InvalidEmbeddedSyscallTable);
            assert!(!error.to_string().contains("private"));
            assert!(!format!("{error:?}").contains("private"));
        }
    }
}
