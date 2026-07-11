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
| vCPU runner | The `bangbang-hvf-vcpu` thread owns `HvfVcpuOwner`. `HvfVcpuRunner` serializes HVF operations through commands and can return immutable X0-X30, PC, and CPSR values; guest-visible MIDR, MPIDR, and baseline PFR/DFR/ISAR/MMFR compatibility metadata; raw SP_EL0, SP_EL1, ELR_EL1, and SPSR_EL1 values; raw AFSR0_EL1, AFSR1_EL1, ESR_EL1, FAR_EL1, PAR_EL1, and VBAR_EL1 values; raw ACTLR_EL1 and CPACR_EL1 values; raw SCTLR_EL1, TTBR0_EL1, TTBR1_EL1, TCR_EL1, MAIR_EL1, AMAIR_EL1, and CONTEXTIDR_EL1 values; raw TPIDR_EL0, TPIDRRO_EL0, and TPIDR_EL1 values; raw baseline Q0-Q31, FPCR, and FPSR values; raw APIA, APIB, APDA, APDB, and APGA pointer-authentication keys in a debug-redacted value; raw physical-timer CNTKCTL, control, and CVAL values; raw virtual-timer mask, offset, control, and CVAL values; CPU-level IRQ/FIQ pending values; Hypervisor.framework's opaque GIC device-state bytes; or raw EL1 GIC ICC CPU-interface values through dedicated owner-thread commands. The snapshot barrier invokes none of these captures, and the remaining architectural/device inventory is not implemented. |
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
detached immutable value only after every read succeeds. Dedicated command-owned
admission excludes runs, MMIO completion, boot setup, metadata, timer,
interrupt operations, cancellation, and shutdown until capture finishes, even when the
caller abandons its response. This command is not called by the public pause or
snapshot-create paths and is not complete restorable vCPU state.

A second runner-local command reads raw `SP_EL0`, `SP_EL1`, `ELR_EL1`, and
`SPSR_EL1` values in that order and publishes one immutable value only after all
four reads succeed. It shares a core-register admission domain with the
general-register command, so neither capture can overlap the other or any
conflicting runner operation, and command-owned admission survives response
abandonment and unwind. Borrowed and owned boot sessions delegate this capture,
but the supervisor lease and public snapshot paths do not invoke it. The subset
has no restore API, input validation, persistence, or snapshot-schema meaning.

A separate core-register command reads raw `AFSR0_EL1`, `AFSR1_EL1`,
`ESR_EL1`, `FAR_EL1`, `PAR_EL1`, and `VBAR_EL1` in that order. It publishes
only after all six owner-thread reads succeed and shares the same command-owned
admission domain. Fault reports and guest addresses are sensitive guest state;
AFSR contents are implementation-defined, and the value does not validate one
coherent exception. It also omits vector-table memory and safe restore ordering.
Both boot-session forms delegate capture, while the supervisor lease and public
snapshot paths do not invoke it. Signed coverage writes an aligned unused VBAR
and takes no intervening guest exception; current Apple Silicon reads AFSR0 as
zero after a guest write, so that field is not assumed writable.

A separate core-register command reads raw `ACTLR_EL1` then `CPACR_EL1` and
publishes only after both owner-thread reads succeed. It shares the same
command-owned admission domain. Complete capture requires macOS 15 because
Hypervisor.framework exposes only `ACTLR_EL1.EnTSO` there; CPACR can contain
optional FP/SIMD/SVE/SME access controls that this raw value does not validate.
Both boot-session forms delegate capture, while the supervisor lease and public
snapshot paths do not invoke it. The value has no writable-bit, feature,
restore-ordering, persistence, or snapshot-schema policy. Signed coverage sets
only EnTSO and baseline FPEN, executes ISB, and destroys the VM after capture.

A separate core-register command reads guest-visible `MIDR_EL1`, `MPIDR_EL1`,
`ID_AA64PFR0_EL1`, `ID_AA64PFR1_EL1`, `ID_AA64DFR0_EL1`,
`ID_AA64DFR1_EL1`, `ID_AA64ISAR0_EL1`, `ID_AA64ISAR1_EL1`,
`ID_AA64MMFR0_EL1`, `ID_AA64MMFR1_EL1`, and `ID_AA64MMFR2_EL1` in that
order. It publishes only after all eleven owner-thread reads succeed and shares
the core-register admission domain, including bidirectional exclusion with the
standalone MPIDR metadata getter. These values describe the virtual CPU/HVF
feature view, not physical-host identity or mutable restore state; bangbang sets
MPIDR affinity to zero. Both boot-session forms delegate capture, but the
supervisor lease and public snapshot paths do not invoke it. Optional macOS
15.2 SVE/SME IDs, newer beta-only IDs, configuration-time feature manifests,
feature masks, destination policy, persistence, and schema remain deferred.
Signed coverage compares two captures and the MPIDR getter without hard-coding
one Apple CPU model or inferring portability.

