# Firecracker v1.16.0 CPU-template contract

This document is the human-owned contract for the reviewed arm64 CPU-template
profile delivered by issues #1393, #1402, and #1403. It is pinned to
Firecracker v1.16.0 commit
`d83d72b710361a10294480131377b1b00b163af8` and to the public
Hypervisor.framework surface available to the macOS Apple Silicon backend.

The implementation deliberately separates three outcomes:

- exact expert-controlled masks are implemented and verified for eleven arm64
  identification registers, ACTLR.EnTSO, and the reviewed core and SIMD/FP
  profile;
- KVM capability numbers and `kvm_vcpu_init.features` words have no
  identity-preserving HVF namespace and receive stable platform faults; and
- static CPU names are configuration policy, not aliases for arbitrary live
  writes. `V1N1` remains pending configuration but cannot execute because its
  documented Neoverse V1 source-model contract is not true on Apple Silicon.

The ARM modifier properties and schemas now have a complete finite policy.
Multi-architecture operation/path aggregates, CPU-template corpora, and helper
tools remain nonterminal because their x86 and public dump/strip/verify/
fingerprint contracts are independent Wave 7 work.

## Request model and bounds

`PUT /cpu-config` accepts Firecracker's aarch64 `kvm_capabilities`,
`reg_modifiers`, and `vcpu_features` arrays. Missing arrays default to empty.
Each array has a persistent maximum of 256 entries, in addition to the normal
HTTP/config-file byte limits. Input order is preserved across the API/runtime
action boundary.

- KVM capabilities retain exact add/remove direction and the complete `u32`
  capability number. The optional `!` prefix means remove.
- ARM one-register entries require a KVM arm64 register identity whose encoded
  width is exactly 32, 64, or 128 bits. Their bitmap accepts `0`, `1`, `x`, and
  `_`: zero and one select a filtered target bit, while `x` preserves the
  baseline bit.
- vCPU feature entries retain an exact `u32` index plus 32-bit filter/value.
  The valid fixed KVM feature-word domain is index `0..7`.

The parser rejects unknown or duplicate JSON fields, malformed numeric or
bitmap strings, non-arm64/invalid-width register identities, over-width
bitmaps, value bits outside the filter, more than 256 entries in any array,
duplicate capability numbers regardless of add/remove direction, duplicate
register identities, and duplicate feature indexes. These duplicate/index
checks intentionally fail earlier and more strictly than upstream's eventual
KVM behavior.

All input and executable aggregates have manual value-redacted `Debug`
implementations. Structurally valid but unavailable KVM-only categories cross
the runtime action boundary only long enough to return their fixed platform
classification; they are never installed as effective controller state.

## Executable custom subset

Bangbang implements exact one-register semantics
`target = (baseline & !filter) | value` for this finite profile:

- U64 core registers X0 and X4-X30, SP_EL0, PC, PSTATE/CPSR, SP_EL1,
  ELR_EL1, and SPSR_EL1;
- U128 Q0-Q31, interpreted explicitly with little-endian integer/byte
  conversion; and
- U32 FPCR and FPSR, transported through HVF's scalar U64 API only when the
  complete observed value fits U32.

The accepted KVM core low indices (the `kvm_regs` byte offset divided by four)
are X0/X4-X30 at `0` and `8..=60` with stride 2, SP_EL0/PC/PSTATE at
`62/64/66`, SP_EL1/ELR_EL1/SPSR_EL1 at `68/70/72`, Q0-Q31 at `84..=208` with
stride 4, and FPSR/FPCR at `212/213`. No padding or intervening index inherits
the policy of a neighboring field.

The U64 system-register profile is closed and exact:

- `ID_AA64PFR0_EL1`;
- `ID_AA64PFR1_EL1`;
- `ID_AA64DFR0_EL1`;
- `ID_AA64DFR1_EL1`;
- `ID_AA64ISAR0_EL1`;
- `ID_AA64ISAR1_EL1`;
- `ID_AA64MMFR0_EL1`;
- `ID_AA64MMFR1_EL1`;
- `ID_AA64MMFR2_EL1`;
- `ID_AA64ZFR0_EL1`;
- `ID_AA64SMFR0_EL1`; and
- `ACTLR_EL1`, only when the modifier filter is a subset of EnTSO bit 1.

ZFR0 and SMFR0 require the public macOS 15.2 register boundary. A tiny
target-only C `__builtin_available` query runs while the complete typed template
is prepared, before VM or topology creation; absence produces one stable
value-free fault and no member access or write. ACTLR is an explicit macOS 15
tier and admits only the public SDK-documented EnTSO bit. A zero filter remains
a valid observable no-op for every accepted identity.

Any ordered combination of distinct accepted identities is valid, including a
single-register or mixed-width template. X1-X3 receive a boot-reserved fault;
the AArch32 banked SPSR_ABT/UND/IRQ/FIQ fields receive an unavailable-state
fault. MIDR and MPIDR/topology state, CPACR and boot/dependency controls,
translation and exception state, thread/context and cache selection, pointer
authentication keys, debug/trap state, timers, GIC/ICC state, optional mutable
SME state, and disabled EL2 state each have a stable category-only safety
fault. KVM demux/CCSIDR, firmware, firmware-feature, canonical SVE, and unknown
coprocessor classes have distinct platform classifications. Padding, reserved
or invalid class fields, wrong widths, semantic aliases, and unnamed system
encodings fail before effective state replacement. No raw `hv_sys_reg_t`
constructor or catch-all system-register path exists.

Each width computes the mask relation in its own integer type. Q reads use
`u128::from_le_bytes` and writes use `u128::to_le_bytes`; host-native byte
layout is never inferred. FPCR/FPSR reads with any nonzero transport bits above
U32 fail closed before template writes, and U32 targets are written only as
zero-extended values.

