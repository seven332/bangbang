# Snapshot Feasibility

This document records the current feasibility boundary for Firecracker-style
snapshot support on macOS with Hypervisor.framework. It is an implementation
roadmap, not a statement that snapshot create or restore is supported today.

## Current Status

bangbang recognizes Firecracker-shaped snapshot requests and inspection
commands, but does not create, load, read, write, or inspect snapshot files.

- `PUT /snapshot/create` and `PUT /snapshot/load` parse request bodies before
  reaching VMM action policy.
- Valid create requests are paused-state-only and valid load requests are
  pre-boot-only.
- Create requests currently return state-policy faults before startup and while
  running, then return the snapshot-specific unsupported fault only after state
  policy reaches a paused instance. Load requests return the snapshot-specific
  unsupported fault before startup and state-policy faults after startup.
- For a process-owned paused instance, create now crosses a scoped supervisor
  command-admission barrier before returning that unsupported fault. The
  lease-owned operation acknowledges quiescence from the block and entropy
  limiter retry schedulers, immediately releases them, and creates no files.
- `--snapshot-version` and `--describe-snapshot <PATH>` are recognized as
  first-class CLI commands, but fail before API socket publication or HVF
  startup because bangbang has no supported snapshot data format.

## Current Ownership and Pause Boundary

The current single-vCPU process keeps control-plane, run-loop, and HVF
ownership on separate threads:

| Owner | Live resources and responsibilities |
| --- | --- |
| Process owner | `ProcessVmm` owns the VMM controller, startup executor, and active `BootRunLoopSupervisor` handle. It serves API requests and commits public instance-state transitions, but it does not own the live boot session after startup. |
| Boot worker | The `bangbang-hvf-boot-loop` thread owns `ProcessHvfBootSession`, including packet I/O and `OwnedHvfArm64BootSession`. The latter owns mapped guest memory, the MMIO dispatcher and device resources, GIC metadata, metrics state, entropy state, and block and entropy retry schedulers. Device-update commands execute here. |
| vCPU runner | The `bangbang-hvf-vcpu` thread owns `HvfVcpuOwner`. `HvfVcpuRunner` serializes HVF operations through commands and can return immutable X0-X30, PC, and CPSR values; guest-visible MIDR, MPIDR, and baseline PFR/DFR/ISAR/MMFR compatibility metadata; optional macOS 15.2 ZFR0/SMFR0 SVE/SME compatibility metadata; mutable macOS 15.2 SME `PSTATE.SM`/`PSTATE.ZA` controls; conditional maximum-width macOS 15.2 streaming Z0-Z31 bytes, maximum-derived P0-P15 predicate bytes, a maximum-SVL-square ZA matrix, and fixed 64-byte SME2 ZT0 contents in separate debug-redacted values; raw macOS 15.2 SMCR_EL1, SMPRI_EL1, and TPIDR2_EL0 values in a debug-redacted value; raw macOS 15.2 SCXTNUM_EL0 and SCXTNUM_EL1 software context numbers in a debug-redacted value; raw SP_EL0, SP_EL1, ELR_EL1, and SPSR_EL1 values with paired ordered restore; raw AFSR0_EL1, AFSR1_EL1, ESR_EL1, FAR_EL1, PAR_EL1, and VBAR_EL1 values; raw ACTLR_EL1 and CPACR_EL1 values; raw CSSELR_EL1 cache-selection state; every DFR0-reported raw DBGBVR/DBGBCR hardware-breakpoint pair; every DFR0-reported raw DBGWVR/DBGWCR hardware-watchpoint pair; raw MDCCINT_EL1 and MDSCR_EL1 debug controls; raw Hypervisor.framework debug-exception and debug-register-access trap policy; raw SCTLR_EL1, TTBR0_EL1, TTBR1_EL1, TCR_EL1, MAIR_EL1, AMAIR_EL1, and CONTEXTIDR_EL1 values with paired ordered restore; raw TPIDR_EL0, TPIDRRO_EL0, and TPIDR_EL1 values with paired ordered restore; raw baseline Q0-Q31, FPCR, and FPSR values with paired ordered restore; raw APIA, APIB, APDA, APDB, and APGA pointer-authentication keys in a debug-redacted value; raw physical-timer CNTKCTL, control, CVAL, and TVAL values; raw virtual-timer mask, offset, control, and CVAL values; CPU-level IRQ/FIQ pending values; Hypervisor.framework's opaque GIC device-state bytes; or raw EL1 GIC ICC CPU-interface values through dedicated owner-thread commands. The snapshot barrier invokes none of these captures or restore operations, and the remaining architectural/device inventory is not implemented. |
| Auxiliary and host | Limiter retry threads retain deadlines and can request vCPU cancellation during ordinary running or paused operation. The snapshot barrier can temporarily quiesce the block and entropy schedulers. The vmnet interface, vsock listener, retained streams, and their host/kernel buffers remain open for the lifetime of the boot session. A transient vsock polling thread is joined at the end of each vCPU run step. |

A successful public pause has a narrower boundary than a snapshot needs:

1. `ProcessVmm` validates `Running` and asks the supervisor to pause.
2. The supervisor queues a pause command, wakes the run loop, and cancels an
   active HVF run.
3. The boot worker finishes the canceled step's pending wakeup dispatch, drains
   the command, records its worker status as `Paused`, closes the pause gate,
   and acknowledges the command.
4. Only after that acknowledgement does `ProcessVmm` commit the public state to
   `Paused`.

After the acknowledgement, the worker cannot enter another guest run-loop
window until resume. The pause gate still wakes to drain commands, however, so
this is not a frozen runtime boundary. In particular:

- memory-hotplug updates and status queries can execute on the boot worker while
  paused, and updates can mutate mapped guest memory and device state;
- MMDS put and patch actions can mutate process-owned shared state;
- block and entropy retry schedulers retain their deadlines and can set wakeup
  tokens or attempt vCPU cancellation;
- explicit paused commands remain admissible even though periodic metrics and
  balloon-stat scheduling are suppressed; and
- vmnet packet queues and vsock connections can change in host or kernel buffers
  even when bangbang is not dispatching them to the guest.

The current pause path does not capture vCPU, GIC, device, or guest-memory state
and does not transfer ownership of any live resource.

The HVF crate now has a narrower runner-local building block: one command reads
X0-X30, PC, and CPSR in architectural order on the owning thread and returns a
detached immutable value only after every read succeeds. A paired operation can
reapply that complete typed value on the same owner thread in X0-X30, PC, CPSR
order. Hypervisor.framework does not batch those 33 writes, so restore is
nontransactional: a typed failure identifies the failed register and number of
completed writes, after which the caller must retry the complete retained value
or discard the vCPU before execution. Generalized command-owned core-register
operation admission excludes runs, MMIO completion, boot setup, metadata,
timer, interrupt operations, cancellation, and shutdown until capture or
restore finishes, even when the caller abandons its response. Both boot-session
forms expose the operations. Public pause and snapshot-create/load paths invoke
neither, and this subset is not complete restorable vCPU state.

A second runner-local command reads raw `SP_EL0`, `SP_EL1`, `ELR_EL1`, and
`SPSR_EL1` values in that order and publishes one immutable value only after all
four reads succeed. A paired owner-thread operation writes the complete typed
value in the same order. Hypervisor.framework provides no four-write
transaction: a reusable typed system-register error identifies the failed
register and completed prefix, after which callers must retry the complete
value or discard the vCPU before execution. It shares a core-register admission
domain with the general-register commands and every capture, so no conflicting
runner operation can overlap it; command-owned admission survives response
abandonment and unwind. Borrowed and owned boot sessions delegate both
operations, but the supervisor lease and public snapshot paths invoke neither.
The subset still has no input validation, persistence, wider restore ordering,
or snapshot-schema meaning.

A separate core-register command reads raw `AFSR0_EL1`, `AFSR1_EL1`,
`ESR_EL1`, `FAR_EL1`, `PAR_EL1`, and `VBAR_EL1` in that order. It publishes
only after all six owner-thread reads succeed. A paired owner-thread operation
writes the complete typed value in the same order. The six SDK writes are
nontransactional and reuse the typed failed-register/completed-prefix error, so
callers must retry the complete value or discard the vCPU before execution.
Both commands share the same command-owned admission domain. Fault reports and
guest addresses are sensitive guest state; AFSR contents are implementation-
defined, and the value does not validate one coherent exception or include
vector-table memory. Both boot-session forms delegate capture and restore,
while the supervisor lease and public snapshot paths invoke neither. Signed
coverage writes an aligned unused VBAR, restores the actual captured value
twice, and takes no later guest exception; captured AFSR readback is preserved
without assuming that either field is writable.

A separate core-register command reads raw `ACTLR_EL1` then `CPACR_EL1` and
publishes only after both owner-thread reads succeed. A paired owner-thread
operation writes the complete typed value in the same order. The two SDK writes
are nontransactional and reuse the typed failed-register/completed-prefix error,
so callers must retry the complete value or discard the vCPU before execution.
Both commands share the same command-owned admission domain. Complete capture
and restore require macOS 15 because Hypervisor.framework exposes only
`ACTLR_EL1.EnTSO` there; CPACR can contain optional FP/SIMD/SVE/SME access
controls that this raw value does not validate. Both boot-session forms delegate
capture and restore, while the supervisor lease and public snapshot paths
invoke neither. The value has no writable-bit, destination-feature, guest ISB,
wider ordering, persistence, or snapshot-schema policy. Signed coverage sets
only EnTSO and baseline FPEN, executes ISB before HVC, then restores the actual
capture twice without post-restore guest execution.

