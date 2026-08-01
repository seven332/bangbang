# Firecracker v1.16.0 Snapshot Paging Contract

This ledger owns `corpus:snapshot-page-faults`: the pinned Firecracker
observable contract, the public-macOS feasibility result, bangbang's bounded
pager protocol and lazy-memory consumer boundary, the frozen native-v1 reader,
and the signed terminal evidence completed by
[#1555](https://github.com/seven332/bangbang/issues/1555).

The supported path reproduces external demand ownership through public macOS
and HVF mechanisms. It is not Linux UFFD descriptor or wire compatibility, and
current native-v2 rejects Uffd.

## Pinned Upstream Contract

The source baseline is Firecracker v1.16.0 commit
[`d83d72b710361a10294480131377b1b00b163af8`](https://github.com/firecracker-microvm/firecracker/tree/d83d72b710361a10294480131377b1b00b163af8).
The authoritative behavior is the pinned
[handling-page-faults-on-snapshot-resume.md](https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/docs/snapshotting/handling-page-faults-on-snapshot-resume.md)
and its
[`persist.rs` implementation](https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/src/vmm/src/persist.rs#L550-L643).

| Property | Pinned Firecracker UFFD behavior |
| --- | --- |
| Guest memory | Anonymous guest regions are registered with one nonblocking, close-on-exec UFFD that requires remove events. |
| External ownership | The handler receives the UFFD descriptor and bounded mapping metadata, maps the snapshot memory file, and supplies copy/zero results for first access. |
| Access planes | Host VMM and guest accesses fault through the same registered ranges. |
| Removal | Balloon discard produces a remove event; a later fault must return zero rather than stale snapshot bytes. |
| Failure | Firecracker retains the descriptor; an unresolved access can wait indefinitely if the handler dies, so the documented policy relies on monitoring and recycling. |
| Containment | Under the jailer, the handler, socket, and memory file are private to the jailed processes. |

An equivalent observable backend therefore needs real external page-content
ownership, host and guest first-access demand, removal generations with
refault-to-zero, bounded failure behavior, and complete cleanup. File/COW or
eager population remains useful but does not satisfy that contract.

## Public macOS Feasibility

The public-platform probes established these constraints:

- `mach_memory_object_memory_entry_64` rejects a caller-owned custom pager;
  private or privileged Mach pager interfaces are outside product support.
- Host `PROT_NONE` does not prevent guest access after an HVF mapping, so
  `hv_vm_protect` is required for guest read/write/execute faults.
- A task-local server generated from public `mach/mach_exc.defs` can own
  `EXC_BAD_ACCESS`, populate only the selected page, and retry the host
  instruction. Task/thread ports never leave the VMM.
- An external connected peer can own bytes and removal state while receiving
  only bounded, offset-based requests.

The combined signed probe recorded:

```text
guest_bypassed_host_protection=true
guest_population value=0x31415926
host_population value=0x00000000 faults=1
removed_guest_population value=0x00000000
handler_death_detected=true
cleanup=complete
```

It passed in the production worker's exact entitlement floor:
`com.apple.security.app-sandbox` and
`com.apple.security.hypervisor`. The task-local bridge uses
`task_swap_exception_ports`; owned source failure has a stable fixed status
70 rather than accidental data fabrication, unbounded waiting, or exposure of
the task exception capability.

The feasibility rationale and current format boundary are maintained in
[Snapshot Feasibility](../../../docs/snapshot-feasibility.md#public-uffd-equivalent-feasibility);
the authority and entitlement boundary is maintained in
[macOS Host Security Model](../../../docs/security.md#uffd-equivalent-pager-authority-boundary).

## Implemented Boundary

### Protocol and external owner

`crates/pager` implements the `bangbang-pager-v1` codec, VMM/peer state
machines, absolute-deadline transport, and concurrent client over an
already-connected Unix stream. The 24-byte `BBPAGER\0` header, random
session identity, region/page/in-flight bounds, monotonic request IDs,
out-of-order exact-tuple matching, removal acknowledgement, cancellation,
terminal fan-out, and shutdown rules are normative in
[Snapshot Pager Protocol](../../../docs/snapshot-pager-protocol.md).

Frames contain opaque identities, generations, aligned offsets, lengths, and
page bytes—never host virtual addresses or paths. The first timeout, EOF,
truncation, malformed response, peer terminal, or worker failure releases
pending callers once and poisons later transport use.

### Lazy anonymous memory and fault bridges

`crates/runtime/src/lazy_memory.rs` owns `LazyGuestMemory`, its bounded page
state/generation coordinator, removal ordering, and the one-shot protected
consumer. It exposes no ordinary mutable/exportable guest-memory surface.

`crates/hvf/src/lazy_host_fault.rs` owns the task-local Mach host-fault
bridge. `crates/hvf/src/lazy_guest_fault.rs` owns the stage-two guest
read/write/execute resolver and `HvfBackend::map_lazy_guest_memory` path.
Both planes share the same coordinator; every woken fault re-evaluates its own
permissions. Teardown joins users, unmaps stage two, drops protected consumers,
restores Mach exception ownership, and only then releases coordinator/source
ownership.

`crates/launcher/src/grant_manifest.rs` owns the contained
`snapshot-pager-stream` grant. `PagerGrantAuthority` claims one connected
stream without granting the worker a snapshot memory file. The launcher never
receives guest payloads or task/thread ports.

### Frozen native-v1 reader

`crates/bangbang/src/vmm.rs` keeps the public frozen native-v1 Uffd reader.
`ProcessVmm::preflight_native_v1_memory_backend` checks macOS Apple Silicon,
the fixed-memory consumer profile, dirty-tracking exclusion, and exact direct
or contained pager authority before opening snapshot artifacts or constructing
HVF state. The state-bound session/layout and peer negotiation validate before
transactional publication.

Direct mode connects to the configured Unix socket with one deadline.
Contained mode consumes only the preconnected pager grant. The persisted
native-v1 image binding remains
`image_id[16] || crc64_jones_le[8] || data_length_le[8]`, with region source
offset `file_offset - 48`. No public native-v1 writer selector is exposed.
Native-v1 File remains the eager regression reader; the current native-v2
family rejects Uffd before pager, memory, or HVF authority is adopted.

## Checked Guest-Memory Consumer Inventory

Every guest-memory consumer has a closed disposition. This table is parsed by
the checked-in inventory test; additions require an explicit lazy-memory
decision and evidence.

| Consumer identity | Concrete paths | Lazy behavior | Enforcement | Disposition |
| --- | --- | --- | --- | --- |
| consumer:guest-memory-slices | `GuestMemory::read_slice` / `write_slice` | Real worker loads and stores fault through Mach | Protected view plus signed App Sandbox data/zero/write probe | bridged |
| consumer:guest-memory-atomic | `GuestMemoryAtomicU64`, ARM64 PVTime publisher | Aligned load/store faults through Mach; lease must not outlive joined runner owners | Non-cloneable composite lifetime plus PVTime shutdown-before-unmap order | bridged |
| consumer:guest-memory-raw-pointer | region host addresses, mapping internals | Raw read/write instructions fault through Mach | No public protected-memory borrow; signed volatile-pointer probe | bridged |
| consumer:hvf-stage-two | HVF mapping and vCPU read/write/execute | Guest faults do not observe host protection | Zero initial stage-two access plus lazy-aware runner/resolver | resolver-only |
| consumer:virtqueue-core | descriptor, available, used, and indirect ring helpers | Uses bounded guest-memory reads/writes in the worker | Protected view; signed `VirtqueueAvailableRing::used_event` probe | bridged |
| consumer:transport-mmio-pci | MMIO/PCI queue dispatch and notification paths | Queue metadata remains in-process | Internal backend borrow; public borrow closed | bridged |
| consumer:boot-fdt | kernel, initrd, command line, FDT, and boot metadata writes | Startup writes fault and populate on demand | Internal protected view before vCPU start | bridged |
| consumer:block-sync-async | Sync/Async file block request headers, data, status, and retry completion | Worker/async executor copies use guest-memory helpers | Protected view; vCPU/device owners join before teardown | bridged |
| consumer:network-vmnet-mmds | virtio-net TX/RX, vmnet copy, MMDS frame/TCP stack | All guest bytes are copied in the contained worker | Protected view; no guest descriptor exported to vmnet | bridged |
| consumer:vsock | TX/RX/event queues and connection packet buffers | In-process queue and packet copies | Protected view; source work quiesces before snapshot/teardown | bridged |
| consumer:entropy | request queue and random-byte writes | In-process queue reads/writes | Protected view and retained retry-owner quiescence | bridged |
| consumer:balloon-control | stats, reporting, and PFN descriptor queues | Queue metadata is in-process | Protected view for control reads/writes | bridged |
| consumer:balloon-reclaim | inflate, hinting, reporting ordinary discard | `madvise` would bypass pager removal generations | Protected view returns `UnsupportedTarget`; profile requires pager-aware removal | preflight-rejected |
| consumer:memory-hotplug-control | virtio-mem request/response queue | Queue bytes are ordinary in-process accesses | Protected view for control traffic only | bridged |
| consumer:memory-hotplug-topology | shared aperture and dynamic insert/remove | Changes mapping inventory outside the fixed lazy coordinator | Profile and protected topology mutations reject | preflight-rejected |
| consumer:pmem | virtio-pmem queue metadata plus separately mapped backing | Queue metadata is covered; optional backing ownership is outside native-v1 lazy profile | Internal view for queue; existing native-v1 optional-device preflight | gated |
| consumer:vhost-user | cloned shared descriptors, userspace bases, socket/grant/backend protocol | Another process would bypass the task-local Mach owner | Anonymous/protected memory preflight before descriptor clone, socket, or grant | preflight-rejected |
| consumer:vmgenid-vmclock-pvtime | VMGenID/VMClock writes and retained PVTime atomics | Worker writes and runner atomics fault through Mach | Protected view; retained atomics destroyed with joined runner owners | bridged |
| consumer:snapshot-restore-population | connected pager data/zero and removal responses | Writes only through the bridge's private alias while primary pages are hidden | Coordinator generation plus exact response validation | resolver-only |
| consumer:snapshot-full-save | native-v1 full memory image streaming | Bounded reads populate missing pages normally | Snapshot-quiesced unsafe internal borrow; signed image writer probe | bridged |
| consumer:snapshot-dirty-diff | dirty bitmap and differential memory composition | Conflicts with lazy WRITE permission ownership | Dirty profile and `enable_dirty_tracking` reject | preflight-rejected |
| consumer:public-memory-borrow | public boot-session `guest_memory` / `guest_memory_mut` | Could retain pointers or atomics beyond bridge lifetime | Public-access backend methods reject lazy; only narrow unsafe snapshot borrow remains | preflight-rejected |
| consumer:teardown | vCPU/PVTime/device stop, HVF unmap, view, Mach owner, coordinator/pager | Incorrect order could leave an unmediated retained lease | Composite field order plus retained partial/failing cleanup owner | ordered-owner |
| consumer:eager-file-regression | eager anonymous/shared and native-v1 File/COW memory | No lazy tag or bridge behavior | Existing constructors select eager profile; full workspace/File tests unchanged | unchanged |

## Evidence Owners

| Evidence | Owner |
| --- | --- |
| Format selection, restore ordering, exact snapshot-version semantics | [Snapshot Feasibility](../../../docs/snapshot-feasibility.md) |
| Wire framing, limits, state machines, and terminal behavior | [Snapshot Pager Protocol](../../../docs/snapshot-pager-protocol.md) |
| Entitlements, contained grant authority, redaction, and cleanup | [macOS Host Security Model](../../../docs/security.md#uffd-equivalent-pager-authority-boundary) |
| Focused and signed command selection | [Testing Guide](../../../docs/testing.md#firecracker-capability-inventory) |
| Protocol/client implementation | `crates/pager/src/lib.rs`, `crates/pager/src/frame.rs`, `crates/pager/src/client.rs` |
| Coordinator and protected consumer | `crates/runtime/src/lazy_memory.rs` |
| Host and guest fault bridges | `crates/hvf/src/lazy_host_fault.rs`, `crates/hvf/src/lazy_guest_fault.rs` |
| Public restore and contained grant assembly | `crates/bangbang/src/vmm.rs`, `crates/launcher/src/grant_manifest.rs` |
| Signed validation | `crates/bangbang/tests/executable_hvf_e2e.rs`, `crates/hvf/tests/hvf_lifecycle.rs`, `crates/hvf/tests/guest_boot.rs`, `crates/launcher/tests/production_bundle_e2e.rs` |

Focused tests pin protocol framing and concurrency, lazy-memory state and
consumer gates, host and guest fault ownership, removal during blocked
population, failure fan-out, native-v1 preflight ordering, contained grants,
and public format routing. Signed cases cover host and guest execute/read/write
demand, coalescing, before/during/after-population removal generations,
refault-to-zero, peer and process death orders, cancellation, repeat cleanup,
the `pager-consumer` chain inside App Sandbox, and the exact nested signing
contract. The complete signed wrapper is the certification gate; commands live
only in the Testing Guide.

## Supported Boundary and Alternatives

The terminal supported boundary is:

- public APIs only; no private Mach API, root helper, ambient network
  entitlement, external task port, or entitlement weakening;
- one bounded generation-aware coordinator shared by task-local Mach and HVF
  stage-two fault bridges;
- a connected external content peer speaking offset-only
  `bangbang-pager-v1`;
- fixed-memory frozen native-v1 Uffd restore with dirty tracking disabled and
  incompatible consumers rejected before resource access; and
- bounded peer-loss terminal behavior and ordered cleanup.

| Alternative | Disposition |
| --- | --- |
| File/COW or eager population | Retained as a distinct backend; it does not delegate individual page contents or removal state. |
| Public custom Mach memory-object pager | Rejected by public SDK/runtime evidence; hidden or privileged interfaces are unsupported. |
| HVF faults plus audited host-access call sites | Rejected because raw pointers and external/shared mappings can bypass a permanently fallible call-site audit. |
| External Mach exception handler | Rejected because it exports task authority and has unsuitable discovery and death behavior. |
| Permanent Uffd rejection | Replaced only for the narrow frozen native-v1 profile; incompatible profiles and current native-v2 still reject before resources. |

## Checked Ledger

| Capability identity | Disposition | Delivery owner | Evidence | Result |
| --- | --- | --- | --- | --- |
| `corpus:snapshot-page-faults` | `implemented-and-verified` | — | Pinned upstream contract; public macOS feasibility; bounded protocol/coordinator/consumer implementation; direct and contained native-v1 restore without worker memory-file authority; signed host/guest/removal/failure/entitlement/cleanup certification | `terminal` |
