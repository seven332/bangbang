# Firecracker v1.16.0 Snapshot Paging Contract

This ledger records the #1527 public-macOS feasibility decision for the pinned
Firecracker snapshot page-fault corpus. It is a delivery-time record, not an
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

## Decision and delivery boundary

Public macOS APIs can reproduce the observable external-demand-paging
contract, but not Linux UFFD descriptor or wire compatibility. Linux transfers
a kernel fault descriptor and performs no later socket request/response;
macOS must have the VMM translate both protection planes into a new bounded
protocol.

The approved later implementation keeps these boundaries:

- Mach task exceptions and all task/thread ports remain inside the worker.
- HVF stage-two exits cover guest read, write, and instruction faults.
- Both paths use one bounded, generation-aware page coordinator.
- A path-selected external peer speaks versioned, offset-only
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
retains nine implementation/certification gates after this record:

1. [#1547](https://github.com/seven332/bangbang/issues/1547) defines the pager protocol.
2. [#1548](https://github.com/seven332/bangbang/issues/1548) adds coordinated lazy anonymous memory.
3. [#1549](https://github.com/seven332/bangbang/issues/1549) bridges host faults.
4. [#1550](https://github.com/seven332/bangbang/issues/1550) bridges HVF guest faults.
5. [#1551](https://github.com/seven332/bangbang/issues/1551) brokers the contained peer.
6. [#1552](https://github.com/seven332/bangbang/issues/1552) integrates removal and failure.
7. [#1553](https://github.com/seven332/bangbang/issues/1553) audits and gates every memory consumer.
8. [#1554](https://github.com/seven332/bangbang/issues/1554) integrates supported native-v1 restore.
9. [#1555](https://github.com/seven332/bangbang/issues/1555) runs signed certification and promotes only direct evidence.

There is no public `Uffd` success before #1554 and no
`implemented-and-verified` inventory result before #1555. Delivery-time
`missing-platform-feasible` remains nonterminal and final capability validation
continues to reject it.

## Checked ledger

| Capability identity | Disposition | Delivery owner | Evidence | Result |
| --- | --- | --- | --- | --- |
| `corpus:snapshot-page-faults` | `missing-platform-feasible` | [#1527](https://github.com/seven332/bangbang/issues/1527) | Pinned upstream contract; public SDK/source audit; signed host, guest, removal, peer-loss, cleanup, and App Sandbox prototype output; unchanged pre-access rejection | `nonterminal` |