A separate core-register command reads raw `SCTLR_EL1`, `TTBR0_EL1`,
`TTBR1_EL1`, `TCR_EL1`, `MAIR_EL1`, `AMAIR_EL1`, and `CONTEXTIDR_EL1` in that
order. It publishes only after all seven owner-thread reads succeed and shares
the same command-owned admission domain. Table bases and context ids are
sensitive guest state. The value does not include table memory, feature
validation, TLB/cache maintenance, or a safe restore sequence; both boot-
session forms delegate it, while the supervisor lease and public snapshot paths
do not invoke it. Signed coverage writes the stable fields while the MMU stays
disabled; current Apple Silicon reads AMAIR as zero after a guest write, so
that implementation-defined field is not assumed writable.

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
after all 34 reads succeed. It shares the general/core-system/exception/
execution-control/identification/translation/pointer-authentication/
thread-context command-owned admission domain and is exposed through both
boot-session forms without involving the supervisor lease or public snapshot
paths.
Hypervisor.framework aliases Q registers to the
low 128 bits of Z registers in streaming SVE mode; this subset therefore omits
the wider SVE/SME state and defines no restore or snapshot-schema contract.

Another core-register command reads raw `TPIDR_EL0`, `TPIDRRO_EL0`, and
`TPIDR_EL1` in that order and publishes one immutable value only after all three
reads succeed. These software thread-ID fields can contain guest TLS or kernel
pointers. The command shares failure-atomic admission with the general,
stack/exception-return, exception-report, execution-control, identification,
translation, pointer-authentication, and SIMD/FP captures and is exposed
through both boot-session forms. It omits
`TPIDR2_EL0`, wider system state, validation, persistence, and restore; the
supervisor lease and public snapshot paths do not invoke it.

