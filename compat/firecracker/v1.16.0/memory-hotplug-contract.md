# Firecracker v1.16.0 memory-hotplug closure contract

This ledger is the checked closure record for #1474, the second delivery slice
of #1440 under #1348. It covers exactly 19 directly owned Firecracker v1.16.0
memory-hotplug identities. Seventeen API operation, path, property, and schema
identities are `implemented-and-verified`. Exactly `corpus:memory-hotplug` and
`semantic.memory-device:virtio-mem-lifecycle-accounting-and-state` remain
`audit-required` because their complete upstream claims include optional-device
snapshot serialization and restore, which Wave 6 owns.

The generated source manifest remains 381 identities, the overlay retains 37
local semantic identities and 418 total records, and this reconciliation moves
the global disposition counts from 164/234/3/17 to 181/217/3/17.

## Evidence keys

- **API/model** — strict request parsing and response serialization in
  `crates/api/src/http.rs`, API conversion in
  `crates/bangbang/src/api_server.rs`, and transactional configuration and
  active-session ownership in `crates/bangbang/src/vmm.rs`.
- **Runtime** — one-queue virtio-mem feature negotiation, block-range policy,
  publication-safe mutation transactions, singleton metrics, and detached
  MMIO/PCI capture state in `crates/runtime/src/memory_hotplug.rs`; exact shared
  reservation identity in `crates/runtime/src/memory.rs`; selected-owner
  traversal in `crates/runtime/src/startup.rs`.
- **HVF** — exact shared-aperture views, HVF map/unmap rollback, discard and
  owner cleanup, dynamic dirty tracking, mapping capture, and paused MMIO/PCI
  traversal in `crates/hvf/src/{memory,startup}.rs`.
- **Focused validation** — route/model/controller tests in
  `crates/api/src/http.rs` and `crates/bangbang/src/{api_server,vmm}.rs`, queue
  and metric tests in `crates/runtime/src/{memory_hotplug,metrics}.rs`, and
  owner/mapping/dirty/accounting tests in `crates/hvf/src/memory.rs`.
- **Signed public validation** —
  `crates/bangbang/tests/executable_hvf_e2e.rs` proves Linux binds the selected
  MMIO and PCI virtio-mem device and the public requested/plugged lifecycle
  converges `0 -> 128 MiB -> 0`; production-bundle coverage proves the same
  lifecycle through the contained worker boundary.

## Exact 19-record ledger