A separate core-register command reads guest-visible `MIDR_EL1`, `MPIDR_EL1`,
`ID_AA64PFR0_EL1`, `ID_AA64PFR1_EL1`, `ID_AA64DFR0_EL1`,
`ID_AA64DFR1_EL1`, `ID_AA64ISAR0_EL1`, `ID_AA64ISAR1_EL1`,
`ID_AA64MMFR0_EL1`, `ID_AA64MMFR1_EL1`, and `ID_AA64MMFR2_EL1` in that
order. It publishes only after all eleven owner-thread reads succeed and shares
the core-register admission domain, including bidirectional exclusion with the
standalone MPIDR metadata getter. These values describe the virtual CPU/HVF
feature view, not physical-host identity or mutable restore state; bangbang sets
MPIDR affinity to zero. Both boot-session forms delegate capture, but the
supervisor lease and public snapshot paths do not invoke it. Newer beta-only
IDs, broader configuration-time feature manifests, feature masks, destination
policy, persistence, and schema remain deferred. Signed coverage compares two
captures and the MPIDR getter without hard-coding one Apple CPU model or inferring
portability.

A separate macOS 11+ configuration query creates a fresh default
`hv_vcpu_config_t`, reads raw `CTR_EL0`, `CLIDR_EL1`, then `DCZID_EL0`, and
releases the retained object before returning one immutable value. It takes no
VM/vCPU handle and remains outside runner admission, boot sessions, and public
snapshot paths. These feature registers are inputs to a later cache manifest,
not the live `CSSELR_EL1` selector, instruction/data `CCSIDR_EL1` geometry, a
destination decision, or restore policy. Signed coverage compares two pre-VM
queries with fixed messages and no raw-value logging.

Another macOS 11+ configuration query creates a fresh default object, reads all
eight raw data or unified `CCSIDR_EL1` values followed by all eight instruction
values, and releases the retained object before returning one immutable
geometry value. It also takes no VM/vCPU handle and remains outside runner
admission, boot sessions, and public snapshot paths. The feature and geometry
queries use independent objects and do not form one atomic cache manifest. The
raw arrays define no implemented-level selection, field interpretation, masks,
destination decision, selector synchronization, cache maintenance, or restore
policy. Signed coverage compares two pre-VM queries with fixed messages and no
raw-value logging.

A separate macOS 15.2+ core-register command reads guest-visible
`ID_AA64ZFR0_EL1` then `ID_AA64SMFR0_EL1` and publishes one optional SVE/SME
identification value only after both owner-thread reads succeed. It leaves the
eleven-register baseline capture unchanged, and both boot-session forms expose
it without involving the supervisor lease or public snapshot paths. These IDs
are compatibility metadata, not streaming execution state or mutable restore
state; broader configuration-time feature manifests, masks, destination policy,
persistence, and schema remain deferred. Signed coverage compares two idle-vCPU
captures without enabling SVE/SME, reading vector/predicate/matrix state,
executing the guest, hard-coding one model, or inferring portability.

A separate runtime-resolved macOS 15.2+ configuration query publishes the
maximum streaming vector length, in bytes, that guests may use. The SDK takes
no VM/vCPU handle, so the typed value is queried before VM creation and remains
outside runner admission and both boot-session forms. It is the conditional
Z-register allocation width, the basis for the conditional P-register width,
and each dimension of the conditional ZA allocation, not
the effective SVL selected through `SMCR_EL1`, feature or destination
compatibility policy, execution data, persistence, or a snapshot schema.
Missing symbols report the OS boundary and an available
symbol's exact `HV_UNSUPPORTED` result remains visible. Signed coverage compares
two successful same-host queries without logging the value, or accepts two
exact `HV_UNSUPPORTED` results.

A separate macOS 15.2+ core-state command runtime-resolves and calls
`hv_vcpu_get_sme_state` once on the owner thread, then publishes immutable
`PSTATE.SM` streaming-mode and `PSTATE.ZA` storage-enable flags only after the
call succeeds. Missing symbols return a structured older-macOS error, while an
available symbol's `HV_UNSUPPORTED` remains visible for SME-incapable hardware.
The flags are mutable execution controls, not identification metadata or the
conditionally present Z/P/ZA/ZT0 data. Both boot-session forms expose the
getter-only capture without involving the supervisor lease or public snapshot
paths. The command performs no maximum-SVL query; the separate configuration
value defines no setter, mode transition, persistence, schema, or restore
ordering. Signed coverage calls the getter twice on an idle vCPU without
assuming or logging values, changing PSTATE, reading SME data, or executing the
guest.

A nineteenth shared-core command conditionally captures streaming SVE Z0-Z31.
It first reads `PSTATE.SM` on the owner thread and returns a topical inactive
error before querying size or allocating when streaming mode is disabled. When
active, it queries the configuration-wide maximum SVL, validates and fallibly
allocates one contiguous `32 * maximum` buffer, then runtime-resolves the macOS
15.2+ `hv_vcpu_get_sme_z_reg` getter and fills exact maximum-width chunks in
architectural order. The typed value is published only after all 32 reads
succeed, exposes bounded slices, and redacts the complete buffer from `Debug`.
Both boot-session forms expose it, but the supervisor lease and public snapshot
paths do not invoke it. The maximum is an allocation width rather than effective
`SMCR_EL1.LEN`; P predicates, ZA, and ZT0 are captured separately. Setters and
transitions, layout interpretation,
feature/destination policy, protected persistence, schema, orchestration,
restore ordering, and multi-vCPU association remain deferred. Signed coverage
accepts only documented unavailability/inactivity or two complete equal idle-
vCPU captures without logging bytes or width, changing SME state, or executing
the guest.

A twentieth shared-core command conditionally captures streaming SVE P0-P15.
It first reads `PSTATE.SM` on the owner thread and returns the same topical
inactive error before querying size or allocating when streaming mode is
disabled. When active, it queries the configuration-wide maximum SVL, requires
that value to be non-zero and divisible by eight, fallibly allocates one
contiguous `16 * (maximum / 8)` buffer, then runtime-resolves the macOS 15.2+
`hv_vcpu_get_sme_p_reg` getter and fills exact predicate-width chunks in
architectural order. The typed value is published only after all 16 reads
succeed, exposes bounded slices, and redacts the complete buffer from `Debug`.
Both boot-session forms expose it, but the supervisor lease and public snapshot
paths do not invoke it. The maximum is an allocation basis rather than effective
`SMCR_EL1.LEN`; Z registers, ZA, and ZT0 are captured separately. Setters and
transitions, byte-layout and inactive-lane interpretation, feature/destination
policy, protected persistence, schema, orchestration, restore ordering, and
multi-vCPU association remain deferred. Signed coverage accepts only documented
unavailability/inactivity or two complete equal idle-vCPU captures without
logging bytes or widths, changing SME state, or executing the guest.

A twenty-first shared-core command conditionally captures the complete SME ZA
matrix. It first reads `PSTATE.ZA` on the owner thread and returns a topical
inactive error before querying size or allocating when ZA storage is disabled;
the SDK explicitly does not require `PSTATE.SM`. When active, it queries a
non-zero configuration-wide maximum SVL, checked-squares that byte count,
fallibly allocates the exact result, then runtime-resolves the macOS 15.2+
`hv_vcpu_get_sme_za_reg` getter for one complete read. The typed value is
published only on success, exposes the raw bytes without layout interpretation,
and redacts bytes and dimensions from `Debug`. Both boot-session forms expose
it, but the supervisor lease and public snapshot paths do not invoke it. The
maximum is an allocation dimension rather than effective `SMCR_EL1.LEN` or a
row/tile contract. ZT0 is captured separately. Setters and transitions, layout
interpretation, feature/destination policy, protected persistence, schema,
orchestration, restore ordering, and multi-vCPU association remain deferred.
Signed coverage
accepts only documented unavailability/inactivity or two complete equal idle-
vCPU captures without logging bytes or dimensions, changing SME state, or
executing the guest.

A twenty-second shared-core command conditionally captures the fixed 64-byte
SME2 ZT0 register. It first reads `PSTATE.ZA` on the owner thread and returns a
topical inactive error without a data read when ZA storage is disabled; the SDK
explicitly does not require `PSTATE.SM`. When active, it runtime-resolves the
macOS 15.2+ `hv_vcpu_get_sme_zt0_reg` getter and performs one read through a
private 64-byte, 16-byte-aligned SDK-compatible output value. It does not query
maximum SVL. The typed value is published only on success, exposes one fixed
array, and redacts every byte from `Debug`. Both boot-session forms expose it,
but the supervisor lease and public snapshot paths do not invoke it. Setters and
transitions, SME2 feature/destination policy, lane interpretation, protected
persistence, schema, orchestration, restore ordering, and multi-vCPU association
remain deferred. Signed coverage accepts only documented unavailability or
inactivity, or two complete equal idle-vCPU captures without
logging bytes, changing SME state, querying maximum SVL, or executing the guest.