A separate runner-local command captures raw `CNTKCTL_EL1`, `CNTP_CTL_EL0`, and
`CNTP_CVAL_EL0` in that order and publishes one immutable value only after all
three reads succeed. It shares generalized timer admission with every virtual-
timer getter, setter, and aggregate capture, and its command-owned admission
survives response abandonment and unwind. Hypervisor.framework exposes the
CNTP registers on macOS 15 and newer only when the VM creates its GIC before
the vCPU. The control ISTATUS bit is derived, and CVAL is an absolute comparator
against a continuing physical count; this subset therefore has no portable
elapsed-time adjustment, interrupt-delivery, writable-bit, or restore policy.
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
  VBAR_EL1; raw ACTLR_EL1 and CPACR_EL1; guest-visible MIDR, MPIDR, PFR0/1,
  DFR0/1, ISAR0/1, and MMFR0/1/2 compatibility metadata; raw TPIDR_EL0,
  TPIDRRO_EL0, and TPIDR_EL1; baseline Q0-Q31, FPCR, and FPSR; raw SCTLR_EL1,
  TTBR0_EL1, TTBR1_EL1, TCR_EL1, MAIR_EL1, AMAIR_EL1, and CONTEXTIDR_EL1; raw
  APIA, APIB, APDA, APDB, and APGA pointer-authentication keys; raw physical
  timer CNTKCTL, control, and CVAL values; raw virtual timer mask, offset,
  control, and CVAL values; and CPU-level IRQ/FIQ pending values have
  owner-thread capture subsets.
  Identification metadata still needs masks and destination compatibility
  policy and is not mutable state to restore.
  Remaining system registers, SVE/SME, and other optional architecture state
  still need a full inventory; the raw virtual-timer offset and absolute
  physical-timer comparator need explicit restore-time adjustment policies;
  derived ISTATUS observations are not control-restore contracts;
  pointer-authentication keys need feature validation, protected persistence,
  and safe enable ordering; and every captured field still needs a restore path
  on the owning thread.
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
| Runner general-register capture (first subset implemented) | #1164 adds a typed immutable X0-X30, PC, and CPSR value plus one serialized owner-thread command with command-owned failure-atomic admission. Borrowed and owned HVF boot sessions expose it, but the snapshot lease does not invoke it. Core system, exception, execution-control, identification, translation, and baseline SIMD/FP state are tracked separately; broader system registers, SVE/SME, timer, interrupt state, restore, and multi-vCPU coordination remain deferred. | Deterministic runner command/conflict/failure tests and signed HVF known boot-register capture. |
| Runner core system-register capture (raw subset implemented) | #1170 adds a typed immutable raw SP_EL0, SP_EL1, ELR_EL1, and SPSR_EL1 value plus one owner-thread command. It shares failure-atomic admission with general-register capture, and both boot-session forms expose it without involving the snapshot lease. Exception, execution-control, identification, and translation state are captured separately; broader system state, validation, restore, orchestration, and snapshot schema remain deferred. | Deterministic four-field order, all read-failure points and retry, bidirectional conflicts, abandonment, channel, unwind, panic, and shutdown tests plus signed guest-written known-value capture. |
| Runner EL1 exception-register capture (raw subset implemented) | #1184 adds typed immutable raw AFSR0_EL1, AFSR1_EL1, ESR_EL1, FAR_EL1, PAR_EL1, and VBAR_EL1 state plus one owner-thread command in the shared core-register admission domain. Both boot-session forms expose it without involving the snapshot lease. Vector-table memory, semantic validation, debug state, persistence, orchestration, schema, and restore remain deferred. | Exact SDK ids; deterministic six-field order, every failure point and retry, nine-way conflicts, abandonment, channel, queued destruction, panic, shutdown, and signed guest-written values including implementation-defined AFSR readback. |
| Runner EL1 execution-control capture (raw subset implemented) | #1186 adds typed immutable raw ACTLR_EL1 and CPACR_EL1 state plus one owner-thread command in the shared core-register admission domain. Complete capture requires macOS 15 for ACTLR.EnTSO. Both boot-session forms expose it without involving the snapshot lease. Optional CPACR fields, feature validation, CSSELR and SME-only controls, debug state, persistence, orchestration, schema, and restore remain deferred. | Exact SDK ids and macOS availability; deterministic two-field order, both failure points and retry, nine-way conflicts, abandonment, channel, queued destruction, panic, shutdown, and signed EnTSO/FPEN capture. |
| Runner identification-register capture (compatibility metadata implemented) | #1192 adds typed immutable guest-visible MIDR, MPIDR, PFR0/1, DFR0/1, ISAR0/1, and MMFR0/1/2 metadata plus one failure-atomic owner-thread command in the shared core-register admission domain. Both boot-session forms expose it without involving the snapshot lease. Optional macOS 15.2 SVE/SME IDs, beta-only newer IDs, configuration-time manifests, feature masks, destination policy, persistence, schema, and multi-vCPU association remain deferred. | Exact eleven stable SDK ids; deterministic order, every failure point and retry, nine-way conflicts including the standalone metadata getter, abandonment, channel, queued destruction, unwind, panic, shutdown, and signed same-vCPU stability/MPIDR comparison without model constants. |
| Runner EL1 translation-register capture (raw subset implemented) | #1182 adds typed immutable raw SCTLR_EL1, TTBR0_EL1, TTBR1_EL1, TCR_EL1, MAIR_EL1, AMAIR_EL1, and CONTEXTIDR_EL1 state plus one owner-thread command in the shared core-register admission domain. Both boot-session forms expose it without involving the snapshot lease. Pointer-authentication keys are captured separately; table memory, feature validation, TLB/cache maintenance, persistence, orchestration, schema, and restore remain deferred. | Exact SDK ids; deterministic seven-field order, every failure point and retry, nine-way conflicts, abandonment, channel, queued destruction, panic, shutdown, and signed MMU-off guest-written values including implementation-defined AMAIR readback. |
| Runner pointer-authentication key capture (raw subset implemented) | #1190 adds a redacted typed value containing five 128-bit APIA, APIB, APDA, APDB, and APGA keys plus one failure-atomic owner-thread command in the shared core-register admission domain. Both boot-session forms expose it without involving the snapshot lease. Feature/algorithm validation, zeroization, protected persistence, SCTLR enable ordering, orchestration, schema, restore, and multi-vCPU association remain deferred. | Exact ten-register ids and low/high pairing; deterministic order, every failure point and retry, nine-way conflicts, abandonment, channel, queued destruction, unwind, panic, shutdown, redacted debug, and signed non-secret guest-written values without PAC execution. |
| Runner SIMD/FP capture (baseline subset implemented) | #1172 adds typed immutable Q0-Q31, FPCR, and FPSR state plus a getter-only 16-byte-aligned HVF FFI seam. Its owner-thread command shares failure-atomic core-register admission with the general, core-system, exception, execution-control, identification, translation, pointer-authentication, and thread-context commands, and both boot-session forms expose it without involving the snapshot lease. Streaming SVE/SME state, restore, orchestration, and snapshot schema remain deferred. | ABI layout tests; deterministic 34-field order, every failure point and retry, nine-way conflicts, abandonment, channel, unwind, panic, and shutdown tests; and signed known Q0/Q31/FPCR/FPSR capture. |
| Runner thread-context register capture (baseline subset implemented) | #1176 adds typed immutable raw TPIDR_EL0, TPIDRRO_EL0, and TPIDR_EL1 state plus one owner-thread command in the shared core-register admission domain. Both boot-session forms expose it without involving the snapshot lease. TPIDR2/SME, wider system state, restore validation, persistence, orchestration, and schema remain deferred. | Exact SDK ids; deterministic three-field order, every failure point and retry, nine-way conflicts, abandonment, channel, queued destruction, unwind, panic, shutdown, and signed guest-written known-value capture. |
| Runner physical-timer capture (raw subset implemented) | #1188 adds typed immutable raw CNTKCTL_EL1, CNTP_CTL_EL0, and CNTP_CVAL_EL0 state plus one failure-atomic owner-thread command. It generalizes timer admission so physical capture and every virtual-timer operation reject each other. Both boot-session forms expose capture without involving the snapshot lease. CNTP requires macOS 15 and GIC creation before the vCPU; elapsed-time adjustment, writable-bit filtering, interrupt delivery, persistence, orchestration, schema, and restore remain deferred. | Exact SDK ids and availability; deterministic three-field order, every failure point and retry, bidirectional timer conflicts, abandonment, channel, queued destruction, unwind, panic, shutdown, and signed disabled/masked guest-written capture with GIC-before-vCPU ordering. |
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
