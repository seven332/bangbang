# Firecracker v1.16.0 memory-hotplug closure contract

This checked #1474 ledger owns exactly 19 Firecracker v1.16.0 memory-hotplug
identities. All 19 are `implemented-and-verified`, including exact native-v2
`2.10.0` serialization, fresh-owner restoration, and signed direct and
normal-production/App-Sandbox MMIO/PCI continuation.

## Evidence keys

- **API/model** — strict request parsing and response serialization in
  `crates/api/src/http.rs`, API conversion in
  `crates/bangbang/src/api_server.rs`, and transactional configuration and
  active-session ownership in `crates/bangbang/src/vmm.rs`.
- **Runtime** — one-queue virtio-mem feature negotiation, block-range policy,
  publication-safe mutation transactions, singleton metrics, and detached
  MMIO/PCI capture state in `crates/runtime/src/memory_hotplug.rs`; exact shared
  reservation identity in `crates/runtime/src/memory.rs`; selected-owner
  traversal in `crates/runtime/src/startup.rs`; exact bounded kind 11 in
  `crates/runtime/src/snapshot_memory_hotplug_v2_10.rs`; and mixed private/shared
  File/COW materialization in
  `crates/runtime/src/snapshot_memory_v2/materialize.rs`.
- **HVF** — exact shared-aperture views, HVF map/unmap rollback, discard and
  owner cleanup, dynamic dirty tracking, mapping capture, and paused MMIO/PCI
  traversal in `crates/hvf/src/{memory,startup}.rs`; exact MMIO/PCI restore
  placement and fresh device/interrupt/dispatcher/route ownership in
  `crates/hvf/src/snapshot_v2_{memory_hotplug_,}platform.rs`.
- **Focused validation** — route/model/controller tests in
  `crates/api/src/http.rs` and `crates/bangbang/src/{api_server,vmm}.rs`, queue
  and metric tests in `crates/runtime/src/{memory_hotplug,metrics}.rs`, and
  fixed/hostile codec, bitmap, geometry, extent, all-sixteen-product,
  materialization, owner rollback, controller commit, immutable same-process
  peer, mapping, dirty, and accounting tests across runtime, VMM, and HVF.
- **Signed public validation** —
  `crates/bangbang/tests/executable_hvf_e2e.rs` and
  `crates/launcher/tests/production_bundle_e2e.rs` prove direct and
  outer-launcher/nested-App-Sandbox MMIO/PCI sources publish exact 2.10, then
  independent explicit-Paused/recaptured and automatic destinations verify
  retained nonzero plugged-memory sentinels before mutation and continue
  partial UNPLUG, driver-reprobe UNPLUG_ALL/replug, PLUG, and final UNPLUG.
  Contained malformed state/memory, cancellation, worker-first death, and
  launcher-first death clean sockets, sessions, mappings, routes, staging, and
  grant authority.

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
| `corpus:memory-hotplug` | implemented and verified | The applicable live API/device corpus plus exact native-v2 2.10 kind-11 encoding, artifact binding, fresh mixed-memory restore, all sixteen products, immutable same-process and fresh-process clones, signed MMIO/PCI restored Linux continuation, and contained cleanup are implemented. Focused and signed public validation. |
| `semantic.memory-device:virtio-mem-lifecycle-accounting-and-state` | implemented and verified | Live and restored STATE/PLUG/UNPLUG/UNPLUG_ALL, failure-atomic HVF mutation, exact shared ownership, dirty tracking, metrics, MMIO/PCI reconstruction, normalized recapture, immutable clones, and teardown are implemented and verified. |

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
  mapping proof. Frozen native-v1 still performs this preflight and rejects the
  unsupported profile before artifact publication.

## Exact native-v2 2.10 snapshot and restore contract

Current exact native-v2 `2.10.0` appends optional singleton semantic kind 11
after required serial kind 8 and independently optional profile-3 storage kind
7, entropy kind 9, and balloon kind 10. All sixteen optional combinations are
valid over one coherent MMIO or PCI transport. Kind 11 retains configuration,
features, config space, inactive or active queue state, exact transport, and a
canonical one-bit-per-block plugged bitmap. Geometry derives the exact bitmap
length; out-of-region and final-byte padding bits are zero. The complete
component is capped at 128 KiB inside the 16-MiB state bound and encodes no host
address, descriptor, backing identity, HVF owner, interrupt/dispatcher owner,
metric owner, cleanup authority, or source dirty epoch.

Kind-1 memory extents remain byte-for-byte unchanged. Inside-aperture extents
must exactly cover kind 11's plugged ranges independent of fragmentation;
crossing, missing, extra, overlapping, offline, misaligned, truncated, or
overflowing relationships reject before publication. File/COW load validates
the complete state/memory pair first, maps base extents privately, allocates one
fresh unlinked shared aperture per destination, copies only committed plugged
bytes into block-granular shared views, establishes a clean dirty epoch, and
constructs fresh MMIO/PCI device, notifier, interrupt, dispatcher, endpoint,
route, metric, and cleanup owners before atomic Paused publication.

The same immutable pair may create independent same-process peers or fresh
processes. Signed direct and contained guests prove the first resumed action
verifies every retained nonzero sentinel byte before topology mutation, then
exercise requested-size changes, partial UNPLUG, Linux unbind/rebind
UNPLUG_ALL, re-PLUG, later growth, and final removal. Explicit destinations
recapture equivalent normalized kind-11 semantics; automatic destinations use
ordinary resume. Malformed state/memory, cancellation, worker/launcher death,
and every lower-layer construction fault either roll back exactly or terminate
without retained unpublished authority.

This is not Firecracker snapshot-byte compatibility. Native-v2 Diff/merge and
Uffd, source owner or dirty-epoch identity,
synchronous host-RSS reduction, guest-independent convergence, and
unconstrained cross-host portability remain explicit non-claims.