A separate macOS 15.2+ core-register command reads raw `SMCR_EL1`,
`SMPRI_EL1`, and `TPIDR2_EL0` in that order and publishes one immutable value
only after all three owner-thread reads succeed. Because `TPIDR2_EL0` can hold
sensitive guest thread context, `Debug` redacts every register. Both boot-
session forms expose the getter-only capture without involving the supervisor
lease or public snapshot paths. It defines no writable-bit or feature
validation, maximum-SVL policy, persistence, schema, or restore ordering with
PSTATE and the conditionally present Z/P/ZA/ZT0 data. Signed coverage performs
two idle-vCPU captures without logging values, writes, data reads, or guest
execution.

A separate macOS 15.2+ core-register command reads raw `SCXTNUM_EL0` then
`SCXTNUM_EL1` and publishes one immutable value only after both owner-thread
reads succeed. These guest software context numbers can identify execution
contexts, so `Debug` redacts both values. Both boot-session forms expose the
getter-only capture without involving the supervisor lease or public snapshot
paths. It defines no interpretation, feature or destination validation,
persistence, schema, or restore ordering with TPIDR and `CONTEXTIDR_EL1` state.
Signed coverage performs two idle-vCPU captures without logging values, writes,
guest execution, reset assumptions, or compatibility inference.

A separate core-register command reads `ID_AA64DFR0_EL1`, derives the
architectural `BRPs + 1` implemented count, then reads each
`DBGBVR<n>_EL1` followed by `DBGBCR<n>_EL1` in ascending order. It exposes
only the implemented 1–16 prefix and publishes no state unless every read
succeeds. Breakpoint values can contain guest virtual addresses, Context IDs,
or VMIDs, and controls can describe enabled debug behavior, so the raw value is
sensitive. Both boot-session forms delegate this getter-only capture; it does
not write or enable debug state, change HVF trap policy, persist values, define
restore validation, or participate in the supervisor lease or public snapshot
paths. Signed coverage observes shape twice from an idle vCPU without guest
execution or model-specific reset assumptions.

A separate core-register command reads `ID_AA64DFR0_EL1`, derives the
architectural `WRPs + 1` implemented count, then reads each
`DBGWVR<n>_EL1` followed by `DBGWCR<n>_EL1` in ascending order. It exposes
only the implemented 1–16 prefix and publishes no state unless every read
succeeds. Watchpoint values contain guest data virtual addresses, and controls
can describe access type, byte selection, linking, and enabled debug behavior,
so the raw value is sensitive. Both boot-session forms delegate this getter-
only capture; it does not write or enable debug state, change HVF trap policy,
persist values, define restore validation, or participate in the supervisor
lease or public snapshot paths. Signed coverage observes shape twice from an
idle vCPU without guest execution or model-specific reset assumptions.

A separate core-state command calls Hypervisor.framework's debug-exception trap
getter followed by its debug-register-access trap getter and publishes the two
host policy booleans only after both owner-thread calls succeed. They correspond
to `MDCR_EL2.TDE` and `MDCR_EL2.TDA`, not guest EL1 debug-register contents.
Both boot-session forms delegate this getter-only capture; it does not call
either setter, change debug behavior, persist policy, define restore validation,
or participate in the supervisor lease or public snapshot paths. Signed
coverage observes both accessors twice from an idle vCPU without assuming,
comparing, or logging values or executing the guest.

A separate core-register command reads raw `SCTLR_EL1`, `TTBR0_EL1`,
`TTBR1_EL1`, `TCR_EL1`, `MAIR_EL1`, `AMAIR_EL1`, and `CONTEXTIDR_EL1` in that
order. It publishes only after all seven owner-thread reads succeed and shares
the same command-owned admission domain. Table bases and context ids are
sensitive guest state. A separate owner-thread operation accepts only the
complete typed value and writes all seven fields in capture order. The writes
are nontransactional and reuse the exact failed-system-register and completed-
prefix error, so failure requires a complete retry or vCPU discard before
execution. Both boot-session forms delegate capture and restore, while the
supervisor lease and public snapshot paths invoke neither. The value does not
include table memory, feature or destination validation, barriers, TLB/cache
maintenance, or a safe MMU transition sequence. Signed coverage leaves the MMU
disabled, preserves actual implementation-defined AMAIR readback, and restores
and recaptures the same complete value twice without later guest execution.

Another core-register command reads the low and high halves of APIA, APIB,
APDA, APDB, and APGA in that order and publishes five 128-bit keys only after
all ten owner-thread reads succeed. Pointer-authentication keys are
cryptographic secrets, so the detached value redacts all key material from
`Debug`; its named accessors are intended only for trusted internal composition.
It shares the core-register admission domain, and both
boot-session forms expose capture without involving the supervisor lease or
public snapshot paths. The value defines no feature/algorithm validation, memory
zeroization, protected persistence, enable ordering, restore, or schema policy.
Signed coverage uses visibly fake keys and never enables or executes PAC.

Another core-register command reads all 16 bytes of Q0-Q31 in ascending order,
then raw FPCR and FPSR, and publishes one immutable baseline SIMD/FP value only
after all 34 reads succeed. A separate owner-thread operation accepts only that
complete typed value and writes Q0-Q31, FPCR, then FPSR. The writes are
nontransactional; a dedicated typed error distinguishes the SIMD/FP and scalar
register spaces and reports the completed prefix, so failure requires a
complete retry or vCPU discard before execution. The SDK's by-value vector
setter crosses one macOS arm64 C shim because stable Rust cannot declare that
SIMD FFI; Rust passes only a pointer to 16 bytes. Capture and restore share
command-owned admission with the general,
core-system, exception, execution-control, cache-selection, breakpoint,
watchpoint, debug-control, debug-trap, baseline identification, optional SVE/SME
identification, SME PSTATE, SME Z-register, SME P-register, SME ZA-register,
SME ZT0-register, translation, pointer-authentication, thread-context restore,
SME system-register, and system-context operations. Both boot-session forms
expose capture and restore without involving the supervisor lease or public
snapshot paths.
Hypervisor.framework aliases Q registers to the
low 128 bits of Z registers in streaming SVE mode; this subset therefore defines
no ordering with wider Z contents, P predicates, the ZA matrix, or ZT0. Those
values use separate conditional capture commands, and none defines a restore or
snapshot-schema contract.
Signed coverage restores and recaptures the actual complete non-streaming
guest-written baseline value twice without a later guest run or raw-value log.

Another core-register command reads raw `TPIDR_EL0`, `TPIDRRO_EL0`, and
`TPIDR_EL1` in that order and publishes one immutable value only after all three
reads succeed. These software thread-ID fields can contain guest TLS or kernel
pointers. A separate owner-thread operation accepts only that complete typed
value and writes the three fields in capture order. The writes are
nontransactional and reuse the exact failed-system-register and completed-
prefix error, so failure requires a complete retry or vCPU discard before
execution. The capture and restore share admission with the general,
stack/exception-return, exception-report, execution-control, cache-selection,
breakpoint, watchpoint, debug-control, debug-trap, identification, translation,
SVE/SME identification, SME PSTATE, SME system-register, pointer-authentication,
SME Z-register, SME P-register, SME ZA-register, system-context, and SIMD/FP
operations and are exposed through both boot-session forms. `TPIDR2_EL0` is captured separately
with SME system registers, while `SCXTNUM_EL0`/`SCXTNUM_EL1` use a separate
system-context value. Address/destination validation, wider context ordering,
persistence, and schema remain outside this value. The supervisor lease and
public snapshot paths invoke neither operation.

A separate runner-local command captures raw `CNTKCTL_EL1`, `CNTP_CTL_EL0`,
`CNTP_CVAL_EL0`, and `CNTP_TVAL_EL0` in that order and publishes one immutable
value only after all four reads succeed. It shares generalized timer admission
with every virtual-timer getter, setter, and aggregate capture, and its command-
owned admission survives response abandonment and unwind.
Hypervisor.framework exposes the CNTP registers on macOS 15 and newer only when
the VM creates its GIC before the vCPU. The control ISTATUS bit is derived, CVAL
is an absolute comparator
against a continuing physical count, and the architecturally signed 32-bit TVAL
is a relative view returned as raw `u64`. TVAL changes while the sequential
CVAL/TVAL reads proceed, so this subset has no simultaneous-value guarantee,
portable elapsed-time adjustment, interrupt-delivery, writable-bit, or restore
policy.
Both boot-session forms delegate capture, while the supervisor lease and public
snapshot paths do not invoke it.

A separate runner-local command captures the HVF virtual-timer mask, raw offset,
raw `CNTV_CTL_EL0`, and raw `CNTV_CVAL_EL0` in that order and publishes one
immutable value only after all four reads succeed. It shares one serialized
timer admission domain with individual access to every captured field,
and its command-owned admission remains active when the caller drops its
response. The offset is the host-time-relative HVF value in
`CNTVCT_EL0 = mach_absolute_time() - offset`; the control register's ISTATUS bit
is derived and may change as virtual time advances. This narrow subset omits
GIC state and does not define a portable offset
adjustment or control-restore policy. Borrowed and owned boot sessions delegate
this capture, but the supervisor lease and public snapshot paths do not invoke
it.

A separate interrupt command captures the CPU-level IRQ then FIQ pending
injection values and publishes one immutable value only after both owner-thread
reads succeed. Individual IRQ/FIQ get/set commands and validated GIC PPI
set/clear commands share one generalized interrupt-operation admission domain,
while CPU levels and GIC state remain distinct models. HVF clears the CPU
pending levels after a vCPU run returns, so their setters are per-run injection
primitives rather than durable restore. Both boot-session forms delegate the
aggregate capture; the supervisor lease and public snapshot paths do not invoke
it. GIC device and EL1 ICC values are captured separately below, while their
persistence, compatible restore, and orchestration remain deferred.

