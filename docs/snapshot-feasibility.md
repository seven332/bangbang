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
| vCPU runner | The `bangbang-hvf-vcpu` thread owns `HvfVcpuOwner`. `HvfVcpuRunner` serializes thread-affine HVF operations through commands, but it has no architectural-state capture command today. |
| Auxiliary and host | Limiter retry threads retain deadlines and can request vCPU cancellation. The vmnet interface, vsock listener, retained streams, and their host/kernel buffers remain open for the lifetime of the boot session. A transient vsock polling thread is joined at the end of each vCPU run step. |

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
- vCPU lifecycle and register APIs are thread-affine, so state capture must run
  on the owning runner thread after the VM is quiesced.
- macOS 15 GIC APIs expose GICv3 distributor, redistributor, ICC, ICH, ICV, MSI,
  and SPI state access and interrupt injection primitives.

The inspected headers do not expose a KVM-style dirty log or dirty-page tracking
API. Firecracker-style diff snapshot parity is therefore not a direct HVF API
mapping. Later work must either prove another supported macOS mechanism, choose
software tracking for specific memory ranges, or document diff snapshots as a
platform-limited feature.

## Target Snapshot-Ready Ownership

The target design adds an internal, exclusive quiescence lease on top of the
public `Paused` state. This is a prerequisite contract, not implemented runtime
behavior or a new Firecracker-facing instance state.

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
| Preparing | The boot worker has exclusive ownership of lease acquisition. It drains earlier commands, closes admission to later mutations, and coordinates auxiliary quiescence. The public controller remains `Paused`. |
| Snapshot-ready | All in-process quiescence invariants have been acknowledged. The lease remains held for state capture, and the public controller remains `Paused`. |
| Releasing | Capture has completed, failed, or been canceled. The boot worker restores ordinary paused command and scheduler policy exactly once before acknowledging release. |

Preparation may acknowledge snapshot readiness only when all of these
invariants hold:

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
  with no deadline thread able to publish another wakeup token;
- no VMM thread is reading or writing vmnet packets or vsock streams, and the
  transient vsock poller has joined;
- lease acquisition and capture are bounded or observe an out-of-band stop
  token, so shutdown does not depend on queueing a command behind lease-owned
  work or on the synchronous API requester making progress; and
- shutdown and terminal status are checked before readiness is returned, so a
  stale successful acknowledgement cannot outlive the session.

The last-but-one invariant controls bangbang's access to external resources; it
does not freeze the host. Packets may accumulate in vmnet/kernel queues, and
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

- Snapshot-ready pause ownership: implement the exclusive quiescence lease and
  invariants above without racing the HVF runner, paused commands, auxiliary
  wakeups, or terminal teardown.
- Guest-memory file model: bangbang needs explicit ownership, layout, copy or
  mapping rules, and failure behavior for memory snapshot files.
- HVF vCPU state capture: all required general, system, SIMD/FP, timer, pending
  interrupt, and optional architecture state must be inventoried and restored on
  the owning thread.
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
| Supervisor lease and admission | Add preparing, ready, and releasing ownership; drain earlier commands; reject or defer later mutations; keep shutdown cancellation independent of the command queue; cover failure, terminal, resume, and shutdown races. | Supervisor and `ProcessVmm` unit tests plus API/process pause-state tests. |
| Auxiliary quiescence | Add acknowledged pause/resume and bounded cancellation to block, entropy, periodic, and other wakeup schedulers. | Deterministic scheduler unit tests and signed HVF cancellation/lifecycle coverage. |
| Runner capture boundary | Add a typed runner command for the first vCPU architectural-state inventory without exposing raw HVF ownership. | Runner command/conflict unit tests and signed HVF get/set round trips. |
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