This is an expert-controlled mechanism, matching Firecracker's warning that an
incorrect custom template can crash a guest or expose an incoherent/insecure
feature view. The allowlist and exact readback prove mechanical application;
they do not certify instruction compatibility, monotonic feature reduction,
an N1 identity, or cross-host portability.

## Replacement and serialization

CPU selection is one transactional effective choice:

- a successful custom PUT replaces any prior custom template or pending static
  selection;
- an empty custom PUT succeeds and clears either selection;
- a valid machine `V1N1` update replaces custom state and remains pending;
- explicit machine `None` clears either selection; and
- omitted or JSON `null` machine fields preserve the current selection.

Complete candidate validation precedes every replacement. A malformed,
unsupported, or otherwise rejected candidate leaves both machine and custom
state unchanged. Post-start requests retain the normal unsupported-state
precedence.

Pending static `V1N1` is visible as `cpu_template: "V1N1"` from
`GET /machine-config` and in the machine section of `GET /vm/config`. Custom
contents are deliberately omitted and serialize as no static selection, as in
Firecracker. Config files retain machine-then-custom action order, so a valid
custom section can replace pending `V1N1` before start.

The x86 static names `C3`, `T2`, `T2S`, `T2CL`, and `T2A` are rejected during
machine candidate validation as foreign AWS/Linux policies. `V1N1` is accepted
as pending configuration, but an effective selection fails `InstanceStart`
before the startup executor or HVF VM construction. This finite writable
profile cannot establish Firecracker's documented Neoverse V1-to-N1 source
contract or its complete unmasked identity on Apple Silicon.

## HVF startup and failure atomicity

The backend maps the validated runtime template before creating a VM. After
the complete fixed vCPU topology and MPIDRs exist, but before guest resources,
memory, or PC/X0/PSTATE boot overrides are installed, it performs one bounded
template transaction:

1. every owner thread reads every requested/admitted register, with no access
   to unrelated allowlisted identities;
2. all reads on all vCPUs complete before the first write;
3. every vCPU must report the same requested baseline vector;
4. ordered targets are computed once from that common baseline; and
5. every owner writes each target through its typed general, system, scalar
   FP, or SIMD operation and immediately rereads it for exact equality before
   moving to the next target.

X0, PC, and PSTATE participate fully in that all-vCPU transaction, then the
ordinary primary Linux boot setup overwrites X0-X3, PC, and PSTATE/CPSR. The
secondary PSCI entry path likewise owns X0-X3, PC, and CPSR after clearing
SCTLR_EL1. X4-X30, the admitted core system registers, Q0-Q31, FPCR, and FPSR
are not changed by initial boot setup. The applied-then-overridden disposition
is explicit policy; it does not skip baseline comparison or exact readback.

The owner-thread command has conflict/retry admission and is unavailable after
the first vCPU run. Mapping, baseline read, baseline mismatch, write, reread,
or readback mismatch reports only a fixed stage/category, member position, and
completed count. Because live system-register writes are not rollback-safe,
any failure destroys the complete unpublished topology and VM; a partially
modified session is never returned or run.

## Snapshot boundary

An effective nonempty custom template is outside the native-v1 snapshot
profile. Snapshot create fails before capture/publication, and snapshot load
requires the existing pristine no-template profile. No CPU-template content is
serialized, no native-v1 schema or version changes, and no custom selection
survives a create/load boundary.

Pending `V1N1` cannot reach a running or paused snapshot source because its
start gate fires before backend construction. Empty custom or explicit `None`
leaves the ordinary no-template snapshot profile unchanged.

Wave 6 retains ownership of broader snapshot profiles and multi-vCPU/device
schemas. Wave 7 owns cross-host portability and the five public
`cpu-template-helper` commands and arguments.

## Security and signed evidence

Raw capability numbers, feature indexes, register identities, masks,
baselines, targets, and readbacks are absent from product `Debug`, `Display`,
HTTP faults, logs, metrics, and serial output. Stable architectural register
names appear only in tracked documentation and tests. The implementation uses
no private Apple API, new entitlement, root requirement, scheduler-affinity
inference, or physical-host model table.

Unit and failure-injection coverage proves bounded/lossless parsing,
replacement atomicity, every accepted identity and terminal family, the 15.2
availability outcomes, every forbidden ACTLR bit, mixed-width requested-set
read-before-write ordering, unrelated-register non-access, no-write baseline or
width mismatch, exact little-endian Q conversion, fail-closed FP transport,
every baseline/write/readback failure position, redaction, retry, and cleanup.
A separately signed two-vCPU lifecycle test captures a disposable in-memory
baseline, applies the seven additional ID registers plus ACTLR.EnTSO together
with the mixed ID/core/Q/FP profile, relies on mandatory all-member readback,
captures the primary pre-run state to prove boot precedence and retained
targets, and shuts both sessions down without emitting raw values. A signed
Linux SMP test applies X0/PC/PSTATE modifiers and reaches userspace on the
PSCI-started secondary, proving that its boot setup also supersedes those
targets. The
existing signed two-vCPU Linux test boots one baseline VM and one canonical
custom ID-register VM, pins a no-stdlib EL0 helper to each CPU, writes bounded
reports only to a scratch block device, and verifies the exact baseline mask
result. Serial receives fixed success/failure markers only.

The strict platform-exclusion evidence and alternatives for the seven narrow
KVM/static inventory leaves, plus the implementation and validation evidence
for the six completed ARM records, are recorded in `capabilities.json`.
Mechanical write/readback establishes routing, not a portable or coherent CPU
feature model; helper capture and comparison remain Wave 7 work.
