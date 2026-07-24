# Firecracker v1.16.0 Snapshot Paging Contract

This ledger records the #1527 public-macOS feasibility decision for the pinned
Firecracker snapshot page-fault corpus and the completed #1547 standalone
protocol, #1548 coordinated lazy-anonymous-memory, #1549 task-local host
fault, #1550 HVF guest-fault, #1551 contained peer-broker, and #1552
removal/peer-failure slices, plus #1553's complete consumer inventory and
gates, and #1554's native-v1 direct/contained restore assembly. The exact
capability remains nonterminal until #1555 final certification; this ledger
does not claim Linux UFFD descriptor or wire compatibility.

## Pinned upstream contract

The source baseline is Firecracker v1.16.0 commit
[`d83d72b710361a10294480131377b1b00b163af8`](https://github.com/firecracker-microvm/firecracker/tree/d83d72b710361a10294480131377b1b00b163af8).
The authoritative behavior is described by the pinned
[page-fault handling document](https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/docs/snapshotting/handling-page-faults-on-snapshot-resume.md)
and implemented by
[`persist.rs`](https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/src/vmm/src/persist.rs#L550-L643).

Firecracker's File and UFFD backends have different observable ownership:

| Property | Pinned Firecracker UFFD behavior |
| --- | --- |
| Memory construction | Firecracker creates anonymous guest regions rather than mapping the snapshot memory file into the VMM. |
| Registration | It creates one close-on-exec, nonblocking UFFD, requires `EVENT_REMOVE`, and registers every host range. |
| Delegation | It connects to the configured Unix socket and sends the UFFD descriptor plus JSON records containing each host base, size, concatenated file offset, and page size. |
| Page population | The external handler maps the memory file and resolves kernel-delivered first-access faults with UFFD copy or zero operations. Host-side VMM accesses and guest accesses participate in the same registered ranges. |
| Removal | Balloon discard produces `UFFD_EVENT_REMOVE`; the handler must remember removed ranges and return zero, not old snapshot bytes, on a later fault. |
| Communication | The Unix socket is a one-shot descriptor/mapping handshake. Page-fault and remove events subsequently flow through the kernel UFFD object. |
| Peer lifecycle | Firecracker retains its UFFD descriptor. If the handler dies with an unresolved fault, the access can wait indefinitely; Firecracker documents operator monitoring and recycling as the policy. |
| Containment | With the Linux jailer, the handler, socket, and memory file reside inside the jail and the socket is private to the two processes. |

An equivalent macOS backend therefore needs real external page-content
ownership, demand population for both host and guest first access,
removal/refault-to-zero, explicit peer lifecycle and failure behavior, and
complete cleanup. Kernel File/COW paging or eager population does not satisfy
that contract merely because bytes arrive lazily or privately.

## Public macOS feasibility evidence

The #1527 probes ran on arm64 macOS 26.5.2 (build 25F84), Xcode 26.6, and the
public macOS 26.5 SDK. Sources were compiled with warnings denied. Every
Hypervisor.framework behavior ran only after code signing, and the combined
case was repeated in an App Sandbox bundle.

### No public custom memory-object pager

The documented `mach_memory_object_memory_entry_64` path rejected a
caller-owned pager port while its internal anonymous-object form succeeded:

```text
custom_pager_entry result=(os/kern) invalid argument (4) entry=null
anonymous_internal_entry result=(os/kern) successful (0) entry=non-null
```

The public SDK describes `memory_object_control_t` as vestigial and supplies no
userspace memory-object server callbacks. Current public
[XNU source](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/osfmk/vm/memory_object.c#L1907-L1917)
also converts a userspace port to no memory object. A private or privileged
custom Mach pager is not a supportable product mechanism.

### Separate guest and host protection planes

Public `hv_vm_protect` with zero guest permissions produced an exact IPA access
exit. A VMM-mediated request copied page contents, restored permissions, and
retried the instruction. Removal forced another exit that resolved to zero,
peer EOF was detected, and cleanup completed:

```text
first_fault reason=1 syndrome=0x93810007 ipa=0x200000
initial_population value=0x11223344
removed_fault reason=1 syndrome=0x93810007 ipa=0x200000
removed_population value=0x00000000
handler_death_detected=true
cleanup=complete
```

Protecting the host mapping with `PROT_NONE` after it was mapped into HVF did
not stop guest access:

```text
exit_reason=1 exception_class=0x16 value=0x1234abcd
guest_bypassed_host_protection=true
cleanup=complete
```

HVF stage-two protection is therefore required for guest faults; host
protection cannot substitute for it.

### Task-local host bridge

A server generated from the public SDK's `mach/mach_exc.defs` received an
owned host `EXC_BAD_ACCESS`, changed only the test page, wrote its contents,
returned success, and let the instruction retry:

```text
resumed_value=0x55667788 exception_count=1
cleanup=complete
```

The same task-local case succeeded with only
`com.apple.security.app-sandbox` and
`com.apple.security.hypervisor`. In contrast, an external exception server
received task-wide authority and could write unrelated target memory; handler
loss produced `SIGBUS`, and production-entitlement App Sandbox service
registration was denied:

```text
out_of_range_task_write=true value=0x0badc0de
dead_handler_fault=SIGBUS
service_registration result=Permission denied (1100)
```

Task and thread ports must stay inside the VMM. An external content owner may
receive only bounded page requests, never the task exception capability.

### Combined public mechanism

The combined probe used HVF zero-permission mappings for guest faults, a
task-local Mach exception bridge for host faults, and a socket-connected child
that owned snapshot bytes and removal state. It passed with the ordinary signed
HVF boundary and inside the production entitlement floor:

```text
guest_population value=0x31415926
host_population value=0x00000000 faults=1
removed_guest_population value=0x00000000
handler_death_detected=true
cleanup=complete
```

The trusted #1527
[Research Phase](https://github.com/seven332/bangbang/issues/1527#issuecomment-5061035828)
records the complete probe audit, and its
[Plan Challenge](https://github.com/seven332/bangbang/issues/1527#issuecomment-5061125888)
accepts the resulting delivery boundary. Prototype sources are workflow
evidence, not checked production implementation.

## Implemented standalone protocol

The dedicated `crates/pager` package now implements the shared
`bangbang-pager-v1` codec, VMM/peer state machines, absolute-deadline transport,
and a concurrent VMM-side client over only an already-connected Unix stream.
The normative wire and lifecycle are in
[`docs/snapshot-pager-protocol.md`](../../../docs/snapshot-pager-protocol.md).

Its 24-byte `BBPAGER\0` v1 header bounds every advertised body before
allocation. A random nonzero 32-byte session binds every frame. Negotiated
limits cover a 4 KiB–2 MiB power-of-two page, 1–128 exact regions, 1–256
combined requests, maximum frame size, and the complete closed operation mask.
Regions and work carry only nonzero opaque identities, generations, aligned
offsets, and lengths—never host virtual addresses or paths.

Request IDs are strictly increasing across page and removal work. Responses
may complete out of order only when the complete stored tuple matches.
Cancellation is session-wide and terminal; shutdown requires all work to
drain. `PagerClient` serializes outbound assignment while one receive owner
dispatches exact out-of-order page/removal replies to a bounded pending map.
The first timeout, EOF, truncation, malformed/mismatched response, explicit
peer terminal, or worker failure releases every pending caller exactly once.
The transport uses one absolute deadline across partial I/O, suppresses
`SIGPIPE`, and becomes poisoned after transport failure. V1 carries no peer
strings, so malformed UTF-8 and peer-diagnostic leakage are excluded by
construction.

Focused unit tests cover every kind, every split boundary, coalescing,
exact/invalid bounds, reserved fields, Linux-UFFD-shaped input, handshake and
region validation, replay/cross-session/mismatch, out-of-order completion,
in-flight exhaustion, concurrent page/removal out-of-order completion,
terminal fan-out, cancellation, shutdown, timeout/EOF, broken pipe, and
redaction. `crates/pager/tests/protocol_process.rs` uses an inherited connected
stream to complete real data/zero/removal/shutdown and cancellation child
sessions:

```sh
cargo test -p bangbang-pager --all-targets --all-features --locked
```

The crate does not open a socket path, transfer a descriptor, map guest memory,
mediate a Mach/HVF fault, grant source authority, or change API behavior.

## Implemented coordinated lazy anonymous memory

`crates/runtime/src/lazy_memory.rs` now implements the backend-neutral mapping
owner and page lifecycle shared by the host and guest fault bridges.
`LazyGuestMemory` is a distinct type rather than a lazy mode on initialized
`GuestMemory`; it transactionally allocates validated private-anonymous regions
but exposes no ordinary safe read/write/atomic/discard/export surface and reads
no source contents.

Before owner publication, construction validates the negotiated region/page/
in-flight tuple, unique IDs, ordered nonoverlapping guest ranges, aligned
nonoverlapping source ranges, checked page counts, a caller-bounded total page
count, and an independent waiter bound. One byte-sized tag per selected page
records `Absent`, `Loading`, `Publishing`, `Present`, or `Removing`, with an
owner-wide terminal overlay. Active operations and waiter completions live in
fallibly pre-reserved vectors capped by negotiated/local limits; there is no
per-page lock, generation allocation, channel, or waiter allocation.

The first absent fault returns one non-cloneable ticket with exact immutable
region, generation, access, offset, guest range, and length metadata.
Duplicates join that generation on one condition variable and observe one
content outcome. Read/write coalescing is content-only: later Mach/HVF bridges
must re-evaluate every woken fault's permissions. Consuming the exact current
ticket enters `Publishing`; a scoped target accepts one exact page of data or
zeroes and commit alone makes the page `Present`.

Issued population and removal tickets occupy negotiated protocol slots. If
removal makes a loading generation stale, that population becomes a counted
retired operation until its exact response is consumed, its ticket is dropped,
or terminal teardown abandons the session. Removal waits for overlapping
actions already linearized, then reserves a distinct slot before any page
mutation, records superseded outcomes, and returns one scoped exact-range
guard. Local zeroing leaves pages `Removing`; only explicit simulated/future
validated `Removed`
acknowledgement commit makes them `Absent` and permits one newer refault
generation.

Requested cancellation, peer failure, abandoned current tickets/guards,
generation exhaustion, poisoned synchronization, and teardown close admission
and wake waiters with stable value-redacted outcomes. Explicit termination
waits for already-linearized publication/removal actions; destructors are
nonblocking and guards retain the mapping until cleanup. Deterministic runtime
tests cover bounds, every state, exact data/zero publication, many duplicate
faults, capacity reuse, retired operations, response replay/mismatch, removal
races/acknowledgement, cancellation/failure/teardown/poison, generation
exhaustion, redaction, and repeated construction/destruction:

```sh
cargo test -p bangbang-runtime lazy_memory --all-features --locked
```

This slice installs no Mach exception port, HVF mapping/protection, socket,
source authority, peer state machine, native-v1 restore route, or public API
success. The coordinator alone does not enforce logical absence; the host and
guest adapters below bind its two internal protection planes. Later sections
record the completed consumer gates and public restore composition.

## Implemented task-local host fault bridge

`crates/hvf/src/lazy_host_fault.rs` now implements the host protection adapter
for macOS Apple Silicon. The build resolves the active public macOS SDK and
generates `mach_exc` user/server stubs from `mach/mach_exc.defs` into
`OUT_DIR`; no generated SDK source, private declaration, entitlement, service,
or external exception right is checked in or required.

Installation first validates that every coordinator page is an integral number
of host pages and creates a private non-copying writable alias for each retained
anonymous mapping with `mach_make_memory_entry_64` and `mach_vm_map`. It then
uses `task_swap_exception_ports` to atomically install only
`EXC_MASK_BAD_ACCESS` with 64-bit Mach exception codes while capturing the
prior task configuration, and transactionally protects the original mappings
with no host access. One process-global bangbang owner prevents ambiguous
stacking. Every partial failure restores permissions, stops any created server,
releases aliases and rights, and preserves the previous configuration.

The native callback accepts only exact ARM64 read/write data-abort protection
faults whose FAR matches the Mach fault address. Rust revalidates that address
against one retained host range before translating it to a guest address. The
shared `HvfLazyPageResolver` acquires the coordinator generation, presents a
trusted in-process source only the opaque region/generation/access and aligned
offset tuple, validates an exact data/zero page, and writes the complete page
through the private alias while the original remains inaccessible. A
sequentially consistent fence precedes least read-only/read-write permission,
the narrow platform-initialization proof, and coordinator commit. A later
write to a read-populated page upgrades only host permission.

Unowned addresses and unsupported exception forms never reach the source.
They are forwarded to the captured legacy or Mach default/state/
state-identity handler with its exact behavior, flavor, returned thread state,
and local port-right cleanup. Task/thread ports, memory-entry rights, aliases,
host addresses, and page bytes never leave the worker.

Shutdown closes resolver admission, waits already-admitted resolution, restores
the original mappings, and restores the captured task handler only if the
bridge still owns the exact bad-access slot. A later owner is preserved. Public
Mach supplies no compare-and-swap exception-port restore, so an independently
concurrent replacement retains a documented check-to-set race; bangbang
serializes its own single owner and requires external owners not to race its
lifecycle. Task exception handling also follows any thread-specific handler,
so the supported worker installs no competing per-thread bad-access owner.

An owned callback error or unwind terminalizes the coordinator and exits the
supervised worker with fixed status 70; it cannot fabricate zero/stale bytes,
return into accidental `SIGBUS`, or wait indefinitely. Peer timeout/death
closes coordinator admission nonblockingly as the first stable `PeerFailure`;
an already suspended callback is released through that same supervised
terminal path.

The resolver now separates source retrieval from platform visibility with a
writer-preferring shared/exclusive transition gate. Page publication and the
complete guest permission union retain one shared lease; removal takes the
exclusive lease, retires loading generations, revokes stage-two permission,
hides host permission, zeroes through the private alias while hidden, waits
for exact `Removed`, commits `Absent`, and only then admits a newer fault.
An old response that reaches `StaleGeneration` retries under the newer
generation instead of terminalizing or reopening stale bytes.

Focused tests cover host-page validation, exact data/zero and permission paths,
source/content/coordinator failure, removal during blocked population,
host/guest revoke and zero refault, owner-busy rollback, admitted-action
shutdown, redaction, and repeated cleanup. The signed `hvf_lifecycle` binary
then performs real read-first, write-first, aligned atomic, and raw-pointer
faults, forwards an unowned fault to a real prior Mach handler, preserves that
handler after it replaces the bridge, repeats the lifecycle, and observes the
fixed terminal exit in a child. The same cases pass under the production App
Sandbox entitlement floor:

```sh
cargo test -p bangbang-hvf --lib --all-features --locked lazy_host_fault
scripts/run-integration-tests.sh --test hvf_lifecycle -- lazy_host_fault_integration::
scripts/run-integration-tests.sh --test app_sandbox -- lazy_host_fault_integration::
```

`HvfLazyPager` now connects the resolver source boundary to `PagerClient` and
the same `LazyGuestMemory`. It rejects a peer-selected in-flight reduction
before the first page operation because the coordinator has already
preconstructed that exact combined operation bound; the protocol's independent
maximum-frame reduction remains supported. The #1549 slice alone did not
certify every memory consumer or make native-v1 `Uffd` succeed; #1553 and #1554
close those narrower gates without promoting the aggregate capability.

## Implemented HVF guest fault bridge

`crates/hvf/src/lazy_guest_fault.rs` now binds the same resolver to owned HVF
guest faults. `HvfBackend::map_lazy_guest_memory` maps each retained lazy region
at its validated maximum permissions, removes all stage-two access
transactionally, and activates the handler only after the complete mapping is
hidden. Dirty-write tracking and raw vCPU creation reject this mode rather than
installing a second protection owner or bypassing runner dispatch.

The exception decoder admits only signed-observed ARM64 data and instruction
abort forms. Data faults retain exact width/direction and IPA; instruction
faults require a valid VA/PC relationship and aligned four-byte IPA. HVC and
SYS64 handling run first, and unowned, disallowed, or malformed candidates
continue to the existing dirty/MMIO path without reaching the source.

For an owned candidate, the handler resolves every touched page through
`HvfLazyPageResolver` before publishing any stage-two permission. Serialized
per-page state unions `READ`, `READ|WRITE`, and `EXECUTE` requirements across
concurrent vCPUs, so a stale peer cannot downgrade a prior upgrade.
Instruction contents are synchronized before execute permission. The runner
then reports one `LazyPage` step and retries the same guest instruction without
advancing PC. The resolver's shared transition lease remains live through
instruction synchronization and every permission update.

The active handler is weakly registered with the resolver. Exclusive removal
uses that owner to revoke the exact aligned range to zero permission, reset
cached unions, and clear overlapping per-vCPU stale history before host
zeroing. Permission publication and revocation therefore cannot cross.

One peer-stale exit after a concurrent publication is admitted as progress; an
identical second exit fails closed instead of spinning. Source/coordinator,
instruction synchronization, and `hv_vm_protect` failures poison the guest
handler and shared resolver before later publication. Setup rollback unmaps
every region it can and retains any failed mapping for explicit cleanup.
Canceled HVF exits do not perform lazy work, while an operation already
synchronously admitted follows the coordinator's bounded drain contract.

Focused tests cover exact syndrome classification, cross-page
resolve-before-permission ordering, serialized permission unions, multi-vCPU
coalescing, stale/no-progress detection, setup rollback, source/protection
terminalization, redaction, dirty/raw exclusion, and canceled dispatch. Signed
`hvf_lifecycle` cases execute first from a lazy page, read and write separate
lazy pages, repeat the lifecycle, observe fail-closed source error with
cleanup, cancel an active runner without duplicate page work, block one source
request while two vCPUs coalesce on it, and keep an unowned instruction fault
on the prior error path. A signed case removes a committed real guest page,
observes a second stage-two fault, and refaults it to zero under a newer
generation. The signed `guest_boot` target boots its entry
instruction directly from a lazy mapping:

```sh
cargo test -p bangbang-hvf --lib --all-features --locked lazy_guest
scripts/run-integration-tests.sh --test hvf_lifecycle -- hvf_lazy_guest_
scripts/run-integration-tests.sh --test guest_boot -- --exact lazy_guest_boot_integration::boots_guest_entry_from_a_lazy_instruction_page
```

The source may now be the pager-backed adapter. #1553 later closes bypassing
memory consumers and #1554 activates the supported native-v1 composition;
aggregate promotion remains deferred.

## Implemented contained pager peer broker

The production launcher now accepts one singleton read-write
`snapshot-pager-stream` startup grant. It walks every source parent with
no-follow directory anchors, rejects aliases and non-sockets, performs one
nonblocking relative connect under a one-second absolute deadline, requires a
current-user peer, and compares the single-link socket vnode before and after
connection. It records only the connected descriptor identity, source vnode
identity, normalized status flags, and redacted peer UID/GID/PID.

The existing atomic launcher/worker grant channel carries that connected stream
as a closed record kind. The worker stages streams separately from files and
directories and independently revalidates `FD_CLOEXEC`, read-write nonblocking
status, `SOCK_STREAM`, `SO_ERROR`, AF_UNIX connection, descriptor identity, and
exact peer credentials before Commit. A one-time `PagerGrantAuthority` claims
only the exact `bangbang-grant:<GrantId>` role; reader loss and contained
teardown revoke unclaimed streams. No path, directory, listener, network
entitlement, task/thread port, host address, or peer diagnostic enters the
worker.

`bangbang-pager` additionally supplies a bounded support-only reference peer
over an already-connected stream. The contained worker probe now uses
`PagerClient`, verifies data and zero, removes the data page, refaults it to
zero, and drains. Signed production-bundle tests exercise that flow plus
cancellation, peer terminal, refused
connection, wrong descriptors and protocol, EOF, timeout, peer and worker
death, repeat launch, cleanup and redaction while inspecting the unchanged
launcher/worker signatures and entitlement floor:

```sh
scripts/run-integration-tests.sh --test production_bundle -- pager_grant
```

The same connected stream is adopted by `HvfLazyPager`; #1554's public restore
assembly consumes this exact authority after the consumer boundary below. The
reference peer is support tooling, not a daemon or Linux UFFD compatibility.

## Implemented protected lazy consumer boundary

`GuestMemory` now distinguishes eager ownership from a protected-lazy
consumer view. `LazyGuestMemory::claim_protected_consumer` is a documented
`#[doc(hidden)]` unsafe boundary that fallibly clones only region metadata and
the existing mmap `Arc`s after the exact Mach bridge is active. An atomic
one-shot claim prevents a second view, the wrapper is not cloneable, and the
safe `LazyGuestMemory` API still exposes no ordinary memory.

The protected view permits the existing bounded `read_slice`, `write_slice`,
and aligned `GuestMemoryAtomicU64` paths because their real load/store
instructions remain in the worker task and therefore fault through the Mach
bridge. Before mutation it rejects dirty tracking, region insertion/removal,
shared reservation, descriptor export, and ordinary discard. The only
supported removal remains `HvfLazyPageResolver::remove_pages`, which owns both
protection planes and exact pager acknowledgement.

`HvfLazyGuestMemoryConsumer` owns the view before a mutex-serialized
`HvfLazyHostFaultBridge`. This preserves the backend's existing `Send + Sync`
contract without declaring the unique Mach exception owner itself `Sync`.
`HvfBackend::map_lazy_guest_memory_with_consumer` consumes that composite,
stores it whenever any lazy HVF mapping survives, and retains it across
activation, binding, partial-map rollback, and failed-unmap paths. Successful
teardown stops joined vCPU/PVTime users, unmaps stage two, drops the protected
view, restores the Mach bridge, and only then releases coordinator/source
ownership. If both stage-two unmap and VM destruction fail, the backend
deliberately retains the mapping, guest-fault handler, and whole composite
rather than release host backing that HVF may still reference.

Crate-internal boot, virtqueue, device, and full-snapshot readers obtain the
protected view through the backend. Public `HvfArm64BootSession` and
`OwnedHvfArm64BootSession` guest-memory borrows reject lazy mode, so safe
callers cannot retain a raw pointer or atomic lease past bridge teardown. Full
native-v1 save uses the narrowly named unsafe
`native_snapshot_guest_memory` borrow only under the existing snapshot
quiescence transaction and performs bounded reads; dirty/diff save remains an
incompatible profile.

`LazyGuestMemoryConsumerProfile` is the closed backend-neutral preflight
classifier for dirty tracking, shared memory, external-process access,
ordinary balloon reclaim, and dynamic memory topology. It has a stable
priority and typed `LazyGuestMemoryConsumerRejection`. #1554 invokes it before
any path, socket, grant claim, artifact, or backend access while assembling the
supported `Uffd` restore.

### Checked guest-memory consumer inventory

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

Focused runtime tests prove the one-shot claim, stable profile priority, normal
slice/atomic operation, every state-changing rejection, empty descriptor
export, and vhost-user preflight. HVF tests prove the composite is `Send +
Sync`, public borrows are closed, and failed unmap or partial-map rollback
retains the bridge until cleanup. Signed lifecycle and guest-boot cases consume
the composite for real HVF execution and repeat teardown. The
test-bundle-only `pager-consumer` production probe runs slice, volatile raw
pointer, aligned atomic, virtqueue metadata, full snapshot image, mutation
rejection, pager removal, and zero refault through a real connected peer under
unchanged App Sandbox entitlements:

```sh
cargo test -p bangbang-runtime lazy_memory --all-features --locked
cargo test -p bangbang-hvf --lib --all-features --locked lazy_composite
scripts/run-integration-tests.sh --test hvf_lifecycle -- hvf_lazy_guest_faults_populate_execute_read_and_write_before_retry
scripts/run-integration-tests.sh --test guest_boot -- --exact lazy_guest_boot_integration::boots_guest_entry_from_a_lazy_instruction_page
scripts/run-integration-tests.sh --test production_bundle -- signed_pager_consumer_chain_runs_inside_app_sandbox
```

## Implemented native-v1 pager restore

#1554 removes the blanket native-v1 `Uffd` classifier rejection and keeps the
Firecracker-shaped `backend_type`/`backend_path` API. On macOS Apple Silicon,
the accepted path is the existing narrow fixed-memory native-v1 profile with
dirty tracking disabled. Here `Uffd` means one `bangbang-pager-v1` peer; an
unmodified Linux UFFD descriptor, JSON/SCM_RIGHTS handshake, or event stream is
not accepted.

`ProcessVmm::preflight_native_v1_memory_backend` runs after pristine-machine
policy and before configured path, socket, snapshot artifact, or backend
access. It validates the platform and
`LazyGuestMemoryConsumerProfile::in_process_fixed`; dirty, shared/export,
external-process, ordinary discard, and dynamic-topology profiles fail there.
Contained mode additionally requires an active exact
`SnapshotPagerStream` grant and validates it without consuming the one-time
stream. Direct mode rejects reserved grant syntax and validates only the Unix
path shape at this stage.

`PreparedHvfSnapshotV1State::prepare_lazy` decodes the state-only artifact and
validates the complete memory binding and platform ranges without opening the
snapshot memory file. Each binding range becomes a pager region with its exact
GPA and `file_offset - 48` data-source offset. Region count, host page size,
total pages, combined operations, frame size, waiter count, ordering, overlap,
alignment, and overflow are checked before the peer is acquired. The persisted
root selector is also resolved or grant-adopted, then validated for exact
identity, capacity, device ID, and read-only regular-file shape before peer
acquisition. The peer session is state-bound as:

```text
image_id[16] || crc64_jones_le[8] || data_length_le[8]
```

Direct mode connects to `backend_path` under the bounded snapshot-pager
deadline. Contained mode one-time claims the launcher-connected stream and
deliberately needs no `SnapshotMemoryInput` file grant in the worker. Both
paths complete the exact `Hello`/regions/`Start`/`Ready` negotiation before an
HVF VM owner exists.

Preparation then owns `LazyGuestMemory`, `HvfLazyPager`, the task-local
`HvfLazyHostFaultBridge`, its one-shot `HvfLazyGuestMemoryConsumer`, validated
root/device state, and the installed runtime in dependency order. Any failure
cancels the pager and drops those owners in reverse order. Once the one-shot
peer is adopted, every later preparation, restore, or worker-publication
failure is terminal for that VMM process; only failures before adoption remain
retryable. The restore
transaction creates and validates the VM/GIC, maps every lazy region with zero
initial stage-two access while retaining the composite, starts the vCPU owner,
restores architecture/GIC/time/identity state, and publishes the paused process
session only after every stage succeeds. Failure uses the existing typed
precommit/postcommit cleanup disposition. There is no eager read, memory-file
mapping, or `File`/COW fallback.

In-process device notifications borrow bytes from the protected consumer while
pmem/virtio-mem executors remain mapping-owned; this closes the combined-borrow
path that eager mappings previously supplied. Real Apple Silicon data
translation aborts may arrive without instruction-syndrome metadata. The lazy
classifier now rejects external-abort, invalid-address, cache-maintenance, and
stage-one-walk forms first, then uses WnR plus an exact one-byte IPA ownership
probe for that form; the protected-range handler still makes the final
authority decision.

Focused tests cover state-derived session bytes, lazy topology, profile/grant
ordering, direct connector redaction, combined protected-memory borrows,
ISV-clear data aborts, eager `File` regression, peer shutdown, and preparation/
restore cleanup. Signed direct execution restores a paused destination from an
externally owned memory image, resumes it, reaches guest `SYSTEM_OFF`, and
observes orderly pager shutdown. The normal production bundle repeats that
path through the exact launcher grant while omitting worker memory-file
authority:

```sh
cargo test -p bangbang native_v1_uffd --all-features --locked
cargo test -p bangbang-hvf --lib --all-features --locked snapshot_restore
scripts/run-integration-tests.sh --test executable_hvf_e2e -- macos_arm64::signed_executable_creates_and_restores_native_v1_snapshot_across_processes --exact
scripts/run-integration-tests.sh --test production_bundle -- normal_bundle_adopts_snapshot_grants_for_create_describe_and_restore --exact
```

## Decision and remaining delivery boundary

Public macOS APIs can reproduce the observable external-demand-paging
contract, but not Linux UFFD descriptor or wire compatibility. Linux transfers
a kernel fault descriptor and performs no later socket request/response;
macOS must have the VMM translate both protection planes into a new bounded
protocol.

The approved complete implementation keeps these boundaries:

- Mach task exceptions and all task/thread ports remain inside the worker.
- HVF stage-two exits cover guest read, write, and instruction faults.
- Both paths use one bounded, generation-aware page coordinator.
- An external peer speaks the implemented versioned, offset-only
  `bangbang-pager-v1`; requests never expose host virtual addresses.
- The launcher connects outside the App Sandbox and passes only the connected
  stream/source authority through the existing contained boundary.
- Handler loss takes one bounded supervised terminal path; it never fabricates
  data, falls through accidentally to `SIGBUS`, or waits indefinitely.
- External or shared memory consumers that bypass the task-local bridge reject
  the request before path, socket, artifact, or backend access.

This decision adds no private API, root dependency, ambient network
entitlement, dynamic Mach service, external task port, entitlement weakening,
or host-wide security or swap change.

## Alternatives

| Alternative | Disposition |
| --- | --- |
| File/COW or eager population | Remains a distinct useful backend, but does not delegate individual page contents or removal to an external owner. |
| Public custom Mach memory-object pager | Rejected by the public SDK/runtime evidence; hidden or privileged pager interfaces are outside support. |
| HVF faults plus explicit host-access hooks | Insufficient as a complete boundary because raw pointers and external/shared mappings can bypass a permanently fallible call-site audit. |
| Direct external Mach exception handler | Rejected because it exports whole-task authority, has unsafe death behavior, and cannot use the tested production App Sandbox discovery path. |
| Permanent UFFD rejection | Replaced for the narrow supported macOS profile by #1554's bounded VMM-mediated peer path; unsupported profiles still reject before resources. |

## Current runtime and promotion gates

`crates/runtime/src/snapshot.rs::classify_v1_load_request` now admits both
`File` and `Uffd`; higher-level
`ProcessVmm::preflight_native_v1_memory_backend` enforces the macOS target,
closed lazy-consumer profile, dirty exclusion, and exact direct/contained pager
authority before artifact access or VM construction. The focused
`native_v1_load_policy_rejects_each_unsupported_dimension`,
`native_v1_uffd_dirty_tracking_rejects_before_artifact_or_starter_access`, and
`native_v1_uffd_rejects_reserved_pager_reference_without_authority_before_state_open`
tests pin the split. `returns_fault_for_snapshot_endpoint` retains the
dirty-`Uffd` public rejection and redaction path.

Delivery parent [#1527](https://github.com/seven332/bangbang/issues/1527)
retains one certification gate after #1554:

1. [#1555](https://github.com/seven332/bangbang/issues/1555) runs cross-slice
   stress, complete entitlement inspection, the full validation matrix, and
   promotes only direct evidence.

Public `Uffd` success is now implemented, but there is no
`implemented-and-verified` inventory result before #1555. Delivery-time
`missing-platform-feasible` remains nonterminal and final capability validation
continues to reject it.

## Checked ledger

| Capability identity | Disposition | Delivery owner | Evidence | Result |
| --- | --- | --- | --- | --- |
| `corpus:snapshot-page-faults` | `missing-platform-feasible` | [#1527](https://github.com/seven332/bangbang/issues/1527) | Pinned upstream contract; public SDK/source audit; protocol/coordinator/host/guest/removal/consumer tests; signed direct and App Sandbox fault/removal/failure/cleanup evidence; exact contained connected-stream brokerage; state-bound lazy restore assembly; signed direct restore and production-bundle restore without worker memory-file authority; final #1555 certification outstanding | `nonterminal` |