| Identity | Final disposition | Exact contract and evidence |
| --- | --- | --- |
| `api-operation:GET /hotplug/memory` | implemented and verified | Returns the committed five-field configuration before start and the exact active requested/plugged status after start. API/model, focused, and signed public validation. |
| `api-operation:PATCH /hotplug/memory` | implemented and verified | Runtime-only requested-size replacement validates total/block bounds, grows the usable aperture to a slot boundary, updates config generation, signals the guest, and commits controller state only after owner success. Focused rollback and signed convergence validation. |
| `api-operation:PUT /hotplug/memory` | implemented and verified | Strict preboot replacement validates block/slot/total geometry and transactionally preserves the prior machine configuration on failure. API/model and signed startup validation. |
| `api-path:/hotplug/memory` | implemented and verified | Complete strict GET/PUT/PATCH route, method, state, JSON, and error behavior. DELETE is neither exposed nor claimed. API route and signed public validation. |
| `api-property:FullVmConfiguration.memory-hotplug` | implemented and verified | Nullable committed configuration appears exactly in `/vm/config`, participates in startup aperture allocation, and is preserved transactionally. API/controller and signed validation. |
| `api-property:MemoryHotplugConfig.block_size_mib` | implemented and verified | Optional/default-2 MiB unsigned value must be at least 2 MiB and a power of two; it defines queue block granularity and exact status projection. Focused boundary tests. |
| `api-property:MemoryHotplugConfig.slot_size_mib` | implemented and verified | Optional/default-128 MiB unsigned value must be at least 128 MiB and a multiple of block size; it controls aperture placement and usable-size growth. Focused boundary tests. |
| `api-property:MemoryHotplugConfig.total_size_mib` | implemented and verified | Required unsigned value must be at least one slot and a slot multiple; it fixes the reserved aperture and status total. Focused overflow/boundary and signed startup tests. |
| `api-property:MemoryHotplugSizeUpdate.requested_size_mib` | implemented and verified | Required unsigned PATCH value must be a block multiple no larger than total size and is delivered failure-atomically to the live owner. Focused rollback and signed lifecycle tests. |
| `api-property:MemoryHotplugStatus.block_size_mib` | implemented and verified | Exact immutable configured block size is returned before and after startup. API serialization and signed status validation. |
| `api-property:MemoryHotplugStatus.plugged_size_mib` | implemented and verified | Exact host-accounted committed plugged blocks are returned; backend mutation and guest-visible used publication precede the device-state commit. Queue failure-order and signed convergence validation. |
| `api-property:MemoryHotplugStatus.requested_size_mib` | implemented and verified | Exact committed requested size is returned and changes only after successful live-owner delivery. Focused transaction and signed PATCH validation. |
| `api-property:MemoryHotplugStatus.slot_size_mib` | implemented and verified | Exact immutable configured slot size is returned before and after startup. API serialization validation. |
| `api-property:MemoryHotplugStatus.total_size_mib` | implemented and verified | Exact immutable configured aperture total is returned before and after startup. API serialization and signed status validation. |
| `api-schema:MemoryHotplugConfig` | implemented and verified | Complete strict three-field schema with defaults, unknown-field/type rejection, semantic geometry validation, and selected MMIO/PCI startup execution. API/model and signed validation. |
| `api-schema:MemoryHotplugSizeUpdate` | implemented and verified | Complete strict required-field PATCH schema with unknown-field/type rejection and runtime-only transaction semantics. API/model and focused rollback validation. |
| `api-schema:MemoryHotplugStatus` | implemented and verified | Complete exact five-field response schema backed by committed configuration and live device accounting. API serialization and signed public validation. |
| `corpus:memory-hotplug` | audit required | All applicable live API/device behavior, metrics, exact owner mapping, and detached state are implemented. **[Wave 6 #1490](https://github.com/seven332/bangbang/issues/1490)** owns optional-device state encoding, artifact integration, restore construction, migration/clone behavior, portability policy, and signed restored-guest outcomes. |
| `semantic.memory-device:virtio-mem-lifecycle-accounting-and-state` | audit required | Live STATE/PLUG/UNPLUG/UNPLUG_ALL, failure-atomic HVF mutation, exact shared ownership, dirty tracking, metrics, MMIO/PCI traversal, capture-ready state, and teardown are implemented. **[Wave 6 #1490](https://github.com/seven332/bangbang/issues/1490)** owns serialized/restored virtio-mem state and aggregate artifact/portability certification. |

## Observable live, metrics, and capture-ready contract

- A single shared metrics producer is installed before activation. The
  `memory_hotplug` JSON object exposes Firecracker's exact 18 fields:
  `activate_fails`, `queue_event_fails`, `queue_event_count`, `plug_agg`,
  `plug_count`, `plug_bytes`, `plug_fails`, `unplug_agg`, `unplug_count`,
  `unplug_bytes`, `unplug_fails`, `unplug_discard_fails`, `unplug_all_agg`,
  `unplug_all_count`, `unplug_all_fails`, `state_agg`, `state_count`, and
  `state_fails`. Latency aggregates serialize `min_us`, `max_us`, and `sum_us`.
  Bangbang adds separate `interrupt_fails`, rollback, owner-cleanup, and
  teardown counters without changing the upstream fields.
- Every parsed supported request records one operation count and one latency
  sample at its final used-publication/commit boundary. Successful plug/unplug
  byte counters include only committed bytes. Internal partial rollback and
  late response/used-ring rollback are recorded once; discard and owner-release
  failures after publication remain distinct and do not rewrite guest-visible
  success. Teardown records owner release, not synchronous RSS convergence.
- Detached device state includes external configuration, config space,
  available and negotiated features, activation, exact queue geometry and
  cursors, pending notifications and interrupt state, and compact plugged
  ranges. Capture rejects unsupported features, activation disagreement,
  overlapping/unmapped rings, cursor disagreement, and consumed-but-unpublished
  descriptors. No guest-memory borrow, lock, endpoint, host address, or file
  descriptor escapes.
- Guest memory retains one descriptor-backed full-aperture reservation and
  exposes only a copyable opaque process-local mapping identity. Every online
  block must be an exact view of that identity. Capture compares compact device
  ranges with active guest owners, dynamic mapping metadata, actual HVF
  `GUEST_RAM` mappings, guest and HVF dirty tracking, the dirty epoch, and exact
  active/offline/current byte accounting. Unrelated dynamic mappings outside
  the aperture are ignored.
- A paused process-supervisor transaction quiesces auxiliary work and requires
  exactly one configured MMIO or PCI owner. MMIO captures under dispatcher
  ownership; PCI captures device and canonical transport under one endpoint
  lock. The result retains MMIO region/IRQ or PCI SBDF/BAR placement plus the
  mapping proof. Native-v1 creation performs this preflight before artifact
  publication but intentionally writes no virtio-mem bytes yet.

## Explicit Wave 6 handoff

This closure intentionally creates no virtio-mem byte encoding or compatibility
version. Wave 6 must integrate the detached value into an optional-device
artifact, define versioning and validation, reconstruct MMIO/PCI live owners,
restore shared-aperture identity and exact online views, re-establish dirty
tracking and queue/interrupt state, reconcile external machine memory, and
prove restored Linux plug/unplug behavior. Only after those outcomes may the
two retained aggregate records become terminal. Firecracker artifact
compatibility, cross-host portability, guest-independent convergence, runtime
device deletion, and synchronous host-footprint reduction are not implied by
this live closure.