Another command creates Hypervisor.framework's opaque GIC state object, queries
and fallibly allocates its reported size, copies the complete serialized GIC
device state except CPU system registers, and releases the retained object on
every outcome. Apple defines the bytes as stable and versioned, but restore can
still reject them after host software changes. The command shares generalized
interrupt admission with CPU pending operations and GIC PPI mutation. Running
it on the current single-vCPU owner loop serializes the stopped-VM requirement
against `hv_vcpu_run`; future multi-vCPU support needs a broader stop barrier.
Both boot-session forms delegate capture, while the supervisor lease and public
snapshot paths do not invoke it, and the command alone does not quiesce
device-side SPI producers. The value redacts its bytes from `Debug` and defines
no bangbang schema, persistence, parsing, or restore policy.

A companion command captures the ten EL1 ICC CPU-interface registers exposed
by Hypervisor.framework: PMR, BPR0, AP0R0, AP1R0, RPR, BPR1, CTLR, SRE,
IGRPEN0, and IGRPEN1. It reads every value on the vCPU owner thread, publishes
only after all reads succeed, and shares generalized interrupt admission with
CPU pending operations, GIC PPI mutation, and the opaque device-blob command.
The fixed value is per-vCPU and separate from the VM-scoped opaque blob. Both
boot-session forms delegate it, while the supervisor lease and public snapshot
paths invoke neither capture. `ICC_SRE_EL2`, ICH/ICV virtualization state,
multi-vCPU association, compatible restore ordering, and persistence remain
deferred.

Paused snapshot create now exercises the first lease-based ownership
foundation. A separate admission cell atomically reserves snapshot preparation
and submits an exclusive FIFO command. Commands admitted earlier execute first;
later ordinary commands, device updates, memory-hotplug mutations, and resume
reject before enqueue. The boot worker revalidates `Paused`, enters the scoped
lease, and acquires acknowledged quiescence guards from both limiter retry
schedulers. Acquisition waits for an already-started wakeup publication and
vCPU-cancel attempt to finish. Only after both schedulers acknowledge does the
worker drain any pending block or entropy retry token into deferred work. While
the guards are held, neither scheduler can publish another token or cancel
attempt. The guards are dropped before the supervisor lease releases, and
ordinary admission is restored before `SnapshotUnsupported` is returned.
Operation errors, queue/response closure, unwind, and repeated release restore
admission when recoverable. Shutdown invalidates it through the existing
out-of-band stop and pause-gate path.

Limiter deadlines remain absolute while quiesced. A due deadline or drained
token is republished asynchronously after guard release, duplicate immediate
work is coalesced, and a distinct future deadline is retained. Canceling a
scheduled retry clears both its deadline and deferred publication. Scheduler
stop is terminal and cannot be undone by a late guard drop.

This barrier does not change ordinary paused behavior outside its short scope
and does not quiesce periodic work, vmnet, vsock, other future auxiliary work,
vCPU state, devices, or guest memory. Block and entropy retry wakeups are the
only acknowledged auxiliary subset. It is therefore not a snapshot-ready
acknowledgement.

## Firecracker Requirements

Firecracker snapshots are more than a control-plane endpoint. A compatible
implementation has to coordinate these pieces:

- VM lifecycle: snapshot creation requires a paused microVM; loading a snapshot
  creates a paused microVM before optional resume.
- Guest memory: create writes a separate memory file; load maps or populates
  guest memory from a memory backend.
- VM and vCPU state: the VMM serializes VM state, vCPU state, and architecture
  state needed to resume execution.
- Device state: every emulated device that can exist at snapshot time needs a
  persisted and restored model state.
- Dirty tracking: diff snapshots depend on a dirty-page mechanism or another
  explicitly documented fallback.
- Host resources: disk files, network interfaces, and vsock backends remain
  user-managed resources outside the snapshot files.
- Data format: the state file has a versioned format; API compatibility alone
  does not imply on-disk Firecracker snapshot compatibility.

## HVF Feasibility

The inspected Xcode SDK Hypervisor.framework headers expose building blocks for
some of the required state:

- `hv_vm_map`, `hv_vm_unmap`, and `hv_vm_protect` can map current-process
  memory into guest physical address space and adjust permissions.
- Apple Silicon vCPU APIs expose general register, system register, SIMD/FP,
  SME, pending-interrupt, virtual-timer mask, and virtual-timer offset get/set
  operations. On macOS 15 and newer, physical-timer CNTP system registers are
  available only when a GIC is created before the vCPU.
- vCPU lifecycle and register APIs are thread-affine. bangbang also routes
  physical- and virtual-timer and pending-interrupt access through the owning
  runner thread as its serialization policy, so a future capture bundle has one
  explicit vCPU command boundary after the VM is quiesced.
- macOS 15 GIC APIs expose GICv3 distributor, redistributor, ICC, ICH, ICV, MSI,
  and SPI state access and interrupt injection primitives. They also expose a
  retained state object whose stable, versioned opaque bytes cover the GIC
  device except separately captured CPU system registers.

The inspected headers do not expose a KVM-style dirty log or dirty-page tracking
API. Firecracker-style diff snapshot parity is therefore not a direct HVF API
mapping. Later work must either prove another supported macOS mechanism, choose
software tracking for specific memory ranges, or document diff snapshots as a
platform-limited feature.

## Target Snapshot-Ready Ownership

The target design builds a full internal, exclusive quiescence lease on top of
the public `Paused` state. Its supervisor command-admission foundation is
implemented, but the complete prerequisite contract is not. None of its phases
is a new Firecracker-facing instance state.

The process owner requests preparation through the supervisor but does not take
the live session from its worker. The boot worker acquires, owns, and releases
the lease because it already owns guest memory and device dispatch. The vCPU
runner retains all thread-affine HVF access. A future snapshot operation may use
a bounded capture command while the lease is held, but command ordering alone
does not establish the lease because process-owner mutations and auxiliary
threads also need an admission boundary.

### Internal lifecycle

| Internal phase | Required behavior |
| --- | --- |
| Ordinary `Paused` | Today's pause acknowledgement has completed. Paused commands and the mutations listed above can still occur. |
| Supervisor preparing | Implemented for the scoped create barrier. Admission reservation and nonblocking FIFO submission share one lock, so earlier commands precede the barrier and later ordinary commands reject. The public controller remains `Paused`. |
| Supervisor leased | Implemented for one scoped boot-worker operation after worker-side pause revalidation. It closes ordinary supervisor command admission and acknowledges block and entropy limiter retry quiescence, but does not establish the remaining snapshot-ready invariants. |
| Snapshot-ready | Future phase after every in-process quiescence invariant below has been acknowledged. The lease remains held for state capture, and the public controller remains `Paused`. |
| Supervisor releasing | Implemented for scoped success, operation error, response closure, unwind, and shutdown invalidation. Recoverable release restores ordinary paused admission exactly once. |

The implemented supervisor barrier does not acknowledge snapshot readiness. A
later preparation path may do so only when all of these invariants hold:

- no vCPU run or MMIO completion is in flight, no new run can start, and the
  runner accepts only lease-authorized capture operations;
- no device dispatch or device-update command is active, and later mutating
  commands are rejected or deferred by an explicit admission policy;
- guest memory is stable except for access performed by the lease-owning capture
  path, including no memory-hotplug mutation;
- process-owner mutations that can affect captured state, including MMDS
  changes, are rejected or deferred; future work must classify genuinely
  read-only requests separately;
- periodic work is stopped and each retry scheduler has acknowledged quiescence,
  with no deadline thread able to publish another wakeup token; the current
  barrier satisfies this only for the block and entropy limiter retry
  schedulers while their guards are held;
- no VMM thread is reading or writing vmnet packets or vsock streams, and the
  transient vsock poller has joined;
- lease acquisition and capture are bounded or observe an out-of-band stop
  token, so shutdown does not depend on queueing a command behind lease-owned
  work or on the synchronous API requester making progress; and
- shutdown and terminal status are checked before readiness is returned, so a
  stale successful acknowledgement cannot outlive the session.

The vmnet and vsock invariant controls bangbang's access to external resources;
it does not freeze the host. Packets may accumulate in vmnet/kernel queues, and
peer activity may change socket buffers or connection state. Those resources
need an explicit metadata, discard, or reconnect policy during later restore
design. Live host descriptors and opaque kernel buffers are outside the guest
snapshot state unless a later design proves otherwise.

### Capture locality

| State or operation | Required owner |
| --- | --- |
| General, system, SIMD/FP, timer, pending-interrupt, and other vCPU-affine HVF state | Captured and restored by a dedicated serialized command on the vCPU runner thread. |
| HVF GIC state | Opaque device-only bytes are captured by a serialized runner command under the current single-vCPU stopped boundary. vCPU-affine CPU-interface registers remain a separate runner-owned inventory and must not be read directly by the process owner. |
| Guest memory and MMIO-device state | Inspected or copied on the boot worker while it holds the lease. |
| Limiter deadlines and other auxiliary scheduler state | Quiesced through an acknowledged handshake coordinated by the boot worker; the scheduler's own state owner supplies any captured fields. |
| API transaction and detached captured-state bundle | Coordinated by the process owner only after snapshot readiness is acknowledged. It may own an immutable captured bundle, but never the live boot session or runner-owned HVF handles. |
| vmnet, vsock, disks, and other host resources | Represented by explicit configuration or restore metadata according to later resource policy, not by serializing live host handles. |

