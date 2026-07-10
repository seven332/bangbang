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

## Required Prerequisites

Snapshot support should land only after these prerequisites are designed and
tested:

- Snapshot-ready pause ownership: the API owner must be able to stop all guest
  execution and device activity at a stable boundary without racing the HVF
  runner thread.
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

Future implementation should be split into issue-sized areas instead of one
large snapshot PR:

- Define snapshot-ready pause and runner-thread state ownership.
- Define the guest-memory snapshot file model and implement full memory create
  before diff snapshots.
- Prototype HVF vCPU and timer state save/restore on the owning thread.
- Inventory GIC state and decide the minimum macOS version boundary.
- Add persistent state models for currently implemented devices.
- Decide snapshot file format compatibility and versioning.
- Decide dirty tracking support or platform-limit behavior.
- Add restore-path e2e coverage once a minimal create/load path exists.

Until those areas land, bangbang should continue reporting snapshot create,
snapshot load, snapshot version, and snapshot inspection as recognized
unsupported behavior.
