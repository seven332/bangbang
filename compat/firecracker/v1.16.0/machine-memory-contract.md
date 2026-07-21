# Firecracker v1.16 machine memory and dirty-epoch contract

This contract fixes Bangbang's Apple Silicon/HVF interpretation of the pinned
Firecracker v1.16.0 machine fields `vcpu_count`, `mem_size_mib`, `smt`,
`track_dirty_pages`, and `huge_pages`. CPU templates remain a separate
capability slice.

## Pinned request and update behavior

Firecracker requires `vcpu_count` and `mem_size_mib` on
`PUT /machine-config`. Omitted optional PUT fields replace prior values with the
defaults: SMT false, no CPU template, dirty tracking false, and
`huge_pages = "None"`. The initial stored configuration is one vCPU and
128 MiB with the same optional defaults.

PATCH preserves omitted or explicit-null fields. `cpu_template = "None"`
clears a stored static template. An empty or null-only candidate returns
`Empty PATCH request.`. The request parser validates JSON shape, integer
representation, strict fields, and enum names, but sends representable semantic
candidates such as vCPU 0 or 33 and memory 0 to VMM validation.

Bangbang selects pre-boot machine-candidate faults in this order:

1. SMT must remain false;
2. vCPU count must be in `1..=32`;
3. memory must be in `1..=1,046,528` MiB;
4. a `2M` candidate must use an even MiB value;
5. the existing CPU-template policy is applied;
6. the dirty-tracking value is accepted; and
7. an otherwise valid exact `2M` candidate receives the platform result.

This preserves the pinned aarch64 SMT/vCPU/memory/page ordering while adding
Bangbang's realized-memory maximum. Only a valid complete machine candidate is
then checked against the configured balloon target, so cross-configuration
compatibility cannot mask a machine-field fault. CPU-template delivery remains
owned by its separate issue.

`track_dirty_pages = true` installs a backend-neutral page bitmap before normal
boot population and write-protects every writable HVF guest-RAM mapping before
vCPU ownership. Guest CPU faults and all bounded boot, VMM, device, discard,
and dynamic-memory mutations feed the same epoch. A visibly committed Full
snapshot transactionally re-protects pages and advances the epoch; load-time
tracking instead starts after image population, so the restored baseline is
clean and the ordered VMGenID replacement followed by VMClock update are its
first dirty writes. This tracking contract does not admit `Diff` snapshot
artifacts.

Rejected numeric candidates use unit-like errors and do not retain or echo the
submitted value. PUT and PATCH validate a complete candidate before changing
machine or balloon-related state.

Authoritative upstream sources:

- [machine request parser](https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/src/firecracker/src/api_server/request/machine_configuration.rs)
- [machine configuration model and update](https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/src/vmm/src/vmm_config/machine_config.rs)
- [v1.16 machine OpenAPI schema](https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/src/firecracker/swagger/firecracker.yaml#L1407-L1447)

## Configured and realized sizing

The public configuration range is:

| Field | Accepted configuration | Startup admission |
| --- | --- | --- |
| `vcpu_count` | `1..=32` | `1..=min(32, host_max)` |
| `mem_size_mib` | `1..=1,046,528` MiB | exact configured size, subject to ordinary allocation/mapping success |
| `smt` | false/default only | no dynamic SMT topology |

HVF queries the public host vCPU maximum before topology allocation or owner
creation. A capacity/query/construction failure leaves no retained boot session
and does not commit `Running`.

Firecracker's aarch64 layout can realize at most 1022 GiB. It nevertheless
accepts a larger nonzero configuration, retains and returns the request, then
clamps the later guest-memory layout with a warning. Bangbang deliberately
rejects a value above 1,046,528 MiB before storage instead of exposing a
requested value the guest does not receive. The internal layout helper still
caps defensively, and startup rejects unchecked oversized state before that
cap can become public behavior.

For every successful Bangbang configuration, the stored GET value is therefore
the value used for:

- balloon target compatibility;
- checked MiB-to-byte conversion;
- aarch64 DRAM layout and FDT memory nodes;
- anonymous guest-memory allocation and HVF mapping; and
- native-v1 expected memory length, capture, and restore validation.

This reject-versus-truncate difference is feasible API behavior, not a
platform-impossibility claim. It is selected so successful state remains
truthful; it does not remove usable memory because Firecracker cannot realize
the excess either.

Upstream truncation source:

- [Firecracker aarch64 `arch_memory_regions`](https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/src/vmm/src/arch/aarch64/mod.rs#L64-L87)

Neither implementation promises a reliable dynamic free-memory preflight.
Bangbang's private anonymous mappings use lazy/no-reserve semantics, and host
availability can change after any check. Allocation/mapping failures remain
typed, failure-atomic startup outcomes.

## Exact `huge_pages = "2M"` meaning

Pinned Firecracker's `2M` value means Linux hugetlbfs-backed guest memory. It
uses `MAP_HUGETLB | MAP_HUGE_2MB`, reports a 2-MiB backing page, requires even
MiB memory, and depends on a preallocated Linux huge-page pool. Its
`MAP_NORESERVE` behavior can defer pool exhaustion to `SIGBUS`. Snapshot
restore, balloon, and dirty tracking have hugetlbfs-specific restrictions.

It is not any of these weaker mechanisms:

- a virtual address aligned to 2 MiB;
- a sequence of mapping/protection operations batched over 2-MiB ranges;
- the current 16-KiB arm64 macOS host base page; or
- an HVF guest IPA translation granule.

Authoritative upstream sources:

- [Firecracker huge-page model and mmap flags](https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/src/vmm/src/vmm_config/machine_config.rs#L36-L88)
- [Firecracker huge-page operation, restore, balloon, and dirty constraints](https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/docs/hugepages.md)

## arm64 macOS and HVF blocker

Supported public Apple APIs do not provide that host-backing contract:

- XNU's arm pmap defines one base page per superpage and states
  `No superpages support`.
- XNU's arm64 SPTM pmap contains the same definition.
- Hypervisor.framework exposes 4-KiB and 16-KiB IPA granules. Those configure a
  guest translation layer and do not create a 2-MiB hugetlbfs host pool.
- Although the SDK exports `VM_FLAGS_SUPERPAGE_SIZE_2MB`, the matching public
  `mach_vm_allocate` call returns `KERN_INVALID_ARGUMENT` (`4`) on the current
  arm64 evidence host (macOS 26.5.2, build `25F84`, 16-KiB host page).

Authoritative platform sources:

- [XNU arm pmap superpage definition](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/osfmk/arm/pmap/pmap.h#L2398-L2400)
- [XNU arm64 SPTM pmap superpage definition](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/osfmk/arm64/sptm/pmap/pmap.h#L2062-L2064)
- [Apple's public XNU superpage test](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/tools/tests/superpages/testsp.c)
- [public Hypervisor.framework IPA granules](https://developer.apple.com/documentation/hypervisor/hv_ipa_granule_t)
- [public IPA-granule configuration](https://developer.apple.com/documentation/hypervisor/hv_vm_config_set_ipa_granule%28_%3A_%3A%29)

The matching local public probe returned:

```text
mach_vm_allocate result=4 address=0x0 aligned=yes
```

The probe used only public SDK declarations:

```c
const mach_vm_size_t size = 2ULL * 1024ULL * 1024ULL;
mach_vm_address_t address = 0;
const int flags = VM_FLAGS_ANYWHERE | VM_FLAGS_SUPERPAGE_SIZE_2MB;
kern_return_t result =
    mach_vm_allocate(mach_task_self(), &address, size, flags);
```

The address/alignment fields are incidental because allocation failed. A
successful probe would also have written the first and last mapping bytes before
calling `mach_vm_deallocate`; the failure occurred before either access.

Ordinary alignment and batching remain feasible optimizations under their own
names. Private APIs are unsupported; root-only host changes alter the accepted
deployment/security model; and a Linux VM or sidecar would back a different
process rather than Bangbang's native Apple-Silicon/HVF guest memory. None is a
valid implementation of this API value.

## Stable public result

`huge_pages = "None"` remains the default and succeeds. Syntactically valid
`"2M"` reaches VMM policy:

- odd in-range memory returns
  `machine mem_size_mib must be an even value when huge_pages is 2M`;
- an otherwise valid even-memory candidate returns
  `machine huge_pages 2M requires exact Linux hugetlbfs backing, which is unavailable on arm64 macOS/HVF`.

Both results occur before guest-memory allocation, HVF VM creation,
entitlement use, or any private/privileged fallback. They leave machine and
balloon state unchanged. Ordinary memory allocation, mapping, protection,
balloon discard, and host-resource failure remain supported or feasible
operations and are not covered by this exclusion.

## Evidence and boundaries

Implementation anchors:

- `crates/api/src/http.rs`: machine request serde and empty PATCH classification;
- `crates/runtime/src/machine.rs`: candidate bounds, precedence, page policy,
  and typed errors;
- `crates/runtime/src/startup.rs`: checked memory bytes and defensive maximum;
- `crates/hvf/src/topology.rs`: host vCPU admission before owner creation; and
- `crates/bangbang/src/api_server.rs`: VMM dispatch and public faults.

Focused validation:

- API parser machine configuration tests;
- runtime machine and VMM-controller machine/balloon transaction tests;
- API-server socket machine precedence and mutation tests;
- `executable_machine_config_bounds_and_fault_precedence_are_transactional`;
- unchecked oversized startup and injected topology-capacity tests; and
- signed executable SMP guest execution plus signed HVF topology lifecycle.

The strict impossibility conclusion and selected policy passed both #1391
Challenge checkpoints:

- [framing Challenge Review](https://github.com/seven332/bangbang/issues/1391#issuecomment-4989728561)
- [plan Challenge Review](https://github.com/seven332/bangbang/issues/1391#issuecomment-4989883731)

CPU templates and register modifiers were completed by their sibling issues;
#1395 and #1396 complete the guest-CPU primitive and public shared dirty epochs.
Diff snapshot serialization and dynamic CPU topology remain separate work.
#1392's cache presentation consumes the configured vCPU count but neither
changes these machine/memory bounds nor caps that count to one matched host
performance level. The exact 2M exclusion does not by itself certify the
aggregate machine schema; #1408 certifies that schema and its GET/PUT/PATCH
surface only after combining every terminal machine field, transactional
controller behavior, startup realization, and signed evidence. Generalized
snapshot artifacts remain separate Wave 6 work.