Exact register inventories, GIC and device schemas, guest-memory file layout,
snapshot format, dirty tracking, and the duration of the lease during file I/O
remain separate design decisions.

### Failure and terminal precedence

- Preparation or capture failure must cancel lease-owned work, restore every
  successfully quiesced scheduler and admission gate, and return to coherent
  ordinary `Paused` behavior before reporting a recoverable error. If rollback
  cannot establish that boundary, the worker must become terminal rather than
  claim ordinary pause or snapshot readiness.
- Resume cannot start a guest run while preparing, snapshot-ready, or releasing.
  It must first cancel or finish capture and receive the exactly-once lease
  release acknowledgement, then use the existing paused-to-running transition.
- Process shutdown takes precedence over preparation and capture. It cancels
  lease work through an out-of-band control path, rather than queueing behind
  that work or relying on a blocked API requester. It prevents a later readiness
  acknowledgement and leaves the existing session owner responsible for
  stopping schedulers, shutting down the runner, destroying the VM, and joining
  the worker exactly once.
- A guest terminal outcome or worker failure that wins the race before pause or
  readiness acknowledgement invalidates the request. The process owner must not
  commit a stale state transition, and existing terminal process behavior
  remains authoritative.

## Required Prerequisites

Snapshot support should land only after these prerequisites are designed and
tested:

- Snapshot-ready pause ownership: extend the implemented supervisor admission
  foundation to satisfy every invariant above without racing the HVF runner,
  process-owner mutations, auxiliary wakeups, or terminal teardown.
- Guest-memory file model: bangbang needs explicit ownership, layout, copy or
  mapping rules, and failure behavior for memory snapshot files.
- HVF vCPU state capture: X0-X30, PC, and CPSR; raw SP_EL0, SP_EL1, ELR_EL1,
  and SPSR_EL1; raw AFSR0_EL1, AFSR1_EL1, ESR_EL1, FAR_EL1, PAR_EL1, and
  VBAR_EL1; raw ACTLR_EL1 and CPACR_EL1; raw CSSELR_EL1 cache selection; every
  DFR0-reported raw DBGBVR/DBGBCR hardware-breakpoint pair; every DFR0-reported
  raw DBGWVR/DBGWCR hardware-watchpoint pair;
  guest-visible MIDR, MPIDR, PFR0/1, DFR0/1, ISAR0/1, and MMFR0/1/2
  baseline compatibility metadata; optional macOS 15.2 ZFR0/SMFR0 SVE/SME
  compatibility metadata; mutable macOS 15.2 `PSTATE.SM`/`PSTATE.ZA` controls;
  conditional maximum-width macOS 15.2 streaming Z0-Z31 bytes;
  conditional maximum-derived macOS 15.2 streaming P0-P15 predicate bytes;
  conditional maximum-SVL-square macOS 15.2 ZA matrix bytes;
  raw macOS 15.2 `SMCR_EL1`, `SMPRI_EL1`, and `TPIDR2_EL0` state;
  raw macOS 15.2 `SCXTNUM_EL0` and `SCXTNUM_EL1` software context numbers;
  raw MDCCINT_EL1 and MDSCR_EL1 debug controls; raw
  Hypervisor.framework debug-exception and debug-register-access trap policy;
  raw TPIDR_EL0, TPIDRRO_EL0, and TPIDR_EL1; baseline Q0-Q31, FPCR, and FPSR; raw
  SCTLR_EL1, TTBR0_EL1, TTBR1_EL1, TCR_EL1, MAIR_EL1, AMAIR_EL1, and
  CONTEXTIDR_EL1; raw APIA, APIB, APDA, APDB, and APGA pointer-authentication
  keys; raw physical timer CNTKCTL, control, CVAL, and TVAL values; raw virtual
  timer mask, offset, control, and CVAL values; and CPU-level IRQ/FIQ pending
  values have owner-thread capture subsets.
  General, core-system, exception, execution-control, and baseline
  thread-context values also have isolated low-level owner-thread restore
  operations, without snapshot validation or orchestration.
  Identification metadata still needs masks and destination compatibility
  policy and is not mutable state to restore.
  SME PSTATE capture still needs maximum-SVL and feature validation plus
  destructive transition ordering with Z/P/FPSR and conditional ZA/ZT0 data;
  its raw flags must not be treated as safe restore input.
  SME Z-register capture still needs effective-SVL and feature/destination
  validation, protected persistence, byte-layout and zeroization policy, and
  coordinated transition/restore ordering with P/FPSR and conditional ZA/ZT0;
  its raw bytes must not be treated as safe restore input.
  SME P-register capture still needs effective-SVL and feature/destination
  validation, protected persistence, byte-layout, inactive-lane, and zeroization
  policy, and coordinated transition/restore ordering with Z/FPSR and
  conditional ZA/ZT0; its raw bytes must not be treated as safe restore input.
  SME ZA-register capture still needs effective-SVL and feature/destination
  validation, protected persistence, byte-layout and zeroization policy, and
  coordinated transition/restore ordering with Z/P/FPSR and conditional ZT0;
  its raw bytes must not be treated as safe restore input.
  SME ZT0-register capture still needs SME2 feature/destination validation,
  protected persistence, lane and zeroization policy, and coordinated
  transition/restore ordering with Z/P/ZA/FPSR; its raw bytes must not be
  treated as safe restore input.
  SME system-register capture still needs feature and writable-bit validation,
  maximum-SVL policy, protected persistence for sensitive `TPIDR2_EL0`, and
  ordered restore with PSTATE plus conditional Z/P/ZA/ZT0 data; its raw values
  must not be treated as safe restore input.
  System-context capture still needs interpretation, feature and destination
  validation, protected persistence, and ordered restore with TPIDR and
  `CONTEXTIDR_EL1` state; its raw values must not be treated as safe restore
  input.
  Hardware-breakpoint and hardware-watchpoint capture still need control-bit
  and destination-count validation, protected persistence, host trap
  coordination, and ordered restore. Debug-control and host debug-trap capture
  remain separate and lack feature/writable-bit validation, setter policy,
  security policy, and ordered restore; raw comparator, MDCCINT/MDSCR, and host
  trap values must not be treated as safe restore input.
  Cache-selection capture is not topology. Default-configuration
  CTR_EL0/CLIDR_EL1/DCZID_EL0 metadata and independent instruction/data CCSIDR
  geometry are queried separately, while feature/geometry interpretation and
  masks, selector validation, synchronization, maintenance, compatibility, and
  restore policy remain required.
  Remaining system registers and other
  optional architecture state still need a full inventory; the raw virtual-
  timer offset, absolute physical-timer comparator, and relative physical-timer
  value need explicit restore-time adjustment policies;
  derived ISTATUS observations are not control-restore contracts;
  pointer-authentication keys need feature validation, protected persistence,
  and safe enable ordering; and every remaining captured field still needs a
  restore path on the owning thread. The seven general-, core-system-,
  exception-register, execution-control, thread-context, translation, and
  baseline SIMD/FP primitives already supply only their isolated,
  nontransactional owner-thread write sequences; none is snapshot validation,
  wider ordering, rollback, feature/MMU/streaming transition, dependent-memory
  or maintenance coordination, or load orchestration.
- Interrupt-controller state: #1178 captures Apple's stable, versioned opaque
  GIC device blob except CPU system registers, and #1180 captures all ten EL1
  ICC registers exposed by the current SDK. `ICC_SRE_EL2`, ICH/ICV inventory,
  compatible restore ordering, host-update failure policy, multi-vCPU
  association, and a bangbang schema remain required before interrupt delivery
  can be considered restorable.
- Device-state persistence: every implemented device needs a stable serialized
  state model, restore validation, and rollback or terminal-failure behavior.
- Dirty tracking decision: full snapshots can be considered separately, but
  diff snapshots need an explicit HVF/macOS strategy.
- Data-format decision: bangbang must choose between Firecracker file-format
  compatibility, a bangbang-native format behind Firecracker-shaped APIs, or a
  documented unsupported boundary.
- Security policy: snapshot paths, memory contents, restored CPU state, and
  restored device state must be treated as untrusted input and must preserve the
  existing host-path redaction policy.

## Implementation Split

Snapshot-ready ownership should land as ordered, PR-sized slices before a
snapshot create success path. Each slice must preserve recognized unsupported
API behavior until all of its prerequisites exist.

