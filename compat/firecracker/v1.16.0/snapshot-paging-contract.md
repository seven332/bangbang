# Firecracker v1.16.0 Snapshot Paging Contract

This ledger records the #1527 public-macOS feasibility decision for the pinned
Firecracker snapshot page-fault corpus and the completed #1547 standalone
protocol, #1548 coordinated lazy-anonymous-memory, and #1549 task-local host
fault, and #1550 HVF guest-fault slices. It is not an aggregate runtime
implementation claim: bangbang still rejects native-v1 `Uffd` before artifact
or backend access.

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
`bangbang-pager-v1` codec, VMM/peer state machines, and absolute-deadline
transport over only an already-connected Unix stream. The normative wire and
lifecycle are in
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
drain. The transport uses one absolute deadline across partial I/O, suppresses
`SIGPIPE`, and becomes poisoned after timeout, EOF, truncation, malformed input,
or transport failure. V1 carries no peer strings, so malformed UTF-8 and
peer-diagnostic leakage are excluded by construction.

Focused unit tests cover every kind, every split boundary, coalescing,
exact/invalid bounds, reserved fields, Linux-UFFD-shaped input, handshake and
region validation, replay/cross-session/mismatch, out-of-order completion,
in-flight exhaustion, cancellation, shutdown, timeout/EOF, broken pipe, and
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
guest adapters below bind its two internal protection planes, while bypassing
consumers remain gated on a later audit slice.

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
return into accidental `SIGBUS`, or wait without the later bounded source
policy. Complete external-peer timeout/death and removal behavior remain
#1552.

Focused tests cover host-page validation, exact data/zero and permission paths,
source/content/coordinator failure, owner-busy rollback, admitted-action
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

This slice does not connect `bangbang-pager-v1`, grant external source
authority, integrate peer-driven removal or failure, certify every memory
consumer, make native-v1 `Uffd` succeed, or promote the aggregate capability.

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
advancing PC.

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
on the prior error path. The signed `guest_boot` target boots its entry
instruction directly from a lazy mapping:

```sh
cargo test -p bangbang-hvf --lib --all-features --locked lazy_guest
scripts/run-integration-tests.sh --test hvf_lifecycle -- hvf_lazy_guest_
scripts/run-integration-tests.sh --test guest_boot -- --exact lazy_guest_boot_integration::boots_guest_entry_from_a_lazy_instruction_page
```

This slice still uses a trusted in-process source. It does not broker a pager
peer, integrate peer-driven removal/failure, certify bypassing memory
consumers, activate native-v1 restore, or promote the aggregate capability.

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
| Permanent UFFD rejection | No longer the best platform conclusion after the signed combined public prototype, but remains the correct runtime behavior until delivery completes. |

## Current runtime and promotion gates

`crates/runtime/src/snapshot.rs::classify_v1_load_request` still rejects every
non-File native-v1 backend with `LoadMemoryBackend` before artifact access or
VM construction. The focused
`native_v1_load_policy_rejects_each_unsupported_dimension` and
`returns_fault_for_snapshot_endpoint` tests preserve that result; the signed
`signed_executable_creates_and_restores_native_v1_snapshot_across_processes`
case additionally proves private UFFD paths remain redacted.

Delivery parent [#1527](https://github.com/seven332/bangbang/issues/1527)
retains five integration/certification gates after the #1550 guest bridge:

1. [#1551](https://github.com/seven332/bangbang/issues/1551) brokers the contained peer.
2. [#1552](https://github.com/seven332/bangbang/issues/1552) integrates removal and failure.
3. [#1553](https://github.com/seven332/bangbang/issues/1553) audits and gates every memory consumer.
4. [#1554](https://github.com/seven332/bangbang/issues/1554) integrates supported native-v1 restore.
5. [#1555](https://github.com/seven332/bangbang/issues/1555) runs signed certification and promotes only direct evidence.

There is no public `Uffd` success before #1554 and no
`implemented-and-verified` inventory result before #1555. Delivery-time
`missing-platform-feasible` remains nonterminal and final capability validation
continues to reject it.

## Checked ledger

| Capability identity | Disposition | Delivery owner | Evidence | Result |
| --- | --- | --- | --- | --- |
| `corpus:snapshot-page-faults` | `missing-platform-feasible` | [#1527](https://github.com/seven332/bangbang/issues/1527) | Pinned upstream contract; public SDK/source audit; signed host, guest, removal, peer-loss, cleanup, and App Sandbox prototype output; implemented `crates/pager` protocol/process tests, `crates/runtime/src/lazy_memory.rs` coordinator/concurrency tests, `crates/hvf/src/lazy_host_fault.rs` focused plus signed/App Sandbox host-fault tests, and `crates/hvf/src/lazy_guest_fault.rs` focused plus signed execute/read/write/failure/cancellation/guest-boot tests; unchanged pre-access rejection | `nonterminal` |
