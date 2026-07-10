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
| vCPU runner | The `bangbang-hvf-vcpu` thread owns `HvfVcpuOwner`. `HvfVcpuRunner` serializes HVF operations through commands and can return immutable X0-X30, PC, and CPSR values; raw SP_EL0, SP_EL1, ELR_EL1, and SPSR_EL1 values; raw baseline Q0-Q31, FPCR, and FPSR values; raw virtual-timer mask, offset, control, and CVAL values; or CPU-level IRQ/FIQ pending values through dedicated owner-thread commands. The snapshot barrier invokes none of these captures, and the remaining architectural inventory is not implemented. |
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
admission excludes runs, MMIO completion, boot setup, metadata, virtual-timer,
interrupt operations, cancellation, and shutdown until capture finishes, including when the
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

A third core-register command reads all 16 bytes of Q0-Q31 in ascending order,
then raw FPCR and FPSR, and publishes one immutable baseline SIMD/FP value only
after all 34 reads succeed. It shares the general/core-system command-owned
admission domain and is exposed through both boot-session forms without
involving the supervisor lease or public snapshot paths. Hypervisor.framework
aliases Q registers to the low 128 bits of Z registers in streaming SVE mode;
this subset therefore omits the wider SVE/SME state and defines no restore or
snapshot-schema contract.

A separate runner-local command captures the HVF virtual-timer mask, raw offset,
raw `CNTV_CTL_EL0`, and raw `CNTV_CVAL_EL0` in that order and publishes one
immutable value only after all four reads succeed. It shares one serialized
virtual-timer admission domain with individual access to every captured field,
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
it, and full GIC/device state, persistence, and restore remain deferred.

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
  operations.
- vCPU lifecycle and register APIs are thread-affine. bangbang also routes
  virtual-timer and pending-interrupt access through the owning runner thread as its serialization
  policy, so a future capture bundle has one explicit vCPU command boundary
  after the VM is quiesced.
- macOS 15 GIC APIs expose GICv3 distributor, redistributor, ICC, ICH, ICV, MSI,
  and SPI state access and interrupt injection primitives.

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
| HVF GIC state | Inventoried by later work and routed to its documented HVF owner; vCPU-affine CPU-interface state must not be read directly by the process owner. |
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
  and SPSR_EL1; baseline Q0-Q31, FPCR, and FPSR; raw virtual-timer mask,
  offset, control, and CVAL values; and CPU-level IRQ/FIQ pending values have
  owner-thread capture subsets. Remaining system registers, SVE/SME, and other optional architecture
  state still need a full inventory; the raw timer offset needs an explicit
  restore-time adjustment policy; the derived ISTATUS observation is not a
  control-restore contract; and every captured field still needs a restore path
  on the owning thread.
- Interrupt-controller state: GIC distributor, redistributor, and CPU interface
  state must have a versioned restore plan before interrupt delivery can be
  considered compatible.
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
| Runner general-register capture (first subset implemented) | #1164 adds a typed immutable X0-X30, PC, and CPSR value plus one serialized owner-thread command with command-owned failure-atomic admission. Borrowed and owned HVF boot sessions expose it, but the snapshot lease does not invoke it. Core system and baseline SIMD/FP state are tracked separately; broader system registers, SVE/SME, timer, interrupt state, restore, and multi-vCPU coordination remain deferred. | Deterministic runner command/conflict/failure tests and signed HVF known boot-register capture. |
| Runner core system-register capture (raw subset implemented) | #1170 adds a typed immutable raw SP_EL0, SP_EL1, ELR_EL1, and SPSR_EL1 value plus one owner-thread command. It shares failure-atomic admission with general-register capture, and both boot-session forms expose it without involving the snapshot lease. Broader system state, validation, restore, orchestration, and snapshot schema remain deferred. | Deterministic four-field order, all read-failure points and retry, bidirectional conflicts, abandonment, channel, unwind, panic, and shutdown tests plus signed guest-written known-value capture. |
| Runner SIMD/FP capture (baseline subset implemented) | #1172 adds typed immutable Q0-Q31, FPCR, and FPSR state plus a getter-only 16-byte-aligned HVF FFI seam. Its owner-thread command shares failure-atomic core-register admission with the general and core-system commands, and both boot-session forms expose it without involving the snapshot lease. Streaming SVE/SME state, restore, orchestration, and snapshot schema remain deferred. | ABI layout tests; deterministic 34-field order, every failure point and retry, three-way conflicts, abandonment, channel, unwind, panic, and shutdown tests; and signed known Q0/Q31/FPCR/FPSR capture. |
| Runner virtual-timer capture (raw subset implemented) | #1166 adds typed immutable mask/offset state and #1168 extends it with raw control/CVAL values. Timer-specific owner-thread get/set commands and one serialized four-field capture share the same admission domain. Both boot-session forms expose capture, but the snapshot lease does not invoke it. CPU pending levels are captured separately; GIC state, restore-time offset/control policy, orchestration, and restore remain deferred. | Deterministic four-field order, conflict, abandon, channel, panic, and retry tests plus signed known-value capture that safely restores the original stable values and writable control bits. |
| Runner pending-interrupt capture (CPU-level subset implemented) | #1174 adds typed IRQ/FIQ owner-thread get/set commands and one failure-atomic IRQ-then-FIQ capture. CPU pending levels and validated GIC PPI mutations share generalized interrupt-operation admission but remain distinct state models. Both boot-session forms expose capture; HVF clear-after-run behavior, full GIC/device state, persistence, orchestration, and restore remain outside this slice. | Raw enum mapping, deterministic order, both failure points and retry, bidirectional conflicts, abandonment, channel, panic, shutdown, and signed `(true, false)`, `(false, true)`, and cleared capture. |
| GIC and device state | Inventory GIC ownership and add stable state models for each implemented MMIO device. | Per-device round-trip unit tests and signed HVF interrupt-state coverage. |
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