| Slice | Scope | Minimum validation |
| --- | --- | --- |
| Supervisor lease and admission (foundation implemented) | #1160 adds atomic admission/FIFO ordering, worker-side pause revalidation, one scoped lease-owned operation, normal-command rejection, structured release, and out-of-band shutdown invalidation. Real capture work and admission across the remaining owners are deferred. | Supervisor and `ProcessVmm` unit tests plus API/process pause-state tests. |
| Auxiliary quiescence (block and entropy implemented) | #1162 adds acknowledged RAII quiescence for the existing block and entropy limiter retry schedulers, waits for in-flight publication, preserves absolute deadlines, drains and defers pending tokens, and keeps stop terminal. Periodic work and any later wakeup scheduler remain deferred. | Deterministic scheduler concurrency tests and supervisor lease-order tests; signed lifecycle coverage remains follow-up work. |
| Runner general-register capture and restore (first bidirectional subset implemented) | #1164 adds a typed immutable X0-X30, PC, and CPSR value plus one failure-atomic owner-thread capture. #1228 adds ordered owner-thread restore of that complete typed value and generalizes the shared admission name from capture to operation. Hypervisor.framework does not make the 33 writes transactional: typed failure context identifies the failed register and completed prefix, and callers must retry the complete value or discard the vCPU before execution. Both boot-session forms expose capture and restore, but the snapshot lease invokes neither. Core system, exception, execution-control, identification, translation, baseline SIMD/FP, schema, validation, rollback, wider ordering, and multi-vCPU coordination remain separate or deferred. | Exact 33-field read/write order; every read and write failure; typed partial-write context; complete retry; twenty-nine-way conflicts; abandonment, channels, queued destruction, unwind, panic, shutdown; and signed same-vCPU idle capture/restore/recapture without guest execution or value logging. |
| Runner core system-register capture and restore (second bidirectional subset implemented) | #1170 adds a typed immutable raw SP_EL0, SP_EL1, ELR_EL1, and SPSR_EL1 value plus one owner-thread capture. #1230 adds ordered owner-thread restore of that complete value and a reusable typed system-register failure with the exact failed register and completed prefix. Hypervisor.framework does not make the four writes transactional, so callers must retry the complete value or discard the vCPU before execution. Both boot-session forms expose capture and restore under shared core-operation admission, but the snapshot lease invokes neither. Exception, execution-control, identification, translation, broader system state, validation, schema, rollback, wider ordering, orchestration, and multi-vCPU coordination remain separate or deferred. | Exact four-field read/write order; every read and write failure; typed partial-write context; complete retry; twenty-nine-way conflicts; abandonment, channels, queued destruction, unwind, panic, shutdown; and signed guest-written known-value capture/restore/recapture without post-restore guest execution or value logging. |
| Runner EL1 exception-register capture and restore (third bidirectional subset implemented) | #1184 adds typed immutable raw AFSR0_EL1, AFSR1_EL1, ESR_EL1, FAR_EL1, PAR_EL1, and VBAR_EL1 state plus one owner-thread capture. #1232 adds ordered owner-thread restore of that complete value through the reusable typed system-register failure with the exact failed register and completed prefix. Hypervisor.framework does not make the six writes transactional, so callers must retry the complete value or discard the vCPU before execution. Both boot-session forms expose capture and restore under shared core-operation admission, but the snapshot lease invokes neither. Vector-table memory, coherent exception semantics, destination validation, persistence, schema, rollback, wider ordering, orchestration, and multi-vCPU coordination remain deferred. | Exact six-field read/write order; every read and write failure; typed partial-write context; complete retry; twenty-nine-way conflicts; abandonment, channels, queued destruction, unwind, panic, shutdown; and signed guest-written capture/restore/recapture preserving implementation-defined AFSR readback without post-restore guest execution or value logging. |
| Runner EL1 execution-control capture and restore (fourth bidirectional subset implemented) | #1186 adds typed immutable raw ACTLR_EL1 and CPACR_EL1 state plus one owner-thread capture. #1234 adds ordered owner-thread restore of that complete value through the reusable typed system-register failure with the exact failed register and completed prefix. Complete capture and restore require macOS 15 because Hypervisor.framework exposes only ACTLR_EL1.EnTSO there. The two writes are nontransactional, so callers must retry the complete value or discard the vCPU before execution. Both boot-session forms expose capture and restore under shared core-operation admission, but the snapshot lease invokes neither. CPACR optional-feature and destination validation, writable-bit policy, guest ISB transitions, wider feature-state ordering, persistence, schema, rollback, orchestration, and multi-vCPU coordination remain deferred. | Exact ACTLR-then-CPACR read/write order; both read and write failures; typed partial-write context; complete retry; twenty-nine-way conflicts; abandonment, channels, queued destruction, unwind, panic, shutdown; and signed EnTSO/FPEN capture/restore/recapture without post-restore guest execution or value logging. |
| Default arm64 vCPU cache feature configuration (raw prerequisite implemented) | #1216 adds a typed immutable raw CTR_EL0/CLIDR_EL1/DCZID_EL0 value queried from a fresh default retained vCPU configuration before VM creation. It remains outside backend instance state, VM/vCPU ownership, runner admission, boot sessions, and snapshot orchestration. CCSIDR geometry is queried separately; interpretation, masks, destination policy, persistence, schema, and restore remain deferred. | Exact macOS 11+ object/feature APIs and ids; null creation, CTR-then-CLIDR-then-DCZID order, arbitrary values, all getter failures, success/error/unwind release, target behavior, accessors, and signed same-host pre-VM stability without raw logging or cache operations. |
| Default arm64 vCPU CCSIDR geometry (raw prerequisite implemented) | #1218 adds a separate typed immutable pair of eight-entry raw data/unified and instruction CCSIDR arrays queried from its own fresh retained default vCPU configuration before VM creation. It remains outside backend instance state, VM/vCPU ownership, runner admission, boot sessions, and snapshot orchestration, and is not atomic with #1216. Implemented-level selection, interpretation, masks, destination policy, persistence, schema, and restore remain deferred. | Exact macOS 11+ object/CCSIDR API and cache types; null creation, data-then-instruction order, all sixteen arbitrary values, both getter failures, success/error/unwind release, target behavior, accessors, and signed same-host pre-VM stability without raw logging or live cache operations. |
| Runner EL1 cache-selection capture (raw subset implemented) | #1196 adds typed immutable raw CSSELR_EL1 state plus one getter-only, failure-atomic owner-thread command in the shared core-register admission domain. Both boot-session forms expose it without involving the snapshot lease or changing cache state. Default-configuration CTR/CLIDR/DCZID metadata and CCSIDR geometry are queried independently; selector validation, interpretation, synchronization, maintenance, persistence, schema, restore, and multi-vCPU association remain deferred. | Exact stable SDK id; one-read failure and fresh retry, twenty-nine-way conflicts, abandonment, command/response channel closure, queued destruction, unwind, panic, shutdown, and signed observation-only capture without writes, CCSIDR queries, maintenance, guest execution, or reset-value assumptions. |
| Runner EL1 hardware-breakpoint capture (raw subset implemented) | #1198 adds a typed immutable implemented count plus raw DBGBVR/DBGBCR prefixes, bounded indexed mappings for all sixteen SDK slots, and one getter-only, failure-atomic owner-thread command in the shared core-register admission domain. Both boot-session forms expose it without involving the snapshot lease or changing debug behavior. Watchpoints and host trap state are captured separately; control-bit validation, protected persistence, schema, restore, and multi-vCPU association remain deferred. | Exact indexed SDK ids; DFR0-first count policy; deterministic pair order, every failure point and fresh retry, twenty-nine-way conflicts, abandonment, command/response channel closure, queued destruction, unwind, panic, shutdown, and signed idle-vCPU shape capture without writes, debug activation, trap changes, guest instructions, or guest execution. |
| Runner EL1 hardware-watchpoint capture (raw subset implemented) | #1200 adds a typed immutable implemented count plus raw DBGWVR/DBGWCR prefixes, bounded indexed mappings for all sixteen SDK slots, and one getter-only, failure-atomic owner-thread command in the shared core-register admission domain. Both boot-session forms expose it without involving the snapshot lease or changing debug behavior. Breakpoints and host trap state are captured separately; control-bit validation, protected persistence, schema, restore, and multi-vCPU association remain deferred. | Exact indexed SDK ids; DFR0-first count policy; deterministic pair order, every failure point and fresh retry, twenty-nine-way conflicts, abandonment, command/response channel closure, queued destruction, unwind, panic, shutdown, and signed idle-vCPU shape capture without raw logging, writes, debug activation, trap changes, guest instructions, or guest execution. |
| Runner EL1 debug-control capture (raw subset implemented) | #1194 adds typed immutable raw MDCCINT_EL1 and MDSCR_EL1 state plus one getter-only, failure-atomic owner-thread command in the shared core-register admission domain. Both boot-session forms expose it without involving the snapshot lease or changing debug behavior. Breakpoint/watchpoint comparators and host trap state are captured separately; feature/security validation, persistence, schema, restore, and multi-vCPU association remain deferred. | Exact two stable SDK ids; MDCCINT-then-MDSCR order, both failure points and retry, twenty-nine-way conflicts, abandonment, command/response channel closure, queued destruction, unwind, panic, shutdown, and signed observation-only capture without debug activation or model constants. |
| Runner arm64 debug-trap policy capture (raw subset implemented) | #1202 adds a typed immutable pair of Hypervisor.framework debug-exception and debug-register-access trap booleans plus one getter-only, failure-atomic owner-thread command in the shared core-register admission domain. Both boot-session forms expose it without involving the snapshot lease, changing policy, or conflating host TDE/TDA-equivalent state with guest EL1 debug registers. Setters, feature/security validation, persistence, schema, restore ordering, and multi-vCPU association remain deferred. | Exact macOS 11+ owner-thread APIs and operation names; exception-then-register-access order, all Boolean combinations, both failure points and fresh retry, twenty-nine-way conflicts, abandonment, command/response channel closure, queued destruction, unwind, panic, shutdown, and signed idle-vCPU observation without value assumptions, logging, setters, debug activation, guest instructions, or guest execution. |
| Runner identification-register capture (compatibility metadata implemented) | #1192 adds typed immutable guest-visible MIDR, MPIDR, PFR0/1, DFR0/1, ISAR0/1, and MMFR0/1/2 baseline metadata plus one failure-atomic owner-thread command in the shared core-register admission domain. Both boot-session forms expose it without involving the snapshot lease. Optional SVE/SME IDs are captured separately; beta-only newer IDs, broader configuration-time manifests, feature masks, destination policy, persistence, schema, and multi-vCPU association remain deferred. | Exact eleven stable SDK ids; deterministic order, every failure point and retry, twenty-nine-way core-operation conflicts plus standalone metadata-getter exclusion, abandonment, channel, queued destruction, unwind, panic, shutdown, and signed same-vCPU stability/MPIDR comparison without model constants. |
| Runner SVE/SME identification-register capture (optional compatibility metadata implemented) | #1204 adds a separate typed immutable raw ZFR0/SMFR0 value plus one macOS 15.2+ failure-atomic owner-thread command in the shared core-register admission domain. The baseline identification value remains unchanged, and both boot-session forms expose the optional capture without involving the snapshot lease. SME PSTATE is captured separately; broader configuration-time manifests, masks, destination policy, streaming data, persistence, schema, restore, and multi-vCPU association remain deferred. | Exact two stable SDK ids and availability; ZFR0-then-SMFR0 order, both failure points and fresh retry, twenty-nine-way conflicts, abandonment, command/response channel closure, queued destruction, unwind, panic, shutdown, and signed same-vCPU stability without model constants, feature enablement, streaming mode, state reads, or guest execution. |
| SME maximum-SVL configuration query (buffer-sizing prerequisite implemented) | #1214 adds one runtime-resolved macOS 15.2+ no-handle query and a typed immutable maximum guest-usable SVL byte length. It remains outside backend instance state, VM/vCPU ownership, runner admission, boot sessions, and snapshot orchestration; #1220 consumes it as an exact per-Z allocation width, #1222 as the basis for each `maximum / 8` P-register width, and #1224 as both dimensions of the checked-square ZA allocation. Z/P require a live-vCPU streaming-mode preflight, whereas ZA requires its storage-enable preflight. ZT0 is independent of maximum SVL; effective SVL, feature/destination policy, persistence, schema, and restore remain deferred. | Exact C ABI and symbol/return behavior; full-width `size_t` preservation, missing-symbol and non-target boundaries, exact `HV_UNSUPPORTED`, typed value/accessor coverage, and a signed double query before VM creation without raw logging or SME state/data operations. |
| Runner SME PSTATE capture (raw subset implemented) | #1206 adds a separate typed immutable `PSTATE.SM`/`PSTATE.ZA` value plus one runtime-resolved macOS 15.2+ getter-only, failure-atomic owner-thread command in the shared core-register admission domain. Both boot-session forms expose it without involving the snapshot lease or calling the setter. Maximum SVL, Z0-Z31, P0-P15, ZA, and ZT0 are captured separately; feature validation, transition ordering, persistence, schema, restore, and multi-vCPU association remain deferred. | Exact C ABI layout and symbol/return behavior; all Boolean combinations, backend failure and fresh retry, twenty-nine-way conflicts, abandonment, command/response channel closure, queued destruction, unwind, panic, shutdown, and signed idle-vCPU observation or exact `HV_UNSUPPORTED` without logging, setters, state changes, SME data reads, guest instructions, or guest execution. |
| Runner SME Z-register capture (conditional raw subset implemented) | #1220 adds a runtime-resolved macOS 15.2+ getter-only command that preflights `PSTATE.SM`, queries maximum SVL, fallibly allocates one contiguous buffer, and publishes exact maximum-width Z0-Z31 slices only after every owner-thread read succeeds. `Debug` redacts the complete buffer, both boot-session forms expose it, and the snapshot lease does not invoke it. P0-P15, ZA, and ZT0 are captured separately; effective SVL, setters/transitions, feature/destination policy, layout conversion, protected persistence, schema, restore ordering, orchestration, and multi-vCPU association remain deferred. | Exact SDK ids/C ABI and availability; inactive/zero/overflow/allocation failures; deterministic 32-read order, every getter failure and fresh retry, bounded accessors, redaction, twenty-nine-way conflicts, abandonment, channel, queued destruction, unwind, panic, shutdown, and signed unavailable/inactive or two complete idle captures without raw logging, setters, state changes, guest instructions, or guest execution. |
| Runner SME P-register capture (conditional raw subset implemented) | #1222 adds a runtime-resolved macOS 15.2+ getter-only command that preflights `PSTATE.SM`, queries and validates maximum SVL, fallibly allocates one contiguous buffer, and publishes exact `maximum / 8`-byte P0-P15 slices only after every owner-thread read succeeds. `Debug` redacts the complete buffer, both boot-session forms expose it, and the snapshot lease does not invoke it. Z0-Z31, ZA, and ZT0 are captured separately; effective SVL, setters/transitions, feature/destination policy, layout and inactive-lane interpretation, protected persistence, schema, restore ordering, orchestration, and multi-vCPU association remain deferred. | Exact SDK ids/C ABI and availability; inactive/zero/divisibility/overflow/allocation failures; deterministic 16-read order, every getter failure and fresh retry, bounded accessors, redaction, twenty-nine-way conflicts, abandonment, channel, queued destruction, unwind, panic, shutdown, and signed unavailable/inactive or two complete idle captures without raw logging, setters, state changes, guest instructions, or guest execution. |
| Runner SME ZA-register capture (conditional raw subset implemented) | #1224 adds a runtime-resolved macOS 15.2+ getter-only command that preflights `PSTATE.ZA` without requiring `PSTATE.SM`, queries a non-zero maximum SVL, checked-squares it, fallibly allocates the exact buffer, and publishes the complete raw matrix only after the owner-thread getter succeeds. `Debug` redacts bytes and dimensions, both boot-session forms expose it, and the snapshot lease does not invoke it. Z/P/ZT0 are captured separately; effective SVL, setters/transitions, feature/destination policy, layout interpretation, protected persistence, schema, restore ordering, orchestration, and multi-vCPU association remain deferred. | Exact C ABI and availability; both streaming-mode values under active/inactive ZA; zero/overflow/allocation failures; exact bytes, backend failure and fresh retry, raw accessors, redaction, twenty-nine-way conflicts, abandonment, channel, queued destruction, unwind, panic, shutdown, and signed unavailable/inactive or two complete idle captures without raw logging, setters, state changes, guest instructions, or guest execution. |
| Runner SME2 ZT0-register capture (conditional raw subset implemented) | #1226 adds a runtime-resolved macOS 15.2+ getter-only command that preflights `PSTATE.ZA` without requiring `PSTATE.SM`, then performs one fixed 64-byte read through a private 16-byte-aligned SDK-compatible value without querying maximum SVL. The detached state is published only after success, redacts every byte from `Debug`, and is exposed by both boot-session forms without involving the snapshot lease. Z/P/ZA are captured separately; setters/transitions, SME2 feature/destination policy, lane interpretation, protected persistence, schema, restore ordering, orchestration, and multi-vCPU association remain deferred. | Exact SDK C ABI, 64-byte size and 16-byte alignment, missing-symbol/present-symbol behavior, both streaming-mode values under active/inactive ZA, exact bytes, backend failure and fresh retry, fixed-size accessor, redaction, twenty-nine-way conflicts, abandonment, channel, queued destruction, unwind, panic, shutdown, and signed unavailable/inactive or two complete idle captures without raw logging, setters, state changes, maximum-SVL queries, guest instructions, or guest execution. |
| Runner SME system-register capture (raw subset implemented) | #1208 adds a separate typed immutable raw SMCR_EL1, SMPRI_EL1, and TPIDR2_EL0 value plus one macOS 15.2+ getter-only, failure-atomic owner-thread command in the shared core-register admission domain. `Debug` redacts every register, and both boot-session forms expose capture without involving the snapshot lease. Maximum SVL, Z0-Z31, P0-P15, ZA, and ZT0 are captured separately; feature and writable-bit validation, persistence, schema, restore ordering, and multi-vCPU association remain deferred. | Exact three stable SDK ids and availability; SMCR-then-SMPRI-then-TPIDR2 order, every failure point and fresh retry, twenty-nine-way conflicts, abandonment, command/response channel closure, queued destruction, unwind, panic, shutdown, redacted `Debug`, and signed same-vCPU idle capture without raw logging, writes, maximum-SVL queries, SME data reads, guest instructions, or guest execution. |
| Runner system-context register capture (raw subset implemented) | #1210 adds a separate typed immutable raw SCXTNUM_EL0/SCXTNUM_EL1 value plus one macOS 15.2+ getter-only, failure-atomic owner-thread command in the shared core-register admission domain. `Debug` redacts both software context numbers, and both boot-session forms expose capture without involving the snapshot lease. Interpretation, feature/destination validation, persistence, schema, restore ordering, and multi-vCPU association remain deferred. | Exact two stable SDK ids and availability; EL0-then-EL1 order, both failure points and fresh retry, twenty-nine-way conflicts, abandonment, command/response channel closure, queued destruction, unwind, panic, shutdown, redacted `Debug`, and signed same-vCPU idle capture without raw logging, writes, guest instructions, guest execution, reset assumptions, or compatibility inference. |
| Runner EL1 translation-register capture and restore (sixth bidirectional subset implemented) | #1182 adds typed immutable raw SCTLR_EL1, TTBR0_EL1, TTBR1_EL1, TCR_EL1, MAIR_EL1, AMAIR_EL1, and CONTEXTIDR_EL1 state plus one owner-thread capture. #1238 adds ordered owner-thread restore of that complete value through the reusable typed system-register failure with the exact failed register and completed prefix. Hypervisor.framework does not make the seven writes transactional, so callers must retry the complete value or discard the vCPU before execution. Both boot-session forms expose capture and restore under shared core-operation admission, but the snapshot lease invokes neither. System-context registers and pointer-authentication keys are captured separately; table memory, feature and destination validation, barriers, TLB/cache maintenance, safe MMU transition ordering, persistence, orchestration, schema, rollback, and multi-vCPU coordination remain deferred. | Exact seven-field read/write order; every read and write failure; typed partial-write context; complete retry; twenty-nine-way conflicts; abandonment, channels, queued destruction, unwind, panic, shutdown; and signed MMU-off guest-written capture/restore/recapture preserving actual implementation-defined AMAIR readback without post-restore guest execution or value logging. |
| Runner pointer-authentication key capture (raw subset implemented) | #1190 adds a redacted typed value containing five 128-bit APIA, APIB, APDA, APDB, and APGA keys plus one failure-atomic owner-thread command in the shared core-register admission domain. Both boot-session forms expose it without involving the snapshot lease. Feature/algorithm validation, zeroization, protected persistence, SCTLR enable ordering, orchestration, schema, restore, and multi-vCPU association remain deferred. | Exact ten-register ids and low/high pairing; deterministic order, every failure point and retry, twenty-nine-way conflicts, abandonment, channel, queued destruction, unwind, panic, shutdown, redacted debug, and signed non-secret guest-written values without PAC execution. |
| Runner SIMD/FP capture and restore (seventh bidirectional subset implemented) | #1172 adds typed immutable Q0-Q31, FPCR, and FPSR state plus a 16-byte-aligned getter FFI seam. #1240 adds one target-gated C shim for the SDK's by-value vector setter and ordered owner-thread restore of the complete typed value. The 34 writes are nontransactional; a dedicated typed error distinguishes SIMD/FP and scalar registers and reports the exact completed prefix, so callers must retry the complete value or discard the vCPU before execution. Both boot-session forms expose capture and restore under shared core-operation admission, but the snapshot lease invokes neither. Maximum-width streaming Z0-Z31 and maximum-derived P0-P15 are captured separately only while `PSTATE.SM` is active; maximum-square ZA and fixed-size ZT0 are captured separately whenever `PSTATE.ZA` is active. Streaming Q/Z alias ordering, feature/destination validation, FPCR/FPSR writable-bit policy, protected persistence/zeroization, rollback, schema, orchestration, and multi-vCPU coordination remain deferred. | Exact 34-field read/write order; C/Rust pointer-to-vector ABI boundary; every read and write failure; mixed-register typed partial-write context; complete retry; twenty-nine-way conflicts; abandonment, channels, queued destruction, unwind, panic, shutdown; and signed non-streaming guest-written capture/restore/recapture without post-restore guest execution or value logging. |
| Runner thread-context register capture and restore (fifth bidirectional subset implemented) | #1176 adds typed immutable raw TPIDR_EL0, TPIDRRO_EL0, and TPIDR_EL1 state plus one owner-thread capture. #1236 adds ordered owner-thread restore of that complete value through the reusable typed system-register failure with the exact failed register and completed prefix. Hypervisor.framework does not make the three writes transactional, so callers must retry the complete value or discard the vCPU before execution. Both boot-session forms expose capture and restore under shared core-operation admission, but the snapshot lease invokes neither. TPIDR2 is captured separately with SME system registers, SCXTNUM_EL0/EL1 use the separate system-context value, and CONTEXTIDR_EL1 remains in translation state; address/destination validation, wider context ordering, persistence, schema, rollback, orchestration, and multi-vCPU coordination remain deferred. | Exact three-field read/write order; every read and write failure; typed partial-write context; complete retry; twenty-nine-way conflicts; abandonment, channels, queued destruction, unwind, panic, shutdown; and signed guest-written capture/restore/recapture without post-restore guest execution or value logging. |
| Runner physical-timer capture (raw subset implemented) | #1188 adds typed immutable raw CNTKCTL_EL1, CNTP_CTL_EL0, and CNTP_CVAL_EL0 state plus one failure-atomic owner-thread command; #1212 extends the same value and command with raw CNTP_TVAL_EL0. It generalizes timer admission so physical capture and every virtual-timer operation reject each other. Both boot-session forms expose capture without involving the snapshot lease. CNTP requires macOS 15 and GIC creation before the vCPU; CVAL/TVAL are separately timed absolute/relative views, and elapsed-time adjustment, writable-bit filtering, interrupt delivery, persistence, orchestration, schema, and restore remain deferred. | Exact SDK ids and availability; deterministic four-field order, every failure point and retry, bidirectional timer conflicts, abandonment, channel, queued destruction, unwind, panic, shutdown, signed disabled/masked guest-written capture, and signed idle TVAL observation without raw-value or stability assumptions. |
| Runner virtual-timer capture (raw subset implemented) | #1166 adds typed immutable mask/offset state and #1168 extends it with raw control/CVAL values. Timer-specific owner-thread get/set commands and one serialized four-field capture share generalized timer admission with physical-timer capture. Both boot-session forms expose capture, but the snapshot lease does not invoke it. CPU pending levels, the opaque GIC device blob, and EL1 ICC state are captured separately; restore-time offset/control policy, orchestration, and restore remain deferred. | Deterministic four-field order, conflict, abandon, channel, panic, and retry tests plus signed known-value capture that safely restores the original stable values and writable control bits. |
| Runner pending-interrupt capture (CPU-level subset implemented) | #1174 adds typed IRQ/FIQ owner-thread get/set commands and one failure-atomic IRQ-then-FIQ capture. CPU pending levels and validated GIC PPI mutations share generalized interrupt-operation admission but remain distinct state models. Both boot-session forms expose capture; HVF clear-after-run behavior, the separately captured opaque GIC device blob and EL1 ICC value, persistence, orchestration, and restore remain outside this slice. | Raw enum mapping, deterministic order, both failure points and retry, bidirectional conflicts, abandonment, channel, panic, shutdown, and signed `(true, false)`, `(false, true)`, and cleared capture. |
| Runner opaque GIC device-state capture (implemented) | #1178 adds a redacted immutable byte value and one owner-loop command for Hypervisor.framework's stable, versioned GIC device blob. It uses fallible allocation and retained-object cleanup, shares generalized interrupt admission, and relies on the current single-vCPU runner for Apple's stopped-VM condition. Both boot-session forms expose capture without involving the snapshot lease. EL1 ICC state is captured separately; parsing, persistence, restore, schema, orchestration, and multi-vCPU stopping remain deferred. | Create/size/data/release order; null, zero, allocation, backend, unwind, conflict, abandonment, channel, queued-destruction, panic, and shutdown coverage; redacted debug; and signed non-empty real-HVF capture. |
| Runner EL1 GIC ICC register capture (implemented) | #1180 adds a typed immutable ten-register value and one owner-thread command for PMR, BPR0, AP0R0, AP1R0, RPR, BPR1, CTLR, SRE, IGRPEN0, and IGRPEN1. It shares generalized interrupt admission and complements, but is not embedded in, the opaque GIC blob. Both boot-session forms expose it without involving the snapshot lease. `ICC_SRE_EL2`, ICH/ICV, restore, persistence, orchestration, and multi-vCPU association remain deferred. | Exact SDK ids and order; every read-failure position and retry; bidirectional conflicts, abandonment, channel, queued-destruction, panic, and shutdown coverage; and signed guest-written PMR/BPR/SRE/group-enable capture. |
| EL2 GIC CPU registers and emulated-device state | Inventory `ICC_SRE_EL2` plus ICH/ICV ownership and add stable state models for each implemented MMIO device. | Per-device round-trip unit tests and signed HVF EL2 CPU-interface/device-state coverage if nested virtualization is enabled. |
| Full guest-memory capture | Define immutable capture ownership, full-memory file layout, error cleanup, and path-redaction behavior before considering diff snapshots. | Memory/file unit tests, process failure tests, and signed full-capture coverage. |
| External resource policy | Define disk, vmnet, and vsock metadata, buffering boundary, disconnect/reconnect behavior, and restore overrides. | Resource-policy unit/process tests and focused signed network/vsock coverage. |
| Snapshot create orchestration | Hold the lease across the agreed capture boundary, assemble the versioned state, publish files transactionally, and release to ordinary pause. | API and process e2e tests plus signed HVF create/resume coverage. |

Snapshot load/restore, file-format compatibility, dirty tracking, minimum macOS
GIC support, and data-format inspection remain their own issue-sized areas. A
restore-path e2e should land only after a minimal versioned create/load pair
exists.

Until those areas land, bangbang should continue reporting snapshot create,
snapshot load, snapshot version, and snapshot inspection as recognized
unsupported behavior.
