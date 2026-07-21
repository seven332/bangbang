# Firecracker Compatibility Scope

This document describes bangbang's intended Firecracker compatibility scope. It
is a planning reference for future API, VMM, and backend work; it does not mean
the current scaffold implements all listed API behavior.

The current repository defines crate boundaries, endpoint names, a minimal
HTTP-over-Unix-socket API server for `GET /`, `GET /version`,
`GET /vm/config`, `GET /machine-config`, pre-boot `PUT /machine-config`
configuration storage, pre-boot `PUT /boot-source` configuration storage, pre-boot `PUT /drives/{drive_id}`
configuration storage, pre-boot `PUT /network-interfaces/{iface_id}` configuration storage, pre-boot `PUT /vsock` configuration storage plus an internal virtio-vsock config-space, packet header model, TX descriptor packet parser, TX available-ring drain helper with used-ring descriptor completion, prepared device resource, host Unix socket listener owner, accepted host stream owner, bounded accepted-stream polling and retention, accepted-stream `CONNECT <PORT>` handshake reader, host local port allocator, retained host connection table model with pending host-initiated request packet headers, RX delivery and late RX retry for host request packet headers, guest `RESPONSE` acknowledgement for retained host-initiated connections, guest `RST` cleanup and guest `SHUTDOWN` partial-state/full-cleanup for retained host-initiated and guest-initiated connections, bounded guest-visible `RST` queueing for unsupported or orphan host-destined guest packets, bounded guest-initiated `uds_path_<PORT>` connection handling with guest `RESPONSE` or `RST` header delivery, guest `RW` payload forwarding to retained host streams for established host-initiated or guest-initiated connections with bounded four-packet per-connection guest-to-host retry buffering, bounded four-packet per-connection host-to-guest `RW` backlog and delivery from established retained streams into guest RX buffers, minimal guest `CREDIT_UPDATE` consumption and `CREDIT_REQUEST` responses with guest-visible `CREDIT_UPDATE` headers for established retained streams, MMIO registration helper, MMIO handler skeleton with active queue metadata retention, handler-level RX/TX notification dispatch, no-op event notification handling, startup FDT attachment, boot-runtime/HVF RX/TX notification dispatch with queue interrupt signaling, and boot-runtime/HVF no-op event notification handling, pre-boot `PUT /metrics` output configuration, pre-boot `PUT /logger` output configuration, pre-boot `PUT /serial` output configuration, process-owned `PUT /actions` startup with an internal boot run-loop worker across bounded step windows, runtime `FlushMetrics` with a minimal per-process metrics sink, a macOS-gated internal vmnet descriptor, lifecycle, start owner, concrete system start/stop backend, and packet descriptor boundary model for future host networking, a backend-neutral VM trait, a minimal VMM action/data model with internal
`InstanceStart` preflight, transactional startup executor, and successful-start state transition helpers, an internal MMDS guest ARP/TCP packet classifier, process-local packet-payload HTTP exchange, process vmnet TX detour, internal MMDS ARP and TCP response-frame synthesis, bounded ordered split-request buffering, and queued virtio-net RX delivery with bounded post-TX retry, backend-neutral guest
physical address and aarch64 DRAM layout/access primitives, arm64 boot
placement helpers, internal boot-source validation and arm64 kernel/initrd
payload loading, an internal Firecracker-shaped drive configuration validation
model, a Firecracker-shaped network interface configuration storage and
validation model, internal virtio-net config-space, activation, TX frame parser, RX buffer parser, prepared device resources, MMIO registration helpers, startup FDT attachment, and internal TX/RX notification dispatch metadata plus injected TX packet sink and RX packet source boundaries for virtio-net devices, a host-file backing access layer, internal configured block-device
preparation and MMIO registration helpers, an internal virtio-block
config-space capacity model, an internal virtio-block request parser, single-request
executor, queue dispatcher, MMIO queue-state bridge, resettable activation
state, notification/interrupt-status dispatch helper, guest-visible raw block
read validation through the signed HVF boot test, an internal TX-only
serial MMIO output device model with shared bounded capture support, and a minimal
Hypervisor.framework VM create/destroy wrapper, a current-thread HVF vCPU
create/destroy wrapper, typed HVF exit surface with MMIO data-abort decoding,
registry resolution, vCPU exit classification, single resolved HVF MMIO
exit dispatch/completion through runtime handlers, explicit runner-thread MMIO
handling commands, narrow vCPU register wrappers, macOS 15+ HVF GIC v3 boot metadata with an explicitly selected public-HVF GICv2m MSI frame and all-virtio PCI/MSI-X path but without ITS, HVF SPI interrupt-line allocation and signaling, minimal internal
arm64 FDT generation with optional RTC, serial, VMGenID, and virtio-mmio device-node descriptors and guest-memory writes, anonymous guest memory allocation
for validated runtime layouts, HVF guest memory map/unmap ownership and
controlled mapped-memory access for allocated regions, an internal MMIO region ownership registry and operation/data
model plus handler dispatch boundary, an internal TX-only serial MMIO output
handler that captures transmit bytes without global state, an internal virtio-mmio register/access
decoder, feature/status, queue, queue notification, and interrupt
status/acknowledgement register state, a composed runtime handler that routes
common register accesses through those state models and exposes drained queue
notifications, delegated device-configuration accesses, and a `DRIVER_OK`
activation hook with reset callback, plus virtqueue descriptor-chain validator,
available-ring read model with negotiated indirect descriptor support,
used-ring write model, and internal virtio-block
queue construction, drain, resettable active queue ownership, and active queue
notification dispatch helper with virtio-mmio queue interrupt-status updates
for future device handlers, internal boot-resource assembly from stored VM
configuration with optional RTC and serial plus block and network MMIO registration,
boot-runtime block and network notification dispatch with per-device metadata,
including an HVF wrapper path for injected virtio-net packet I/O, an internal
backend-neutral interrupt line/status/trigger model, primary-only arm64 HVF
boot-register setup, internal size-one-or-many HVF arm64 boot-session preparation
with one shared MMIO dispatcher, controlled mapped guest-memory access, indexed
runner-thread MMIO handling, a topology-wide run-control boundary, and an
ordered owner-thread vCPU topology consumed by an internal concurrent bounded-run
coordinator with active-only batch cancellation,
baseline and optional SVE/SME guest-visible identification metadata, pointer-
authentication key-state capture with redacted `Debug`, raw cache-selection
plus ordered nontransactional restore of its typed CSSELR_EL1 value,
hardware-breakpoint, hardware-watchpoint, debug-control plus ordered
nontransactional restore of its typed MDCCINT_EL1/MDSCR_EL1 value, debug-trap
policy plus ordered nontransactional restore of its complete two-Boolean value, and
physical-timer CNTKCTL/control/CVAL/TVAL capture, a virtual-timer
mask/offset/control/CVAL boundary, a normalized freeze-downtime timer state with
never-run restore, a fail-closed inactive SVE/SME/debug snapshot classifier,
prepared-session VMGenID replacement plus edge notification, CPU-level IRQ/FIQ pending capture plus
ordered nontransactional restore of its complete typed value, a bounded
internal boot-session run-loop pump, owned internal boot-session handle,
process-level owned
startup-session wiring with optional serial capture and boot run-loop supervision
across bounded step windows with retained internal worker status, process-owned
virtio-net packet-I/O provider selection with no-op fallback and vmnet-backed
startup for configured interfaces, an internal vmnet virtio-net packet I/O
provider keyed by configured interface ID, boot block, virtio-net, and
virtio-vsock queue interrupt signaling,
virtual timer PPI assertion, per-controller metrics and logger output state, and an initial process startup argument model.
There is no broader API request body model beyond the initial boot-source,
drive configuration, drive update, network-interface configuration, vsock configuration, machine-configuration, metrics, logger, serial, and actions bodies, public guest
execution beyond internal startup execution across bounded step windows, full public run-loop control beyond the current pause/resume subset, complete interrupt
delivery, including timer EOI/deactivation-driven unmasking,
general HVF runner-loop notification scheduling, public serial output streaming,
serial/backend interrupt wiring beyond the internal boot block and network notification
and retained serial capture paths,
broader device-backed feature negotiation,
device-backed runner-loop MMIO scheduling, complete device emulation,
production log rotation/syslog/journald/tracing/remote telemetry, process-global
panic/fatal observability durability,
non-timer CPU-suspend wake and broader PSCI power management, or successful actions beyond owned `InstanceStart`
startup with an internal boot run loop across bounded step windows and runtime
`FlushMetrics` yet. The implemented logger, interval metrics, serial, and
native-v1 UART boundaries are defined in
[Firecracker v1.16.0 Observability Contract](#firecracker-v1160-observability-contract):
configuration alone is silent, automatic writes are session-owned and best
effort, and explicit `FlushMetrics` remains fallible. Public drive configuration is
recorded as pre-boot VM state and applied during startup preparation. Runtime
`PATCH /drives/{drive_id}` can refresh the backing file of an existing active
virtio-block device through the process-owned boot session, but public
block-device attachment, boot selection changes, and hotplug remain deferred.

## GICv2m MSI Foundation

On macOS 15 and later, exact `--enable-pci` requests a nonzero, demand-sized MSI
interrupt range from Hypervisor.framework's public GIC API. The process probes
the fixed target and required symbols before readiness; VM startup configures
the host-reported frame and partitions the host SPI range into a nonempty
legacy prefix and a distinct MSI suffix. The pinned Linux GICv2m driver treats
INTID 1019 as its exclusive upper boundary, so neither allocator advertises
that terminal SPI even when HVF reports it. Without the flag, the ordinary GIC,
process startup path, and FDT remain unchanged and MSI-free.

An enabled session advertises one `arm,gic-v2m-frame` child below the GICv3 FDT
node and retains a generation-bound allocator plus a serialized, send-only
signaler. Startup atomically reserves the exact complete VM vector demand.
Because Linux allocates only the vectors each driver actually requests, every
function receives a separately revocable registry over the complete pool of
exact guest address/data routes rather than a predicted per-device subrange.
Ambiguous duplicate routes, out-of-range messages, foreign or stale
capabilities, and quiescing or released registries are rejected without
exposing tuple values in `Debug` or errors. The
message address is derived from the validated frame and its GICv2m `SETSPI`
offset; callers never gain arbitrary host interrupt authority. Device teardown
closes admission and drains in-flight sends independently; the final registry
owner releases the complete allocation so the same range may be reused under a
new generation.

A failed Hypervisor.framework send remains nontransactional: callers cannot
infer whether the guest observed the message and must not blindly retry. The
transport records pending MSI-X state where the virtio specification permits
later delivery; otherwise a device-specific failure policy owns the ambiguity.
VM teardown takes the same sender lock and revokes every retained clone before
unmapping or destroying VM-owned resources.

This foundation is not Firecracker's KVM-backed GICv3 ITS implementation and
adds no interrupt remapping. Its MSI-X use serves focused conformance endpoints
and the public all-virtio startup path described below. It is outside the
accepted native-v1 snapshot profile. Create completes the live-storage handoff
before rejecting PCI ahead of native-state capture and artifact work; load
retains its pre-file/grant/controller/VM-mutation rejection. The exact
`--enable-pci` argument leaf
and the live device-hotplug, runtime-manager, and PCI/MSI/coexistence aggregate
records are promoted; PCI snapshot persistence remains under audit.

## PCI Segment and All-Virtio Startup

The GICv2m frame composes with a backend-neutral PCI segment 0, bus 0. Device 0
is the fixed `[8086:0d57]` host bridge, and devices 1 through 31 use
deterministic, generation-bound slot leases. Focused signed modes retain the
identity-only `[0042:0000]` mock, modern virtio-rng, and static data-device
proofs. Product `--enable-pci` instead publishes balloon, block, network, pmem,
vsock, entropy, and virtio-mem in that Firecracker order after preflighting the
complete endpoint, slot, BAR, and dispatcher-region demand plus exact fixed
MSI-X demand and worst-case three-vector headroom for every remaining runtime
slot.

The arm64 plan reserves the full Firecracker configuration aperture at
`0x70000000..0x80000000`, publishes only the bus-0 1 MiB ECAM window, uses
`0x40003000..0x70000000` for 32-bit BAR ownership, and uses 256–512 GiB for
64-bit BAR ownership. Local lowest-fit allocators return provenance- and
generation-bound leases, deterministically reuse released ranges, and reject
foreign or stale release. Type-0 configuration supports fixed 32/64-bit BAR
encoding and one-shot all-ones size probing; guest relocation writes do not
move an owned range.

ECAM publication uses one atomic MMIO owner lease. The complete region batch,
handler ID, owner provenance, dispatcher provenance, and generation are
validated before mutation; checked release removes only the exact registered
state. The FDT path validates the reserved aperture and BAR windows against
RAM, GIC/GICv2m, and every published platform/MMIO device before emitting a
`pci-host-ecam-generic` node with `msi-parent = <3>`. With the option absent,
the previous FDT bytes and startup inventory are unchanged.

The backend-neutral virtio core now owns feature negotiation, queue state,
activation, reset, and exact queue/config interrupt intents; virtio-MMIO is an
adapter over that core. The modern PCI adapter publishes Firecracker's 512-KiB
capability BAR with common, ISR, device, notification, PCI-config, MSI-X table,
and PBA regions. It supports checked capability chaining, guest feature and
queue programming, notification decoding, ISR semantics, arbitrary guest
MSI-X address/data programming constrained by the device registry, masking and
pending-bit delivery, and ordered publication/teardown. Every currently
configured virtio class has typed PCI operations over the same canonical device
state. Product mode structurally suppresses all corresponding legacy
SPI/MMIO/FDT registrations while retaining host adapters, wakeups, limiter
retries, metrics, device updates, balloon controls, memory mutation, and flush
semantics.
The authority-free MMDS packet path is runtime-owned and shared by the public
MMIO process path and the PCI conformance harness. All platform attachment
remains MMIO.

The signed Linux conformance gate binds the standard `virtio_rng` driver,
programs distinct queue and configuration MSI-X vectors, consumes deterministic
bytes through `/dev/hwrng`, and observes both vectors independently. Additional
signed gates perform block read/write/`fsync`, pmem read/write/flush, and MMDS
request/response through static modern endpoints, require stable PCI identities
and distinct queue/configuration MSI-X vectors, and prove the data classes have
no simultaneous virtio-MMIO publication. Teardown unpublishes in reverse order
before VM destruction; the lower-level endpoint gate also rejects stale state
and proves exact slot, BAR, and GICv2m vector reuse. The signed product gate then
boots all seven virtio classes together, requires deterministic BDFs and no
virtio-MMIO nodes, and performs positive block, MMDS/network, pmem, vsock,
balloon, entropy, and virtio-mem interrupt/I/O. Separate direct and contained
signed gates perform two manual guest rescan/removal rounds for runtime block,
pmem, and network PUT/DELETE and prove exact resource reuse. Focused aggregate
tests pin type-scoped IDs, global network-MAC uniqueness, mixed configuration
truth, concurrent owner-thread serialization, and one shared 31-endpoint
budget that reopens after any runtime device class leaves. Automatic guest
notification, PCI snapshot persistence, and externally certified vmnet
connectivity remain deferred; the default product transport remains MMIO.

## Internal PSCI Power Sessions

Internal HVF boot sessions now compose the ordered owner topology, concurrent
run coordinator, and PSCI power model. Every verified MPIDR feeds the arm64 FDT;
only index 0 receives the initial Linux boot registers, while secondaries remain
offline until `CPU_ON32` or `CPU_ON64`. `AFFINITY_INFO32/64` reports the same
`OFF`/`ON_PENDING`/`ON` model used for scheduling.

`CPU_ON` validates an aligned entry inside mapped guest RAM, installs the entry
and context on the target owner thread, and submits only that target. The caller
does not receive `SUCCESS` until the identified target run is admitted. Caller
completion is then committed before the target becomes logically `ON`; any
post-admission failure terminates the session with indexed evidence instead of
pretending the target returned to `OFF`. Per-vCPU virtual-timer PPIs use the
completing member index.

`CPU_OFF` reserves the calling CPU in the same power model and returns `DENIED`
when it is the last committed `ON` CPU. A successful call consumes the exact
pending runner token without writing X0, removes that member from scheduling,
and commits it `OFF`. A later `CPU_ON` reuses the same owner, MPIDR, and GIC
topology. Re-entry writes the retained `SCTLR_EL1` to zero before applying the
existing Linux X0-X3, PSTATE, and PC-last entry contract; this is a narrow
warm-entry reset and not a claim of complete architectural cold reset.

`CPU_SUSPEND32/64` is a separate retained transaction for an online caller.
The member stays logically `ON`, so peer `AFFINITY_INFO` remains `ON`, while
normal guest execution is replaced by exact-token virtual-timer waits on the
same owner thread. The implementation ignores power-state, entry, and context
arguments, publishes the configured timer PPI before completing X0 with
`SUCCESS`, and preserves the transaction across wakeup/pause cancellation.
Stop, shutdown, and terminal drains do not invent a wake response.

Public process startup now uses this capability for the host-limited range
`1..=min(32, host_max)` while native-v1 capture/load remains a strict one-vCPU
profile. Guest CPU off/re-entry does not change public topology; `CPU_SUSPEND`
is limited to retained EL1 virtual-timer wake without FDT idle-state discovery;
dynamic CPU add/remove remains deferred. Exact expert-controlled masks for all
eleven reviewed arm64 identification registers, ACTLR.EnTSO, and the reviewed
U32/U64/U128 core/SIMD profile are applied to the complete owner topology
before boot overrides, with requested-set read-before-write preflight and
immediate readback. ZFR0/SMFR0 have a public macOS 15.2 pre-VM gate, and every
other KVM/public-HVF family has a stable terminal value-free classification.
Firecracker
v1.15.1 enables KVM's PSCI 0.2 vCPU feature while KVM exposes its latest
compatible PSCI 1.0 revision; bangbang matches that runtime contract and
coordinates it explicitly above Hypervisor.framework's per-vCPU owner threads.

## Arm64 Guest Cache Presentation

Ordinary arm64 HVF startup now establishes the guest cache hierarchy before it
creates a VM. One retained default `hv_vcpu_config_t` supplies
`ID_AA64MMFR2_EL1`, `CTR_EL0`, `CLIDR_EL1`, `DCZID_EL0`, and all data/unified
then instruction `CCSIDR_EL1` slots. Only CLIDR-selected levels are decoded;
legacy and FEAT_CCIDX field layouts are checked separately, and reserved bits,
overflow, inconsistent minimum line sizes, invalid DC ZVA metadata, unsupported
split outer caches, and levels above L3 fail closed.

Raw HVF geometry is not sufficient to claim host sharing. Startup also reads
the public `hw.nperflevels` and `hw.perflevelN.*` sysctls, requires exactly one
performance level to match the L1/L2/L3 sizes, and obtains `cpusperl2` and
`cpusperl3` only from that unique match. Missing, malformed, mismatched,
ambiguous, or non-nested facts reject startup before VM/GIC creation or guest
memory mapping. Performance-level physical/logical CPU counts validate those
sharing factors but do not cap the configured guest count; a final guest cache
group may therefore contain fewer vCPUs than the public sharing factor. No
scheduler affinity, private Apple API, or Apple-model table is used.

The runtime FDT model validates exact positive size/line/set/way geometry and a
nested cache graph. Per-CPU nodes carry split instruction/data or unified L1
properties. Shared unified L2/L3 nodes use deterministic names and descending
phandles, and every CPU or inner cache links directly to its next level.
Parsed-FDT tests cover one CPU, partial L2 groups, and nested L2/L3 groups;
signed Linux coverage compares level, type, size, line size, sets, ways, and
shared CPU lists from guest sysfs against the exact retained production model.

The same startup cache source is retained for native-v1 capture. Capture does
not query a new default configuration: it cross-checks the runner's
`ID_AA64MMFR2_EL1` and encodes the retained manifest, preserving native-v1
bytes and schema. Restored sessions reconstruct that compatibility source from
the already-validated artifact, but expose no reconstructed FDT hierarchy
because cache presentation is not part of the native-v1 schema.

## Arm64 CPU-Template Subset

Bangbang retains Firecracker's bounded custom aarch64 values losslessly and
implements expert masks for eleven U64 identification registers
(`PFR0/PFR1`, `DFR0/DFR1`, `ISAR0/ISAR1`, `MMFR0/MMFR1/MMFR2`, `ZFR0`, and
`SMFR0`), ACTLR.EnTSO, U64 X0 and X4-X30 plus
SP_EL0/PC/PSTATE/SP_EL1/ELR_EL1/SPSR_EL1, U128 Q0-Q31, and U32 FPCR/FPSR.
ZFR0/SMFR0 require a public macOS 15.2 availability check before VM creation;
ACTLR filters are confined to bit 1. Q transport is explicitly little-endian;
FP scalar reads must fit U32 before any write. X1-X3 are boot-reserved and
AArch32 banked SPSRs are unavailable. MIDR/MPIDR, control/context/debug/key/
timer/GIC/SME/EL2 families, aliases and unnamed encodings, and KVM-only classes
have distinct value-free terminal reasons rather than a generic raw-system
fallback. Complete mixed-width baseline collection across all vCPUs precedes
mutation; each common target is written and immediately reread on its owner
thread before boot resource assembly. Primary and secondary boot setup then own
X0-X3/PC/PSTATE as appropriate. A failure destroys the unpublished topology
and VM.

Static `V1N1` is configuration-only: it is GET-visible and replaceable by
custom or `None`, but an effective selection fails before backend construction
because Apple Silicon cannot provide its documented Neoverse V1 source model.
KVM capability/feature namespaces are separate strict platform exclusions.
Exact bounds, replacement/GET/snapshot behavior, expert-risk limits, signed
evidence, and remaining Wave 7 helper/portability ownership are in the checked
[CPU-template contract](../compat/firecracker/v1.16.0/cpu-template-contract.md).

## Internal Concurrent vCPU Run Coordination

The ordered HVF topology is consumed by an internal concurrent
bounded-run coordinator. It submits one identified generation to every online,
idle member before collecting completions, keeps one shared MMIO dispatcher,
accepts out-of-order owner-thread results, and routes indexed boot-register,
deferred PSCI, and GIC PPI operations without exposing the topology's runner
storage or raw HVF vCPU identifiers. Each vCPU remains permanently owned by its
original runner thread.

Wakeup, pause, stop, and shutdown requests freeze submission, snapshot only the
currently active generations, issue one slice-level `hv_vcpus_exit`, and publish
their barrier only after every member in that exact snapshot acknowledges.
Concurrent reasons coalesce as shutdown/stop, pause, then wakeup. A successful
exit request records per-member cancellation debt so a normal-completion race
cannot turn a delayed cancellation into false guest progress on the next run.
Offline members are never submitted or included in the cancellation slice.
Runner failures and terminal guest results use the same peer-drain path and a
stable topology-indexed precedence independent of completion arrival order.

Signed lifecycle coverage runs two real vCPUs against separate guest entry
points. Each writes a shared-memory flag and waits for its peer before the host
issues one active-only stop barrier; both identified runs must return
`Canceled`, and the complete create/run/cancel/shutdown/VM teardown sequence is
repeated. Signed `guest_boot` coverage additionally boots a two-vCPU Linux
session, validates FDT CPU nodes for MPIDRs `[0, 1]`, pins a deterministic tiny
init to CPU1 with `sched_setaffinity`, verifies CPU1 with `getcpu`, and emits a
fixed marker without sleeps. Public startup and native-v1 remain one-vCPU.

## Firecracker Model Alignment

bangbang should follow Firecracker's process model: one `bangbang` process
manages one microVM. Future API work should keep the control plane outside the
guest execution fast path.

## Firecracker v1.16.0 Remaining-Device Audit

The remaining-device baseline is pinned to Firecracker
[`v1.16.0`](https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/CHANGELOG.md#L9-L116)
at commit `d83d72b710361a10294480131377b1b00b163af8`. “Implemented” below means the
documented macOS/HVF virtio-MMIO subset, not Linux/KVM mechanism or complete
optional-device parity.

| Firecracker delta | bangbang classification |
| --- | --- |
| [#5786 PCI hotplug/hot-unplug](https://github.com/firecracker-microvm/firecracker/pull/5786) for block, pmem, and net | Implemented for non-root block/pmem and network devices in the public PCI profile, including transactional Running/Paused PUT/DELETE, manual guest rescan/removal, exact owner cleanup, and capacity reuse. Pmem additionally owns a direct file-backed mapping lease, exact-prefix flush/unmap, and same guest-range reuse. Network coordinates per-entry MMDS-only or vmnet packet I/O with PCI, metrics, limiter retry, and live-config ownership; uncertain cleanup is terminal. Default virtio-MMIO and runtime root block/pmem mutation retain nonmutating rejection. |
| [#5789 pmem rate limiting](https://github.com/firecracker-microvm/firecracker/pull/5789) | Implemented for the supported pmem subset. Like [Firecracker's queue](https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/src/vmm/src/devices/virtio/pmem/device.rs#L362-L452), a non-empty coalesced event charges one operation plus the exact backing length before flush; bangbang retains throttled work for a session-owned retry and supports atomic live limiter replacement. |
| [#5906 64-byte aarch64 `rng-seed`](https://github.com/firecracker-microvm/firecracker/pull/5906) and [#5762 64-KiB virtio-rng cap](https://github.com/firecracker-microvm/firecracker/pull/5762) | Implemented. The pinned upstream source shows the [64-byte FDT property](https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/src/vmm/src/arch/aarch64/fdt.rs#L275-L283) and [64-KiB queue bound](https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/src/vmm/src/devices/virtio/rng/device.rs#L31-L35). |
| [#5760 VMGenID ACPI HID](https://github.com/firecracker-microvm/firecracker/pull/5760) | Not applicable to bangbang's aarch64 DeviceTree-only device. Its `microsoft,vmgenid` node matches the pinned [Firecracker DeviceTree shape](https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/src/vmm/src/arch/aarch64/fdt.rs#L289-L299), and native-v1 load replaces the generation and notifies the guest. |
| [#5793 cross-slot virtio-mem updates](https://github.com/firecracker-microvm/firecracker/pull/5793) | Implemented at bangbang's block-owned/HVF-mapping abstraction. Firecracker updates [every intersecting KVM slot](https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/src/vmm/src/devices/virtio/mem/device.rs#L502-L554); bangbang does not expose KVM slot identity and instead proves adjacent, partial, cross-conceptual-slot, and rollback behavior over exact dynamic mappings. |
| [#5794 balloon statistics bound](https://github.com/firecracker-microvm/firecracker/pull/5794) and [#5884 hinting `204`](https://github.com/firecracker-microvm/firecracker/pull/5884) | Implemented. Statistics are bounded to the same [256-tag limit](https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/src/vmm/src/devices/virtio/balloon/device.rs#L48-L52); hinting routes return `204 No Content`. |
| [#5818 virtio initialization/status sequencing](https://github.com/firecracker-microvm/firecracker/pull/5818) | The new PCI sequencing is transport-limited; existing virtio-MMIO ordered initialization and clear-bit rejection except reset are implemented and tested. |
| [#5809 x86 KVM clock restore](https://github.com/firecracker-microvm/firecracker/pull/5809) | Platform/profile-limited. It is not the aarch64 startup VMClock contract; mutable VMClock restore/signaling remains outside native-v1. |

Other v1.16.0 changelog entries are not silently absorbed into this device
scope. [#5824 serial limiting](https://github.com/firecracker-microvm/firecracker/pull/5824)
is covered by the implemented TX limiter, while
[#5799 log callsite limiting](https://github.com/firecracker-microvm/firecracker/pull/5799)
remains in the broader observability scope. Network MTU
[#5828](https://github.com/firecracker-microvm/firecracker/pull/5828) and vsock
`EVENT_IDX` [#5872](https://github.com/firecracker-microvm/firecracker/pull/5872)
are implemented in their owning live subsets. Vsock restore changes
[#5323](https://github.com/firecracker-microvm/firecracker/pull/5323) and
[#5882](https://github.com/firecracker-microvm/firecracker/pull/5882) remain
explicit native-v1 optional-device exclusions. UART restore
[#5764](https://github.com/firecracker-microvm/firecracker/pull/5764) is covered
only by bangbang's exact native-v1 UART profile, not Firecracker artifact
compatibility. Aarch64 cache visibility
[#5780](https://github.com/firecracker-microvm/firecracker/pull/5780) belongs to
machine/CPU topology, and x86 KVM MSR coverage
[#5738](https://github.com/firecracker-microvm/firecracker/pull/5738) is
non-applicable to arm64 HVF. Linux host-kernel support is likewise a platform
boundary rather than a macOS device claim.

Bangbang completion evidence is equally exact. The merged implementation PRs
are [#1334 virtio-mem](https://github.com/seven332/bangbang/pull/1334),
[#1335 targeted pmem flush](https://github.com/seven332/bangbang/pull/1335),
[#1336 pmem limiting](https://github.com/seven332/bangbang/pull/1336),
[#1337 Darwin discard](https://github.com/seven332/bangbang/pull/1337), and
[#1338 free-page reporting](https://github.com/seven332/bangbang/pull/1338).
The pinned signed executable source contains the exact
[balloon reporting](https://github.com/seven332/bangbang/blob/1bffe45784cc2d627adb8419b85453ec82b3fa71/crates/bangbang/tests/executable_hvf_e2e.rs#L1887),
[virtio-mem lifecycle](https://github.com/seven332/bangbang/blob/1bffe45784cc2d627adb8419b85453ec82b3fa71/crates/bangbang/tests/executable_hvf_e2e.rs#L2127),
[PL031](https://github.com/seven332/bangbang/blob/1bffe45784cc2d627adb8419b85453ec82b3fa71/crates/bangbang/tests/executable_hvf_e2e.rs#L2369),
[VMClock discovery](https://github.com/seven332/bangbang/blob/1bffe45784cc2d627adb8419b85453ec82b3fa71/crates/bangbang/tests/executable_hvf_e2e.rs#L2465),
[entropy](https://github.com/seven332/bangbang/blob/1bffe45784cc2d627adb8419b85453ec82b3fa71/crates/bangbang/tests/executable_hvf_e2e.rs#L2695),
[pmem limiter/flush](https://github.com/seven332/bangbang/blob/1bffe45784cc2d627adb8419b85453ec82b3fa71/crates/bangbang/tests/executable_hvf_e2e.rs#L2808),
and [native-v1 VMGenID replacement](https://github.com/seven332/bangbang/blob/1bffe45784cc2d627adb8419b85453ec82b3fa71/crates/bangbang/tests/executable_hvf_e2e.rs#L5385)
cases. These are guest-visible gates; the validation matrix keeps broader
focused backend coverage separate.

Firecracker's aarch64
[PL031 node has no interrupt property](https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/src/vmm/src/arch/aarch64/fdt.rs#L443-L456),
so bangbang's no-alarm PL031 is an implemented Firecracker aarch64 subset rather
than a missing interrupt implementation. ARM PVTime remains platform-limited:
Firecracker allocates and registers
[one KVM-backed 64-byte region per vCPU](https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/src/vmm/src/builder.rs#L558-L600),
while an HVF execution-time observation alone is not that shared-page guest ABI.

Balloon inflate, accepted hinting, and free-page reporting use whole-range
validation, per-owner segmentation, inward host-page alignment, and Darwin
zero-before-free advice. Pinned Apple XNU maps
[`VM_BEHAVIOR_FREE` to `VM_SYNC_KILLPAGES`](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/osfmk/vm/vm_map.c#L16745-L16759),
[stops at map holes](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/osfmk/vm/vm_map.c#L21220-L21237),
[rounds destructive work inward](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/osfmk/vm/vm_map.c#L21331-L21349),
and documents deactivation as clearing modified state and
[forgetting page changes](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/osfmk/vm/vm_object.c#L2620-L2652).
This supports zero-safe best-effort reclaimability. It does not promise
synchronous RSS or footprint reduction and does not use paired reusable-page
accounting.

## Firecracker v1.16.0 Observability Contract

Bangbang implements a process-local logger, interval metrics writer, and TX-only
serial output subset. Compatibility here describes observable records, trigger
and failure behavior, stable field names, and ownership; it does not require
Firecracker's Linux timerfd/eventfd plumbing, global metric/logger statics, or
lock-free packed limiter representation.

### Logger records and delivery

Logger output is silent by default because no sink is configured. `PUT /logger`
and the matching CLI flags can open one process-local file or FIFO with
append/create and `O_NONBLOCK` semantics. Open errors and later diagnostics do
not echo the configured path. Level, optional level/origin prefixes, and module
prefix matching filter records before delivery.

Successfully parsed API requests and successful `InstanceStart` and explicit
`FlushMetrics` actions are unrestricted host records: they do not consume the
guest-triggered limiter. Request records contain only method and path, never
request bodies. The boot timer is the one bounded guest-triggered logger
callsite. It admits an initial burst of ten records, refills the five-second
budget at one token per 500 ms, increments
`logger.rate_limited_log_count` for every denied record, and emits one
unrestricted `Warn` recovery record before the next admitted boot-time record.
Unconfigured or filtered records do not consume the limiter and are not missed
deliveries.

Sink locking never waits. Lock contention or poisoning and write or flush
failure increment the saturating `logger.missed_log_count`; they never change
an API response, action result, VM startup result, or guest boot-timer MMIO
result. A rate-limited record is counted as rate-limited rather than missed.

### Metrics field and transaction model

Every implemented event total is an interval increment: deprecated, GET, PUT,
PATCH, logger, signal, and UART counters; block, pmem, network, MMDS, vsock,
entropy, RTC, and balloon counts, byte totals, failures, errors, and limiter
activity; and block latency `sum_us`. This applies to aggregate and stable-key
per-drive, per-pmem-device, and per-interface objects. `*_bytes` fields are
bytes, `*_us` fields are microseconds, and count/failure/event fields count the
named events.

The stores repeated with their latest value are process startup wall/CPU
elapsed time, `boot_run_loop_status`, the most recent successful lifecycle or
admitted snapshot action latencies, and block latency `min_us`, `max_us`, and
`sample_count`. Each completely written line also contains bangbang's
non-upstream `vmm.metrics_flush_count: 1` marker for that successful line.

The process keeps one typed previous-successful snapshot. A line is derived
against that snapshot and the baseline advances only after the complete write
succeeds. A monotonically increasing producer emits `current - previous`; a
new or reset generation whose current value is lower emits its full current
value. New keyed devices start from zero, disappeared devices are omitted, and
reappearing or replaced same-ID producers follow the same reset rule. A failure
increments `logger.missed_metrics_count` and retains the old baseline. Because
a nonblocking writer can accept bytes before reporting an error, the next
success deliberately replays the uncommitted interval with at-least-once rather
than exactly-once semantics.

The JSON schema stays sparse. Empty optional device families and empty keyed
objects are omitted; bangbang does not synthesize a zero-filled Firecracker
schema for absent or unimplemented devices. Omission is not a support claim.
Issue #717 remains `NOT_PLANNED` because Firecracker exposes no
`GET /vm/config` request metric field, so bangbang does not invent one. Issue
#738 remains `NOT_PLANNED` because Firecracker's metrics write-failure path
increments `missed_metrics_count`, not `logger.metrics_fails`; bangbang has no
matching producer for the latter.

### Metrics triggers and errors

Configuring a metrics sink writes nothing before a VM session exists. The
first retained session causes one best-effort initial attempt regardless of
whether the sink came from CLI, config file, or API configuration. The
periodic scheduler is dormant preboot, anchors its first deadline 60 seconds
after session creation, runs in both `Running` and `Paused`, and schedules the
next deadline after an unconfigured no-op, success, or failure. Automatic
initial and periodic failures do not change the action, API loop, or process
result.

Explicit `FlushMetrics` is different: it is a runtime-only API action, is
rejected before startup, records its API/action effects, and returns a sink
failure to its caller. While the process still owns the retained session and
live diagnostics, every normal API or no-api convergence path makes one
best-effort terminal attempt and then returns the original success or error.
This includes handled shutdown, guest terminal outcomes, worker terminal
errors, and ordinary bind/wait/server errors; it does not add process-global
panic-hook or fatal-signal durability.

### Serial output and native-v1

`PUT /serial` stores a nullable public output path and optional byte token
bucket before boot. A configured file or FIFO is opened nonblocking with
path-redacted errors. With no path, guest TX goes to one bounded 64-KiB internal
capture buffer rather than stdout. There is no public serial RX, stdin route,
or streaming API. An exhausted limiter drops bytes without sleeping or failing
the guest write; the interval `uart` object reports implemented TX writes,
missed writes, output errors, and rate-limiter dropped bytes. Read and flush
fields remain zero because the TX-only implementation has no such producers.

Bangbang-native v1 accepts only `SerialConfig::default()`. Its device state
captures the serial MMIO metadata plus interrupt-enable, line-control,
modem-control, scratch, and both divisor-latch register bytes. Restore creates
a fresh bounded output buffer with empty UART metrics. Buffered or in-flight TX
bytes, a public path, limiter configuration or budget, and UART counters are
not captured. This exact local profile is not Firecracker snapshot-artifact
compatibility.

### Stable product boundaries

The ordinary CLI has no production rotation, syslog, journald, tracing, remote
telemetry, or resource-broker policy. Logger and metrics state remains
process-owned rather than global, so there is no panic/fatal-signal durability
claim. These named product and architecture boundaries, the sparse metrics
schema, and the serial RX/stdout/native-v1 limits replace an open-ended “full
logging and metrics” placeholder.

The ordinary `bangbang` CLI remains the direct, uncontained process entry point.
The separate production `Bangbang.app` entry point has a fixed unsandboxed
launcher and one nested `dev.bangbang.worker` VMM. The worker is separately
signed with exactly App Sandbox and Hypervisor entitlements, while the launcher
has neither; both use Hardened Runtime. Assembly is private and no-clobber,
inspects both code objects before exclusive publication, and runtime launch
fails closed on wrong placement, symlinks, missing or modified code, signature,
identifier, or required-entitlement failures.

The launcher preserves ordinary worker argument bytes or accepts one exact
outer `--bangbang-jailer-v1 ... --` policy before the existing grant envelope.
That policy binds the fixed executable, current uid/gid, one validated ID,
launcher-owned timing, repeatable last-value `fsize`/`no-file` values, and
optional daemon mode. Darwin's default-close spawn policy supplies only a
private marker environment, standard streams, and fixed lifecycle,
startup-grant, dormant vsock-broker, and dedicated vhost-user-broker endpoints
to the worker.
It validates the suspended live worker, resumes only the
private bootstrap, authenticates the child-attributed socket peer and live code,
and then binds a random 256-bit identity to a versioned, 4-KiB-bounded protocol
with exact sequences, closed lifecycle messages, and a fixed reserved-zero
`Start(WorkerPolicy)`. Worker authentication of the launcher is intentionally
asymmetric: it verifies matching real/effective credentials, process session,
and direct-parent PID before policy application, while App Sandbox denies its
independent Security.framework lookup of the parent. The worker installs and
reads back exact soft/hard `RLIMIT_FSIZE` and `RLIMIT_NOFILE` without raising an
inherited hard limit; production no-file defaults to 2048.

The worker creates, locks, and descriptor-enters one exact mode-0700 empty
namespace in its container before `Prepared`; the launcher independently verifies name, owner,
mode, device, inode, emptiness, and live lock before `Proceed`. After that gate,
socket publication may add only fixed strict role/child/identity ownership
records. `Starting`,
committed API/no-API `Ready`, one cancellation, and path-free `Terminal` state
are monotonic and sequence-checked. A surviving side performs exact-inode
cleanup, and a later worker performs bounded unlocked-empty recovery after both
sides are killed. Signed Apple Silicon evidence covers malformed bootstrap and
closed environment/fd policy, exact limits plus real `EMFILE`/`SIGXFSZ`, private
cwd, worker-first/launcher-first/both-killed cases, concurrent-session
isolation, both graceful signals, container API service, exact external startup
config/metadata/kernel/initrd grants, repeatable block/pmem grants, delayed
atomic claims that retain opened identities after pathname replacement,
failure-atomic mismatch handling, read-only guest-write rejection, writable
block and pmem persistence, preauthorized live block replacement, exact
write-only logger/metrics/serial grants, outside-container API service, both
granted-vsock initiation directions, and real sandboxed HVF guests ending
through PSCI `SYSTEM_OFF`. It also proves external native-v1 snapshot
create/describe/state-memory-root restore and exact staging cleanup after worker
death.

`--daemonize` re-executes the same validated outer code with default-close
`SETSID`, `/dev/null` standard streams, a marker-only environment, and one
closed handoff fd. The re-executed launcher remains the worker supervisor. The
original returns one PID only after worker readiness and exact acknowledgment;
parent loss before acknowledgment cancels the unpublished session, while later
SIGINT/SIGTERM to that PID follows normal reap and cleanup. Signed evidence also
covers two simultaneous daemon supervisors and peer survival after one stops.

This is macOS containment, not direct Linux jailer/seccomp equivalence. The
session namespace itself grants no host resource. The bounded startup channel
provides external descriptor authority, and contained config, metadata, kernel,
and initrd consumers adopt exact read-only grants without reopening tagged path
strings. Block and pmem consumers similarly adopt exact repeatable grants with
access derived from device intent at config-file, API startup, and the existing
live block replacement seam. Logger, metrics, and serial consume singleton
write-only grants. API and vsock consume exact singleton directory anchors plus
one bounded safe child; short-lived binders perform same-filesystem exclusive
publication, while guest-initiated vsock connections use one fixed port-only
launcher facet with no guest bytes or outgoing-network entitlement. Snapshot
describe/state/memory/root consumers adopt exact files; create retains
repeatable output anchors with bounded children and strict crash-cleanup
records. General dynamic post-Ready delivery, hard revocation, cross-filesystem
socket publication, vmnet provisioning and policy, arbitrary uid/gid transition,
configurable chroot, launch constraints, Developer ID possession, automatic
restart, and notarization remain later work. The exact Linux seccomp, cgroup,
network-namespace, and PID-namespace mechanisms now have terminal macOS
platform exclusions; this does not make the surrounding aggregate jailer or
production-host records complete.

The macOS host security baseline is documented separately in
[macOS Host Security Model](security.md). That document records the current
socket, host-path, HVF entitlement, guest-data, and multi-process boundaries, and
also records Linux Firecracker hardening features that are not implemented by the
current macOS/HVF scaffold.

The concise support-status and test-layer summary is maintained in
[Firecracker Validation Matrix](firecracker-validation-matrix.md). This document
remains the detailed source for endpoint behavior, field policy, compatibility
rationale, and platform limits.

### Capability inventory authority

The checked
[Firecracker v1.16.0 capability inventory](../compat/firecracker/v1.16.0/README.md)
is the structural scope authority for exhaustive compatibility work. Its
machine-owned source manifest pins exact Swagger, executable CLI, non-Swagger
route, public-tool, and source-corpus identities. Its separate human overlay
owns dispositions and evidence so regeneration cannot manufacture support or
erase an unresolved contract.

This document remains the detailed behavioral explanation and an evidence
target for inventory records. It is not, by itself, proof that every upstream
identity has been audited. In particular, historical landing notes, family-level
`implemented` language, parser recognition, stable unsupported behavior,
`partial`, `deferred`, and product/profile limits must not be promoted to
`implemented-and-verified` without record-specific implementation and
validation evidence under the inventory rules.

During #1348 delivery, `audit-required` and `missing-platform-feasible` remain
visible nonterminal states. Final validation rejects both. A
`proven-platform-impossible` record requires the exact upstream contract,
authoritative platform evidence, alternatives, stable public behavior, focused
tests, compatibility/security documentation, and a current Challenge result.
The inventory foundation itself changes no runtime behavior.

After #1389/#1390 lifecycle delivery and #1391 machine sizing/page policy, the
then-current 417-record delivery inventory contained 38 `implemented-and-verified`, 366
`audit-required`, three `missing-platform-feasible`, and ten
`proven-platform-impossible` records. Eight exclusions are
`corpus:seccomp`, both executable seccomp leaves, and the five jailer
cgroup/network/PID-namespace leaves; the other two are the exact machine `2M`
property and pinned hugepages corpus. Broad aggregates retain independent
handoffs.

Following the remaining #1388 slices and the #1408 closure audit, the generated
source manifest has 381 identities and the delivery overlay adds 37 local
semantic identities. After the #1420 block, #1421 pmem, #1422 network, and
#1423 aggregate runtime-hotplug promotions, the then-current 418 records contained 81
`implemented-and-verified`, 317 `audit-required`, three
`missing-platform-feasible`, and 17 `proven-platform-impossible` outcomes. The
[machine and lifecycle closure ledger](../compat/firecracker/v1.16.0/machine-lifecycle-audit.md)
accounts for the original 28 records and the directly related boot-source,
machine, CPU, and VM-state aggregates. Generalized snapshots remain with Wave
6, public tools and applicable broad specifications with Wave 7, and final
cross-capability/export certification with Wave 8.

After #1444-#1449, #1461, #1462, and the #1471 aggregate storage closure, the
then-current 418 records contained 114 `implemented-and-verified`, 284
`audit-required`, three `missing-platform-feasible`, and 17
`proven-platform-impossible` outcomes. The checked
[storage closure contract](../compat/firecracker/v1.16.0/storage-contract.md)
accounts for exactly 40 directly owned identities: 38 are terminal with
record-specific implementation and validation evidence, while exactly
`corpus:pmem` and
`semantic.storage:pmem-root-mapping-flush-and-state` remain with Wave 6 for
optional-device serialization and restore.

After #1473, the checked balloon closure promotes its 50 API
operation/path/property/schema leaves. The current 418 records contain 164
`implemented-and-verified`, 234 `audit-required`, three
`missing-platform-feasible`, and 17 `proven-platform-impossible` outcomes.
Exactly `corpus:ballooning` and
`semantic.memory-device:balloon-oom-stats-hinting-and-reporting` retain the
Wave 6 balloon encoding, artifact, restore, migration/clone, portability, and
signed restored-guest handoff.

The intended public control plane is Firecracker-style HTTP over a Unix domain
socket. The implemented `GET /`, `GET /version`, `GET /vm/config`,
`GET /machine-config`, `GET /hotplug/memory`, pre-boot
`PUT /machine-config`, pre-boot `PUT /boot-source`, pre-boot
`PUT /drives/{drive_id}`, pre-boot `PUT /network-interfaces/{iface_id}`,
pre-boot `PUT /pmem/{id}`, pre-boot `PUT /vsock`, pre-boot
`PUT /hotplug/memory`, pre-boot `PUT /metrics`, pre-boot `PUT /logger`,
pre-boot `PUT /serial`, parsed `PUT /actions`, pre-boot
`PATCH /machine-config`, parsed `PATCH /mmds`, parsed
`PATCH /hotplug/memory`, runtime `PATCH /vm`, and runtime
`PATCH /drives/{drive_id}` requests already map through a minimal internal VMM
action/data boundary. Validation rejects malformed boot-source, memory-hotplug,
drive update, VM state update, and actions requests before VMM state mutation.
Successful `InstanceStart`, the `Running` transition, runtime
`Paused`/`Running` transitions through `PATCH /vm`, and one internal boot
run-loop worker are implemented with configured or bounded internal serial TX
output and retained active, paused, terminal-outcome, or error status.
Process-owned API-enabled and no-api runs exit successfully after guest PSCI
`SYSTEM_OFF` or `SYSTEM_RESET` and fail on non-success terminal worker states.
The logger, sparse interval metrics, initial/periodic/explicit/terminal trigger
rules, serial limiter, and precise native-v1 UART profile are the implemented
supported subset documented above. Public serial RX/streaming/default stdout,
process-global panic/fatal durability, and production telemetry facilities are
explicit boundaries rather than unqualified future “full” observability work.

## Offline Seccompiler Compatibility

The workspace provides a `seccompiler-bin` host tool with Firecracker v1.16's
five public argument names and short aliases. It accepts `x86_64` and
`aarch64`, the required JSON input, Firecracker's default
`seccomp_binary_filter.out`, deprecated `--basic`, and long-only
`--split-output`. Help and version identify this as bangbang's offline
compatibility tool. Invalid invocation exits 2; compilation or artifact I/O
failure exits 1. Non-help diagnostics are fixed categories that retain no
argument, path, policy, syscall, or OS-error value.

The compiler accepts exactly the pinned `vmm`, `api`, and `vcpu` policy shape,
resolves syscalls against the checked libseccomp v2.6.0 tables, preserves all
v1.16 actions and argument operators, and emits the same bad-architecture
`KILL_THREAD` behavior as v1.16's default libseccomp context. It caps input at
1 MiB, each thread at 1,024 rules, each rule at six conditions, and each
classic-BPF program at Linux's 4,096-instruction limit. The independent pure
Rust lowering may use a different instruction layout from libseccomp, but its
actions are the compatibility contract; implementation-time comparison against
the pinned aarch64 tool covered the shipped policy and 433,440 independent
syscall/architecture/argument cases.

Combined output serializes the ordered map with the exact pinned bitcode 0.6.9
Serde format and rejects output above Firecracker's 100,000-byte consumer cap.
Firecracker can deserialize it as `HashMap<String, Vec<u64>>`. Split output is
the raw little-endian `sock_filter` word stream in exactly `vmm.bpf`, `api.bpf`,
and `vcpu.bpf`; the requested output basename selects only their parent.

Input opens its final component no-follow, nonblocking, and close-on-exec,
requires a regular file, and performs one bounded UTF-8 read. Output retains a
no-follow directory descriptor, refuses symlink/directory/FIFO/socket targets,
stages complete owner-only synced files, probes required no-replace/exchange
rename flags before final mutation, and checks device/inode identities through
publication, rollback, and cleanup. Pre-completion observed failures restore
prior entries where identity proof remains available. Errors distinguish
rollback uncertainty from an already committed result whose directory sync or
private cleanup is uncertain. Each visible file is complete, but POSIX has no
single transaction spanning the three split names, so crash-atomic three-file
publication is not claimed.

This capability ends at offline artifact creation. macOS/HVF cannot install or
enforce Linux seccomp. #1384 terminally classifies Firecracker v1.16's current
filter reader, runtime corpus, and process flags as public-macOS platform
exclusions; the older install-helper wording in pinned `docs/seccompiler.md`
does not expand the host tool into a runtime API.

## Runtime Isolation Platform Exclusions

The following Linux identities have no equivalent current public macOS process
boundary. Their Firecracker names are rejected, not accepted as no-ops or
translated into narrower native controls:

| Firecracker input/corpus | Exact upstream effect | Stable macOS outcome |
| --- | --- | --- |
| `corpus:seccomp`, `--no-seccomp`, `--seccomp-filter PATH` | Select default, empty, or caller-loaded `vmm`/`api`/`vcpu` classic-BPF programs and install each nonempty program after `PR_SET_NO_NEW_PRIVS` with Linux `seccomp(SECCOMP_SET_MODE_FILTER)`. | `bangbang` reports only the first fixed unsupported name before filter-path/config-file access, VMM/backend construction, readiness, or API socket publication. Missing, separated, attached, duplicate, and both conflict orders are covered. Direct mode already has no Linux filter; App Sandbox remains an immutable signed boundary. |
| jailer `--cgroup`, `--cgroup-version`, `--parent-cgroup` | Select cgroup v1/v2, create/inherit controller hierarchies, write arbitrary controller files, and attach the PID through `tasks` or `cgroup.procs`. | The production parser returns a closed fixed-name category before grant parsing/preparation, bundle/profile work, private staging, session creation, spawn, publication, or worker execution. Darwin rlimits are scalar process limits, not cgroup identities. |
| jailer `--netns PATH` | Open the supplied namespace handle with no-follow and call `setns(CLONE_NEWNET)` before later jail setup. | The same early rejection never opens `PATH`. Network Extension is an entitled VPN extension, App Sandbox is access policy, and vmnet configures guest networking; none joins the host process to a path-named stack. |
| jailer `--new-pid-ns` | Call `clone(CLONE_NEWPID)` so the first child is PID 1 in a nested visibility domain. | The same early rejection precedes session/worker creation. Darwin process groups, sessions, supervision, and event monitoring retain host PID identity and visibility. |

The five jailer names are recognized only before the launch-policy delimiter and
are absent from successful help. Attached values are inspected only for their
fixed name, separated values are not consumed, lookalikes retain the generic
invalid-policy result, and post-delimiter worker argv stays opaque. Derived
`Debug` and fixed `Display` use a closed `JailerIsolationArgument` enum, so a
path, cgroup property, parent, PID, or policy value cannot enter the error.

Unit and direct process tests prove fixed errors and no socket/readiness for the
seccomp inputs. The separately signed production-bundle test combines each
jailer shape with a private invalid grant and socket request, then proves empty
stdout, exact redacted stderr, no socket, and unchanged session state. This is
ordering evidence for rejection before any public or persistent mutation, not
a claim that App Sandbox, rlimits, Endpoint Security, Network Extension, vmnet,
sessions, or supervision implements the Linux mechanism.

## Process Startup CLI

The current `bangbang` executable has a checked Firecracker v1.16.0 process
contract for all 23 configured argument names. Nineteen argument leaves have
implemented and verified process-facing behavior; both seccomp leaves are
terminal public-macOS platform exclusions; PCI and `--snapshot-version` remain
explicit cross-family handoffs. The exact audit and evidence are recorded in
[`compat/firecracker/v1.16.0/process-contract.md`](../compat/firecracker/v1.16.0/process-contract.md).
The executable binds a Unix socket and
serves `GET /`, `GET /version`, `GET /vm/config`, `GET /machine-config`,
`GET /hotplug/memory`, pre-boot `PUT /machine-config`, pre-boot
`PUT /boot-source` configuration storage, and pre-boot
`PUT /drives/{drive_id}` configuration storage, pre-boot `PUT /pmem/{id}`
configuration storage, runtime `PATCH /drives/{drive_id}` backing refresh,
pre-boot `PUT /network-interfaces/{iface_id}` configuration storage, pre-boot
`PUT /vsock` configuration storage, pre-boot `PUT /hotplug/memory`
configuration storage, pre-boot `PUT /metrics` output configuration, pre-boot
`PUT /logger` output configuration, pre-boot `PUT /serial` output
configuration, parsed `PATCH /hotplug/memory`, metrics and logger startup CLI
configuration, plus process-routed `PUT /actions` startup and metrics flush
with an internal boot run-loop worker across bounded step windows or
state/configuration faults. The process can also read `--config-file` for the
supported startup subset, start the VM before serving the API socket, and then
keep the API socket available for runtime requests. With `--no-api`, the same
supported config-file startup path runs without publishing an API socket and
exits on handled `SIGINT`, handled `SIGTERM`, or guest PSCI `SYSTEM_OFF` or
`SYSTEM_RESET`. Reset is a terminal process outcome, and external run-loop
management remains deferred.

| Argument | Current behavior | Compatibility notes |
| --- | --- | --- |
| `--api-sock <PATH>` | binds the API Unix socket | Firecracker defaults to `/run/firecracker.socket`; bangbang defaults to `/tmp/bangbang.socket` because macOS does not normally provide `/run`. This is an intentional host-platform difference. |
| `--http-api-max-payload-size <BYTES>` | configures the maximum accepted HTTP API request body size | Defaults to Firecracker's `51200` byte limit and accepts the complete non-negative `usize` domain. The configured value applies to the HTTP body declared by `Content-Length`; request-head bytes are bounded separately by bangbang's parser safety cap. A zero limit permits bodyless requests and returns `413 Payload Too Large` for every nonempty body. Malformed, overflowing, and duplicate values are rejected during argument parsing. |
| `--id <ID>` | parsed, validated, and stored | Defaults to Firecracker's `anonymous-instance`. IDs use Firecracker's 1-to-64 UTF-8-byte bound and accept `-` or any Unicode alphanumeric character. The exact accepted value is returned through `GET /`; punctuation, symbols, empty values, and byte-overlong multibyte values fail before readiness. |
| `--start-time-us <MICROS>` | parsed and reported in minimal metrics | Accepts non-negative `u64` microsecond values passed by Firecracker-style launchers. When provided, session-initial, explicit runtime, 60-second periodic, and normal-terminal metrics output includes `api_server.process_startup_time_us` as the sampled monotonic clock minus this value, saturating at zero for future timestamps. |
| `--start-time-cpu-us <MICROS>` | parsed and reported in minimal metrics | Accepts non-negative `u64` microsecond values passed by Firecracker-style launchers. When provided, session-initial, explicit runtime, 60-second periodic, and normal-terminal metrics output includes `api_server.process_startup_time_cpu_us` as the sampled process CPU clock minus this value, saturating at zero for future timestamps before adding optional parent CPU time. |
| `--parent-cpu-time-us <MICROS>` | parsed and reported in minimal metrics | Accepts non-negative `u64` microsecond values passed by Firecracker-style launchers. When `--start-time-cpu-us` is also provided, every emitted store value adds this value into `api_server.process_startup_time_cpu_us`; it is not serialized separately. |
| `--metrics-path <PATH>` | configures metrics output before API serving | Uses the same per-process metrics sink and redacted host-path error policy as `PUT /metrics`. A later duplicate `PUT /metrics` request fails without replacing this sink. |
| `--log-path <PATH>` | configures logger output before API serving | Uses the same per-process logger sink and redacted host-path error policy as `PUT /logger`. |
| `--level <LEVEL>` | configures logger level before API serving | Accepts the existing logger levels `Off`, `Trace`, `Debug`, `Info`, `Warn`, `Warning`, and `Error`; invalid levels fail before readiness with the bad-configuration exit status. Minimal API request, action, and boot-timer logs are emitted only when the configured level allows `Info`. |
| `--module <MODULE>` | filters implemented logger events | Matches the stored `PUT /logger` field and filters current logger events with Firecracker-style module-path prefix matching. API request method/path lines use `bangbang_runtime::api_server`, action logs use `bangbang_runtime::vmm_action`, and boot-timer logs use `bangbang_runtime::boot_timer`. |
| `--show-level` | enables level prefix for minimal logger events | Writes `level=Info` before minimal API request, action, and boot-timer log lines. |
| `--show-log-origin` | enables origin field for implemented logger events | Writes `origin=<file>:<line>` before API request, action, and boot-timer log messages. |
| `--boot-timer` | enables guest boot-time logging | Registers the Firecracker aarch64 pseudo-MMIO boot timer at `0x4000_0000`; a guest write of byte value `123` at offset `0` logs elapsed wall and process CPU time through the configured logger sink when level and module filters allow `Info` for `bangbang_runtime::boot_timer`. This is process observability state and is not exposed in `GET /vm/config`. |
| `--enable-pci` | selects all-virtio PCI startup on supported macOS arm64/HVF hosts | Exact flag syntax is immutable for the process. Required target and GIC/MSI symbols are checked before API/no-api readiness; one shared 31-endpoint slot/BAR/dispatcher budget plus exact fixed and worst-case runtime vector demand is checked before `Running`. Balloon, block, network, pmem, vsock, entropy, and virtio-mem use deterministic modern PCI functions while serial, RTC, boot timer, GIC, VMGenID, and VMClock remain platform MMIO devices. PCI mode omits the VMM-supplied `pci=off` and publishes only the PCI/GICv2m transport FDT; default startup remains all-virtio-MMIO. Native-v1 create/load rejects the PCI profile. Running/Paused non-root block, pmem, and network PUT/DELETE share type-scoped identity and one owner-thread inventory with manual guest coordination; PCI snapshots remain deferred. |
| `--mmds-size-limit <BYTES>` | configures the maximum serialized MMDS data-store size | When omitted, follows the effective HTTP API payload limit like Firecracker; with default HTTP settings this is `51200` bytes. The complete non-negative `usize` domain is accepted. A zero limit permits startup and rejects every serialized JSON object through the MMDS data-store-limit response. Malformed, overflowing, and duplicate values fail during argument parsing. |
| `--metadata <PATH>` | initializes MMDS data before API serving or no-api readiness | Reads a readable regular UTF-8 JSON metadata file up to 1 MiB and applies it through the same runtime validation and serialized data-store limit as `PUT /mmds`. Malformed files, non-object data, oversized files, duplicate object keys, empty paths, control-character paths, and missing-value inputs fail before readiness. |
| `--config-file <PATH>` | startup implemented for supported subset | Reads a Firecracker-shaped JSON configuration from a readable regular file up to 1 MiB, applies supported sections through the same validation path as matching API requests, and starts the VM with `InstanceStart`. In API-enabled mode, the API socket is published only after successful startup. Malformed files, oversized files, duplicate object keys, unknown sections, unsupported sections, or invalid sections fail before socket publication or no-api readiness. |
| `--help`, `-h` | prints help | Help describes the current API socket scope. |
| `--version`, `-V` | prints version | `-V` is retained from the existing bangbang scaffold. |
| `--snapshot-version` | implemented native-envelope inspection | Prints `v1.0.0` and exits successfully before fd-table setup, API socket publication, signal setup, or HVF startup. This is bangbang's native data-format version, not Firecracker's state-file version. |
| `--describe-snapshot <PATH>` | implemented native-envelope inspection | Opens a regular file with a 16 MiB payload / 16 MiB + 40 byte total cap, validates the complete bangbang-native header, exact length, CRC-64/Jones trailer, exact `1.0.0` version, arm64 identity, 4096-byte guest granule, and zero flags, then prints `v1.0.0`. In contained mode an exact `bangbang-grant:<GrantId>` claims one `SnapshotDescribeInput`/`ReadOnly` descriptor and inspects it without reopening the tag; direct mode keeps ordinary pathname opening. Missing, non-regular, oversized, malformed, corrupt, future-version, incompatible, missing-grant, or wrong-role files fail before startup with the bad-configuration exit status and path/payload/reference-redacted diagnostics. Firecracker state files are intentionally incompatible. |
| `--no-api` | config-file startup without API socket | Requires `--config-file`. Starts the supported config-file subset without binding or publishing the configured API socket, then waits for handled `SIGINT`, handled `SIGTERM`, or guest PSCI `SYSTEM_OFF` or `SYSTEM_RESET`. Runtime control and remaining runtime error exit-code parity remain deferred; `SYSTEM_RESET` is a terminal process outcome. |
| seccomp process flags | rejected | Firecracker's runtime seccomp installation is Linux-specific and has no public macOS equivalent. |

Normal startup also performs best-effort fd-table preallocation from
`RLIMIT_NOFILE` before opening configured resources. bangbang uses
non-clobbering descriptor duplication for this Firecracker-style startup guard,
so inherited high-numbered descriptors are not overwritten. Early commands such
as help, version, and snapshot inspection skip this setup.

Startup timing arguments are intentionally not exposed in `GET /vm/config` or
logs because they are process observability data, not guest configuration. When
metrics are configured, session-initial, explicit `FlushMetrics`, Running or
Paused periodic, and normal-terminal lines write the sampled store values under
`api_server`; omitted timing arguments remain omitted. Parsed `GET /`, `GET /version`,
`GET /machine-config`, `GET /mmds`, `GET /balloon`,
`GET /balloon/statistics`, `GET /balloon/hinting/status`, and
`GET /hotplug/memory` API requests are counted under `get_api_requests`; parsed
core configuration PUTs, `PUT /mmds`, `PUT /mmds/config`, `PUT /metrics`,
`PUT /logger`, `PUT /serial`, `PUT /balloon`, `PUT /hotplug/memory`,
`PUT /pmem/{pmem_id}`, and `/actions` API requests are counted under
`put_api_requests`; parsed `PATCH /machine-config`, `PATCH /mmds`,
`PATCH /drives/{drive_id}`, `PATCH /network-interfaces/{iface_id}`,
`PATCH /balloon`, `PATCH /balloon/statistics`,
`PATCH /balloon/hinting/start`, `PATCH /balloon/hinting/stop`,
`PATCH /hotplug/memory`, and `PATCH /pmem/{pmem_id}` requests routed through
VMM control are counted under `patch_api_requests`.
Parsed deprecated HTTP API usage is counted under
`deprecated_api.deprecated_http_api_calls` for explicit non-null machine
`cpu_template`, MMDS V1 config, deprecated `vsock_id`, and snapshot-load
`mem_file_path` or `enable_diff_snapshots: true` request forms. Parser failures,
including empty body-required mutating requests, malformed bodies, and path/body
ID mismatches, for the PUT and PATCH endpoints above with matching
Firecracker-shaped request metric fields are counted in the same count/fail
counters when the endpoint is identifiable from the request line.
Direct config-file and startup initialization paths are not API requests and
are not included in these counters. `PATCH /vm` remains outside
`patch_api_requests` because Firecracker does not expose a matching
`PatchRequestsMetrics` field for VM state changes. The balloon API request
fields are bangbang-specific extension counters: GET, PUT, and PATCH balloon
routes report `balloon_count`, and PUT/PATCH failures also report
`balloon_fails`. Firecracker exposes balloon device metrics but no matching
balloon API request metric fields. bangbang emits minimal top-level device
metrics objects for implemented behavior: aggregate `block` and non-empty
per-drive `block_{drive_id}` entries report virtio-block queue events,
read/write/flush counts and bytes, runtime backing update success/failure
counters, read/write latency aggregates, observable request/event failures, and
implemented block limiter throttling; runtime block dispatch also exposes a
backend-neutral retry delay when a limiter leaves a descriptor pending;
aggregate `net` and per-interface `net_{iface_id}` entries report implemented
virtio-net packet/byte/failure activity; aggregate `vsock` reports implemented
virtio-vsock packet, byte, connection-cleanup, and queue/event failure
activity; aggregate `entropy` reports implemented virtio-rng request, byte,
host-randomness failure, and event-failure activity, and runtime entropy queue
dispatch exposes a backend-neutral retry delay when a limiter leaves a
descriptor pending; `balloon` reports implemented virtio-balloon queue activity
and failures, including separate inflate/hint/report discard attempts,
zero-safe-reclaimed byte, skipped-edge byte, requested reporting byte, and
failed-attempt fields;
`signals.sigpipe` reports handled non-terminating `SIGPIPE`
signals. HVF block, PMEM, network, and entropy limiter retry wakeups are wired
for active queues and share acknowledged native-v1 quiescence. Direct and
contained vhost-user block use the same per-drive block metric identity: guest kicks and
interrupts follow the normal counters, while a closed or broken backend call
path increments `event_fails` without exposing its socket. Producer classes
that do not exist in the supported device subset remain absent rather than
appearing as synthetic zero-filled fields. The startup timing stores match Firecracker's
`ProcessTimeReporter` field names for the implemented process path.

Supported value-taking startup arguments accept both Firecracker-style
`--arg value` and `--arg=value` forms. Value-less flags, such as `--no-api`,
`--show-level`, `--show-log-origin`, and `--snapshot-version`, reject attached
values.

Like Firecracker's shared parser, the first standalone `--` ends option
parsing. The Firecracker main process does not consume retained extra
`String` arguments, so bangbang ignores following help/version spellings,
unknown options, and positional values. Bangbang additionally splits
`OsString` input before UTF-8 conversion, safely ignoring non-UTF-8 bytes after
the separator; Firecracker collects `env::args()` before parsing, so this last
behavior is a bangbang robustness extension rather than an upstream claim.

`--config-file` currently accepts the supported Firecracker-shaped sections
`machine-config`, `boot-source`, `drives`, `network-interfaces`,
`mmds-config`, `vsock`, `entropy`, `balloon`, `pmem`, `metrics`, `logger`, `serial`, and
`cpu-config`. The `cpu-config` section is parsed through the same request model
as `PUT /cpu-config`: empty/no-op custom template bodies clear CPU-template
selection, and custom templates containing the reviewed ID/core/SIMD/FP
profile become the effective startup selection. KVM capability, KVM vCPU-init
feature, mixed-category, noncanonical-width, boot-reserved, unavailable-banked,
and outside-profile requests fail with the same value-redacted platform faults
as the socket path.
No raw modifier value reaches startup diagnostics. The `drives`
section is parsed through
`PUT /drives/{drive_id}`: empty or all-null limiter objects are treated as
unconfigured, and configured bandwidth/ops limiters are stored for startup
block queue dispatch. The `entropy` section is parsed through `PUT /entropy`:
empty bodies, `rate_limiter: null`, empty `rate_limiter: {}` objects, all-null
`bandwidth`/`ops` bucket objects, and configured rate limiters are accepted.
The `balloon` section is parsed through `PUT /balloon` and stores the
pre-boot control-plane configuration. Startup can attach the current
guest-visible virtio-balloon MMIO/FDT shell from that stored configuration,
including the reporting feature and compacted reporting queue when
`free_page_reporting: true`. The runtime handler and HVF boot loop can dispatch
inflate, deflate, and reporting queue
notifications, publish zero-length used-ring completions, and signal the
allocated balloon interrupt line when queue completion requires it. Completed
inflate and deflate descriptors validate PFN ranges against mapped guest memory
and prepare the next compact paired accounting value before used-ring
publication; successful publication commits that value by infallible move.
Runtime `PATCH /balloon` can update the stored target size and active
virtio-balloon `num_pages` config-space value after startup while preserving the
other stored balloon fields. `GET /balloon/statistics` can return required
target and actual fields from the current target and internal inflated-page
accounting plus guest-reported optional fields from bounded statistics queue
reports. Free-page hinting command descriptors and active-run range
descriptors are validated and recorded by runtime queue dispatch. The
backend-neutral balloon handler can also complete a pending statistics
descriptor and mark queue-interrupt intent when runtime policy triggers a
statistics update. The process runtime schedules those statistics updates from
the configured polling interval while the VM is running. Device-writable
free-page reporting descriptors are validated and sent through the same
best-effort, inward-aligned discard owner before completion; requested, advised,
skipped, and failed work remain distinct. Detached balloon state captures
validated queue cursors, pending statistics, hint commands, and compact PFN
ranges through the paused MMIO/PCI owner without creating a serialized format.
Synchronous footprint guarantees and balloon serialization/restore remain
deferred. The `pmem` section is
parsed through `PUT /pmem/{id}`; valid entries store Firecracker-shaped
pre-boot configuration and appear in `GET /vm/config`. Exactly one ordinary
block or pmem device may be root. Pmem order supplies its stable Linux index,
so startup appends `root=/dev/pmem<i>` plus `ro` or `rw`; same-ID replacement
keeps that order, and conflicting replacement fails before configuration or
grant mutation. In contained mode, a successful `PUT` may claim one exact
repeatable pmem grant
whose access matches `read_only`; the validated opened backing is retained by
pmem ID and moved into startup without reopening its tag. Ordinary paths retain
the historical deferred-open behavior. Startup validates each backing as a
non-zero regular host file, mmaps it to a 2 MiB-aligned host range, and retains
the handles and mappings. Startup also assigns deterministic non-overlapping
2 MiB-aligned guest physical ranges after the aarch64 MMIO64 gap, skipping
current guest RAM, and populates the internal virtio-pmem config-space
`start`/`size` values from those ranges. HVF startup creates the VM with the
framework-reported maximum IPA size so the post-MMIO64 pmem ranges are
addressable and registers a cloned lease on each exact prepared mapping after
DRAM. The host pointer needs only host-page alignment; the guest address and
mapped length remain 2 MiB aligned. Writable backings are shared, and
read-only backings use a private, write-capable host mapping because HVF
rejects a read-only host address; guest permissions still remain read-only and
non-executable, so accidental host writes are copy-on-write and a guest write
fault cannot mutate the backing. Queue flush and graceful teardown synchronize
exactly `file_len` bytes with `MS_SYNC`; the private aligned tail is volatile.
Failed unmap retains every lease that HVF may still reference.
Runtime also has a backend-neutral virtio-pmem identity, queue metadata,
feature-bit, 16-byte config-space foundation, and flush request completion
handling.
Startup attaches each prepared pmem device through the selected transport: a
guest-visible virtio-mmio/FDT node by default or a modern PCI function with
`--enable-pci`. Both expose the same prepared `start` and `size` config values.
Configured bandwidth and operation buckets are reported through
`GET /vm/config` and charged once per non-empty coalesced flush event before
descriptor consumption. Flush selection is lazy after the first valid request
and scoped to the notified device, so empty or malformed-only events do not
synchronize a backing and peer pmem devices are not traversed. One event result
is cached only for later valid descriptors on that device. Throttled work
retains its queue cursor and is retried by a dedicated session-owned HVF wakeup.
Runtime `PATCH /pmem/{id}` accepts missing, `null`, empty, and all-null rate
limiter objects as no-op updates and atomically replaces or clears present
buckets on the exact active device before committing stored configuration.
Runtime PUT/DELETE still reject root-device mutation.
The `memory-hotplug` section is parsed and stored like `PUT /hotplug/memory`.
When present, `InstanceStart` attaches the current virtio-mem MMIO/FDT shell,
with zero plugged, requested, and usable bytes. Runtime
`PATCH /hotplug/memory` can update the requested size and grow the active
virtio-mem usable config-space aperture to a slot boundary while signaling a
config interrupt. The runtime virtio-mem handler can track plugged blocks,
answer `STATE` requests as plugged, unplugged, or mixed, accept valid
`PLUG`/`UNPLUG`/`UNPLUG_ALL` requests, and update virtio-mem config-space
`plugged_size`. After startup, `GET /hotplug/memory` reports that active
runtime plugged size when the handler can be queried. Accepted guest requests
operate over complete validated block ranges and use exact block-owned HVF
dynamic mappings that may be split or combined for unplug. Backend mutation
precedes ACK publication, device state commits after guest-visible completion,
and partial or late failures roll applied ranges back in reverse order. Signed
executable coverage proves Linux and the public status surface complete a
requested/plugged `0 -> 128 MiB -> 0` lifecycle. Runtime device deletion,
broader public guest-memory accounting, and optional-device snapshot state
remain deferred; bangbang does not claim Firecracker's KVM slot mechanism.
The config-file path does not load MMDS data; use Firecracker's separate
`--metadata <PATH>` startup argument for startup MMDS data.

CLI values are untrusted input. Current validation rejects invalid IDs, empty
socket paths, and socket paths containing control characters. API startup also
fails if the configured socket path already exists. Socket cleanup removes the
socket inode created by the current process during normal shutdown and handled
`SIGINT`/`SIGTERM` shutdown; fatal signal exits use `_exit`, and uncatchable
forced termination such as `SIGKILL` can still leave a stale socket path behind.
The API socket is unauthenticated;
bangbang restricts the published socket inode to owner-only permissions, and
the parent directory remains part of the access-control boundary. Operators
should use a private socket directory on multi-user hosts. Process CLI parsing
stays outside the future VM/vCPU fast path and should add only trivial startup
overhead. Error and status output avoid echoing path-like CLI values.

### Process Exit Status

The current executable uses a small process exit status contract:

| Exit status | Current meaning | Compatibility notes |
| --- | --- | --- |
| `0` | Help or version completed successfully, the API server exited without error, no-api mode handled `SIGINT`/`SIGTERM` shutdown, or a process-owned VM exited after guest PSCI `SYSTEM_OFF` or `SYSTEM_RESET`. | Matches Firecracker's success status. |
| `148` | The process intercepted `SIGSYS`. | Matches Firecracker's `BadSyscall` exit code for an explicitly delivered signal. Linux seccomp bad-syscall enforcement remains platform-limited on macOS. |
| `149` | The process intercepted `SIGBUS`. | Matches Firecracker's fatal signal exit code. |
| `150` | The process intercepted `SIGSEGV`. | Matches Firecracker's fatal signal exit code. |
| `151` | The process intercepted `SIGXFSZ`. | Matches Firecracker's fatal signal exit code. |
| `152` | Startup configuration failed before the process entered runtime, including config-file, metadata, logger-sink, and metrics-sink configuration failures. | Matches Firecracker's `BadConfiguration` exit code for clearly startup configuration failures. |
| `153` | Startup argument parsing failed before process configuration began. | Matches Firecracker's `ArgParsing` exit code. |
| `154` | The process intercepted `SIGXCPU`. | Matches Firecracker's fatal signal exit code. |
| `156` | The process intercepted `SIGHUP`. | Matches Firecracker's fatal signal exit code. |
| `157` | The process intercepted `SIGILL`. | Matches Firecracker's fatal signal exit code. |
| `1` | Process failure, including API socket bind, signal handler registration, no-api signal wait failure, API accept failure, startup time accounting failure, periodic runtime work failure, or a process-owned boot worker non-success terminal state. | Used for non-configuration process failures before more specific Firecracker-compatible process errors exist. Per-connection read/write errors do not terminate the API server. |

Fatal signal exits call `_exit`, so normal Rust destructors and API socket
cleanup do not run on those paths. `SIGPIPE` remains non-terminating in
Firecracker and is not exposed as a process-exit status by bangbang; runtime
metrics can report handled occurrences as `signals.sigpipe`. `SIGINT` and
`SIGTERM` remain graceful successful shutdown signals.

## Compatibility Baseline

bangbang's first Firecracker compatibility baseline is the upstream
`firecracker-microvm/firecracker` `v1.16.0` release tag:

- tag: `v1.16.0`
- commit: `d83d72b710361a10294480131377b1b00b163af8`

A release tag is the compatibility reference because it represents a published
Firecracker interface. Development branch commits can still inform
implementation research, but they must not redefine bangbang's compatibility
target without an explicit baseline update. A standalone pinned commit is
precise, but it should be tied to a release tag for this project so the baseline
is both reproducible and recognizable.

Use these upstream files and documents as sources of truth when comparing
Firecracker behavior:

- `src/firecracker/swagger/firecracker.yaml` for the published HTTP API surface
- `src/firecracker/src/api_server/parsed_request.rs` for method and path routing
- `src/vmm/src/rpc_interface.rs` for VMM actions and state-dependent behavior
- `src/vmm/src/vmm_config/net.rs` for network MTU and rate-limiter construction
- `docs/device-api.md` for endpoint, device, input, and output dependencies
- `docs/device-hotplug.md` for the Developer Preview PCI attach/remove boundary
- `docs/mmds/mmds-design.md` for guest packet handling and security constraints
- `docs/design.md` for process model, thread model, and threat-containment
  expectations

Unreviewed upstream drift in API routing, VMM actions, device behavior, or
published docs must not implicitly change bangbang's target. Future baseline
updates must be explicit pull requests that update this documentation and
describe API, state, documentation, security, performance, and test impact
before changing this reference.

## Support Level Vocabulary

The current scaffold implements `GET /`, `GET /version`, `GET /vm/config`,
`GET /machine-config`, pre-boot `PUT /machine-config` configuration storage, pre-boot
`PUT /boot-source`, `PUT /drives/{drive_id}`,
`PUT /network-interfaces/{iface_id}`, `PUT /vsock`, `PUT /entropy`, `PUT /metrics`, and `PUT /logger` configuration
storage over HTTP on a Unix domain socket, plus runtime `FlushMetrics` and
periodic runtime metrics flushes after successful startup. The support levels below describe compatibility targets for
future API work:

- supported target: planned for the first boot-oriented API implementation
- planned later: expected to be compatible later, but outside the first tier
- deferred: blocked on a separate capability, device, or backend design
- intentionally unsupported: not part of the current macOS/HVF target without a
  later compatibility policy change

For request fields, rejected means the future API should fail the request once
JSON models exist. Optional means the field may be omitted; for Firecracker
fields represented as nullable optional values, explicit `null` should be
treated like omission unless a row says otherwise. Ignored means accepted with
no effect. No supported target field is intentionally ignored. Deferred request
fields should be rejected until their capability is implemented. Some fields
have value-specific policy so Firecracker's explicit default values remain
accepted while feature-enabling values stay out of the first tier. Unknown JSON
fields should be rejected to match Firecracker `v1.16.0` request models that
deny unknown fields.

## Endpoint Compatibility Matrix

The first planned compatibility tier is the smallest boot-oriented API surface.
Rows marked as implemented describe current behavior; the rest describe planned
compatibility targets.

| Method | Endpoint | Support level | Scope notes |
| --- | --- | --- | --- |
| `GET` | `/` | supported target; implemented | Describe the microVM instance. The state becomes `Running` after successful startup with the internal boot run-loop worker across bounded step windows retained. |
| `GET` | `/version` | supported target; implemented | Report the VMM version with a Firecracker-shaped body. |
| `GET` | `/vm/config` | supported target; implemented | Returns the accumulated supported VM configuration subset. Unsupported sections are omitted until their models exist. |
| `GET` | `/machine-config` | supported target; implemented | Returns the stored/default machine configuration. |
| `PUT` | `/machine-config` | supported target; implemented | Stores the first vCPU and memory configuration subset before boot; values are applied during startup preparation. |
| `PATCH` | `/machine-config` | supported target; implemented | Applies pre-boot partial updates to the stored machine configuration, preserving omitted fields and rejecting invalid updates without mutation. |
| `PUT` | `/boot-source` | supported target; implemented | Stores guest kernel path, optional initrd path, and optional boot arguments before boot. Direct paths open during startup preparation; contained grant tags claim exact read-only descriptors during the successful request and move them into startup without reopening the tags. |
| `PUT` | `/drives/{drive_id}` | supported target; pre-boot plus PCI-only file and eligible direct/contained-vhost runtime attach implemented | Before boot, stores initial virtio-block configuration including optional file-backed bandwidth/ops rate limiters or the strict socket matrix. Direct vhost uses an operator path; contained vhost requires an exact connect-only directory grant plus child and receives only a brokered stream. After startup with public `--enable-pci`, Running or Paused requests may transactionally attach one new non-root file or vhost endpoint when the immutable live guest-memory profile is already shared. File paths open before owner submission; contained file requests may consume only an exact still-unused initial-manifest grant. Vhost requests run side-effect-free owner profile/capacity preflight before direct discovery or contained child reservation/broker I/O, materialize against exact live shared regions on the owner, and publish `/vm/config` only after every endpoint lease succeeds. Default MMIO, root, duplicate, anonymous RAM, invalid backing/grant/negotiation, unavailable session, and exhausted capacity are nonmutating; duplicate and owner-preflight failures do not contact a candidate socket or broker. Same-ID runtime PUT remains duplicate rejection. |
| `PUT` | `/metrics` | implemented supported sparse subset | Opens one process-local file/FIFO sink before boot with nonblocking output and path-redacted errors; duplicate initialization fails without replacing it, and observability state is omitted from `GET /vm/config`. Configuration alone writes nothing. A retained session causes one best-effort initial line; 60-second output continues in Running and Paused; explicit runtime `FlushMetrics` is fallible; and normal process convergence makes one best-effort final attempt. Lines use the interval/store, successful-baseline, reset-aware, sparse-schema, and at-least-once retry contract above for all implemented API, logger, signal, UART, and device producers. |
| `PUT` | `/actions` | supported target; internal startup execution and explicit metrics flush implemented | Parses `InstanceStart` and `FlushMetrics` and routes them through the process VMM owner. Parsed request and successful action logger records are best effort and never gate the functional result. `InstanceStart` validates boot source and state, prepares an owned HVF session with configured or bounded internal serial TX, starts the worker, and commits `Running` after the worker handle is retained. `FlushMetrics` is rejected before startup; after startup it returns `204` for an unconfigured/successful sink or a metrics fault for a failed configured write, and it retains its API/action/logger effects. Automatic initial, periodic, and terminal writes do not route through `/actions` and create no action log. The aarch64 `SendCtrlAltDel` parser path contributes to `put_api_requests.actions_count` but not `actions_fails`, matching Firecracker's parser-entry placement. |
| `PUT` | `/actions` with `SendCtrlAltDel` | intentionally unsupported; parser rejected | Firecracker gates this action on x86 keyboard behavior; the first bangbang target is Apple Silicon. The unsupported request is counted under `put_api_requests.actions_count` without incrementing `actions_fails`. |
| `PUT` | `/logger` | implemented supported process-local subset | Stores pre-boot configuration, opens an optional nonblocking sink, applies level/show/module filters, and omits observability state from `GET /vm/config`. Parsed API method/path and successful `InstanceStart`/explicit `FlushMetrics` actions are unrestricted host records with no bodies. Boot-timer records use the bounded callsite and recovery contract above. Sink contention/poison/write/flush failure increments `missed_log_count` and never changes the request, action, or guest result. No sink is configured by default. |
| `PUT` | `/serial` | implemented TX output and limiter subset | Stores an optional pre-boot public path and byte token bucket; `{}` or `"serial_out_path": null` clears the public path. Startup opens a configured file/FIFO nonblocking, otherwise it uses a bounded 64-KiB internal buffer rather than stdout. Exhausted bytes are dropped without blocking, sleeping, or failing the guest write, and implemented UART deltas report writes, errors, missed writes, and dropped bytes. Public RX/stdin/streaming and read/flush producers are absent. |
| `PUT` | `/cpu-config` | supported finite arm64 custom profile; all other categories terminally classified | Parses bounded ordered Firecracker aarch64 custom templates with a 256-entry limit per array, exact 32/64/128-bit ARM identities/bitmaps, fixed seven-word vCPU-feature indexes, stronger duplicate-identity checks, and value-redacted diagnostics. A successful custom PUT replaces static/custom state; empty input clears it. Exact `(baseline & !filter) | value` execution covers eleven U64 ID registers, ACTLR.EnTSO, U64 X0/X4-X30 and reviewed SP/PC/PSTATE fields, U128 Q0-Q31 with explicit little-endian transport, and U32 FPCR/FPSR with fail-closed scalar conversion. ZFR0/SMFR0 require a public macOS 15.2 pre-VM gate; ACTLR filters are limited to bit 1. Every requested typed baseline is read on every owner before any write, targets are common, and each write is immediately reread; boot setup then overrides X0/PC/PSTATE. Any failure destroys the unpublished VM. X1-X3, banked state, all named unsafe/dependency/time/ownership/EL2 public-HVF families, aliases, unnamed encodings, and invalid KVM fields receive stable value-free policy faults; KVM capability, vCPU-feature, demux, firmware, firmware-feature, SVE, and unknown classes have distinct platform faults. Custom contents are omitted from GET and excluded from native-v1 snapshots. See the checked CPU-template contract. |
| `PUT` | `/network-interfaces/{iface_id}` | pre-boot storage plus PCI-only Running/Paused insertion implemented | Stores up to 16 initial virtio-net configurations before boot without opening host networking resources, including Firecracker-shaped RX/TX bandwidth and ops limiters. Startup preparation attaches configured interfaces over the selected virtio-MMIO/FDT or modern PCI transport. In a live PCI session, a new validated ID/MAC prepares one independent packet-I/O entry using the immutable startup/MMDS policy, checks actual contained vmnet authority, publishes metrics and PCI ownership on the owner thread, and commits live configuration last. Existing entries keep their queues and resources. Duplicate ID/MAC, invalid host config, capacity, authority, command, and publication failures preserve prior state; uncertain cleanup is terminal. Default MMIO rejects runtime insertion. Internal notification dispatch and runtime PATCH retain the same limiter/retry behavior. External direct-vmnet connectivity, limiter-specific metrics, and snapshots remain deferred. |
| `PUT` | `/vsock` | supported target; implemented supported live MMIO-or-PCI startup/Unix-socket subset | Repeated valid pre-boot requests atomically replace stored configuration; post-start PUT is stably rejected without mutation. Direct mode defers opening the ordinary path until startup. Contained mode recognizes only exact `bangbang-grant:<GrantId>/<SocketChild>` references, claims one exact `VsockSocketDirectory` after complete request validation, and retains its scope/anchor without reopening the tag; rejected replacement preserves prior public and private state. Startup either binds and inode-tracks the direct path or exclusively publishes a supplied owner-only listener through the exact anchor, then attaches one guest-visible endpoint over the selected startup transport with three 256-entry queues and cleans up only its own socket. Host initiation uses that main listener. Guest initiation in contained mode uses a session-bound launcher facet fixed once to the anchor/child and carrying only monotonic `u32` ports plus connected stream descriptors; the launcher receives no guest payload and the worker gains no outgoing-network entitlement. The live handler supports bounded handshakes and four-packet directional backlogs, 256 connections per direction, dynamic 64-KiB credit windows with wrapping counters, partial/full shutdown, two-second request/shutdown cleanup, reset/error handling, `EVENT_IDX`, no-op event notifications, and path/payload-redacted diagnostics. Signed Apple Silicon cases verify both initiation directions, ≥1 MiB direct transfers and a 1-MiB granted host-initiated transfer, both peers' write-half-close/EOF, terminal cleanup, two-stream isolation, an outside-container granted API listener, and no steady-state helper or entitlement change. Indirect descriptors are a supported bangbang extension. PATCH, DELETE, runtime hotplug, broader CID routing, full event payloads, runtime PCI hotplug, vhost/KVM, and general performance/artifact parity remain excluded. Native-v1 snapshot UDS override, event-queue `TRANSPORT_RESET`, and post-restore RX gating remain the stable #543 exclusions. |
| `GET`, `PUT`, `PATCH` | `/mmds` | supported target; control-plane storage, runtime guest-query formatting, internal guest GET response modeling, request parsing, process-local exchange handling, response-byte serialization, process-local token authority, process-local guest token `PUT` modeling, process-local MMDS v2 GET token enforcement, internal guest ARP/TCP packet classification, process-local packet-payload HTTP exchange, process vmnet TX detouring, bounded per-interface contiguous split-request buffering, internal ARP/TCP response-frame synthesis, and signed executable guest fetch paths implemented | Stores bounded in-memory JSON object contents in the process runtime, returns stored JSON for control-plane `GET` or JSON `null` before initialization, applies RFC 7396 merge-patch semantics for `PATCH`, rejects uninitialized `PATCH`, and keeps previous data on oversized update failure. The runtime can also resolve initialized metadata by JSON-pointer path, format JSON or Firecracker-shaped IMDS text, parse process-local guest HTTP `GET` request bytes into URI/output-format/token inputs, map internal guest GET requests to process-local status/content-type/body response values, turn complete process-local guest HTTP request buffers into deterministic HTTP response bytes that preserve accepted `HTTP/1.0` or `HTTP/1.1` status-line versions, generate/validate bounded process-local opaque MMDS tokens, and model process-local guest `PUT /latest/api/token` exchanges as prerequisites for guest-visible delivery. When configured for MMDS v2, process-local guest GET requests require exactly one valid `X-metadata-token` or `X-aws-ec2-metadata-token` value generated by token PUT; missing, duplicate, unknown, or expired tokens return `401 Unauthorized`. The runtime can classify ARP requests for the configured MMDS IPv4 address and raw Ethernet/IPv4/TCP guest packet bytes addressed to that IPv4 address and TCP port 80 while rejecting malformed, truncated, fragmented, non-TCP, or non-MMDS packets, and it can identify pure empty-payload TCP SYN, ACK-only packets that acknowledge bangbang's deterministic SYN-ACK, FIN close, packets carrying guest RST, and unsupported control packets, synthesize SYN-ACK frames for SYN packets, synthesize ACK plus FIN-ACK frames for empty FIN close packets, synthesize minimal RST frames for unsupported empty controls, consume guest RST packets without response even when they also carry payload bytes, and turn non-empty candidate TCP payloads that acknowledge bangbang's deterministic SYN-ACK and do not carry unsupported SYN or FIN payload control flags into the same process-local HTTP response bytes as the guest HTTP helper. Process vmnet packet I/O now detours MMDS ARP requests, pure empty-payload MMDS SYN packets, pure empty-payload MMDS ACK-only packets that acknowledge bangbang's deterministic SYN-ACK, pure empty-payload MMDS FIN close packets, guest packets carrying RST, unsupported empty control packets, and non-empty MMDS candidate TX payloads on MMDS-configured interfaces when they acknowledge bangbang's deterministic SYN-ACK and do not carry unsupported SYN or FIN payload control flags. Shared process-local MMDS data remains visible to control-plane and packet paths, while every configured interface detour owns a separate split-request buffer collection and response queue. Each detour buffers split request headers only when every fragment starts at the next expected TCP sequence number, rejects non-contiguous buffered fragments without forwarding them to vmnet, synthesizes deterministic Ethernet/ARP replies, Ethernet/IPv4/TCP SYN-ACK frames, minimal Ethernet/IPv4/TCP FIN close frames, minimal Ethernet/IPv4/TCP RST frames, and Ethernet/IPv4/TCP response frames carrying generated HTTP response bytes, retains those frames in its bounded queue, exposes queued frames through the matching virtio-net RX source before vmnet reads, prioritizes ARP replies before queued TCP responses, and schedules one bounded post-TX RX retry when that source reports a queued response. The signed executable HVF e2e target includes direct-rootfs scenarios that configure `vmnet:shared`, deterministic MMDS data, and MMDS v1 or MMDS v2 before startup, then have the guest fetch `/meta-data/bangbang-marker` through `169.254.169.254` and write host-observable success markers. A two-interface MMDS-only scenario finds both guest devices by configured MAC, binds one request to each, writes distinct fixed marker sectors, and reports both interface metric objects without opening vmnet resources. The v2 scenario first requests `/latest/api/token` with a bounded TTL and uses the returned token header for the metadata fetch. Full ARP cache management, gratuitous ARP, ARP timeout/retry policy, broader ACK-number validation beyond the narrow ACK-only and non-empty payload SYN-ACK acknowledgement paths, full TCP stream tracking, out-of-order reassembly, retransmission policy, stateful RST policy, session timeout policy, and broader per-interface TCP session state beyond the current split-request buffers remain deferred to future guest-visible MMDS networking work. |
| `PUT` | `/mmds/config` | supported target; control-plane config storage implemented | Parses Firecracker-shaped MMDS config with required `network_interfaces`, optional `version`, optional RFC 3927 usable link-local `ipv4_address`, and optional `imds_compat`; keeps empty or whitespace-only interface IDs as malformed request bodies, but routes empty interface lists to runtime semantic validation before mutation; validates referenced interface IDs against configured network interfaces; stores config before startup; and keeps post-start requests on the normal unsupported-state policy. Broader guest-visible MMDS behavior remains deferred to future MMDS networking work. |
| `PUT` | `/snapshot/create`, `/snapshot/load` | supported narrow native-v1 Full/File subset | Parses Firecracker-shaped bodies, rejects malformed input first, normalizes deprecated load `mem_file_path` to a `File` backend and dirty tracking to the OR of old/new flags, and keeps paths/overrides redacted through typed API/runtime values. Create is paused-only and admits `Full` for one vCPU, exactly one regular read-only root drive, default serial, and no optional devices or MMDS. Direct mode invokes the path adapter; contained mode validates `bangbang-grant:<GrantId>/<SnapshotOutputChild>` references, retains matching repeatable output-directory anchors, and publishes staging/finals relative to them. Both stream aggregate capture into owner-only staging, exclusively commit memory first and state last, return `204`, and leave the source paused. A tracked source re-protects and advances only after either durable or uncertain-visible Full commit; every pre-visible failure keeps its old epoch, while incomplete rollback prevents resume and tears down safely without misreporting artifact visibility. Load is pre-boot-only and requires successful-action history plus current non-logger/metrics configuration to be pristine. Direct mode opens the committed kind-2 pair by path. Contained mode duplicates state for bounded preinspection, discovers any grant-tagged persisted root, atomically takes every tagged `SnapshotStateInput`, `SnapshotMemoryInput`, and read-only `DriveBacking`, then completes from prepared state and supplied files without tag reopen. Both validate before fresh VM construction, optionally attach a clean destination epoch after image population, commit a real session as `Paused`, and use ordinary resume when `resume_vm` is true. Retryable preparation failures preserve pristine eligibility; uncertain cleanup is terminal. State-invalid requests and unsupported dimensions fail before their established mutation boundaries. Admitted successes, capability rejections, and execution failures record snapshot latency; parser and invalid-state failures do not, and deprecated usage is counted independently. Typed execution faults, logs, metrics, staging records, and response bodies expose no artifact path, grant ID, child, filesystem identity, or guest/HVF value. No Firecracker state-file interoperability, `Diff`, UFFD, realtime adjustment, overrides, optional-device profile, or cross-host portability is claimed. |
| `GET` | `/balloon` | supported target; pre-boot and runtime config read implemented | Returns the stored Firecracker-shaped balloon configuration after successful `PUT /balloon`, runtime `PATCH /balloon`, or valid runtime `PATCH /balloon/statistics`; returns the balloon-specific unsupported fault when no balloon configuration exists. Runtime derives backend-neutral virtio-balloon identity, features, queues, and config space from stored config. Startup attaches the current endpoint over the selected startup transport, and the HVF boot loop can dispatch inflate, deflate, statistics, free-page hinting, and free-page reporting notifications with interrupt signaling. Inflate/deflate descriptors update internal inflated-page accounting, hinting command descriptors update `guest_cmd`, and completed inflate plus accepted current-command hint and reporting ranges use best-effort inward-aligned Darwin zero/free advice before dispatch returns. Statistics queue reports are parsed and stored for `GET /balloon/statistics`, process-level periodic scheduling can complete a pending statistics descriptor with queue-interrupt intent while the VM is running, and device metrics distinguish discard attempts, reporting-requested bytes, actual advised bytes, skipped edges, and failures. |
| `PUT`, `PATCH` | `/balloon` | implemented and verified; pre-boot configuration, live target/statistics updates, queue dispatch, accounting, and Darwin discard | `PUT /balloon` stores Firecracker-shaped balloon configuration before boot, rejects targets larger than current guest memory without mutating prior config, accepts and preserves `free_page_reporting: true`, appears in `GET /balloon` and `GET /vm/config`, and feeds startup attachment of a virtio-balloon endpoint over the selected transport with the reporting feature and compacted queue when enabled. Pre-boot machine-config updates also reject memory sizes smaller than an already configured balloon target. Runtime `PATCH /balloon` updates the stored `amount_mib`, active `num_pages` config space, config generation, and config interrupt. Runtime `PATCH /balloon/statistics` can update a nonzero statistics polling interval to another nonzero value while preserving Firecracker's rejection of runtime statistics enable/disable transitions. Runtime queue dispatch covers inflate, deflate, configured statistics reports, pending statistics descriptor completion from process-level periodic scheduling, active-run hinting ranges, and device-writable reporting ranges. Completed inflate plus accepted current-command hint and reporting ranges are zeroed and made clean/reclaimable on inward-aligned Darwin host-page interiors; advice is best effort, unsupported targets fail honestly, requested reporting bytes remain distinct from actual advised bytes, and no synchronous footprint reduction is promised. |
| `GET` | `/balloon/statistics` | implemented and verified; required target/actual and optional guest-reported fields | Routes through the VMM state/action policy. Statistics queries are post-boot-only, require configured active balloon state, and return Firecracker-shaped required fields: `target_pages`, `actual_pages`, `target_mib`, and `actual_mib`. Target values come from the current stored balloon target, including runtime `PATCH /balloon` updates. Actual values come from active inflated-page accounting. Optional guest-reported fields are included only after a bounded statistics queue report records them; process-level periodic scheduling can complete a pending descriptor and request the next report while the VM is running. Linux's pre-`DRIVER_OK` statistics kick is admitted after `FEATURES_OK` and retained until balloon activation. |
| `PATCH` | `/balloon/statistics` | implemented and verified; live nonzero interval update | Parses Firecracker-shaped statistics interval update request bodies and rejects malformed or invalid bodies first. Valid requests are post-boot-only, require a configured balloon, accept unchanged intervals as no-ops, and update stored plus active balloon state for nonzero-to-nonzero interval changes. Runtime `0 -> nonzero` and `nonzero -> 0` transitions are rejected without mutation, matching Firecracker's statistics state-change rule because the stats queue cannot be hot-added or removed. The updated interval feeds process-level periodic scheduling for running VMs. |
| `PATCH` | `/balloon/hinting/start` | implemented and verified; host command state, guest acknowledgement, and Darwin active-range discard | Parses Firecracker-shaped free-page hinting start commands, including empty/default commands, rejects malformed or invalid bodies first, and then routes valid requests through the VMM state/action policy. Hinting start is post-boot-only, requires a configured balloon with `free_page_hinting: true`, preserves `acknowledge_on_stop` in backend-neutral device state, advances the host command id while skipping reserved values, updates active config space, raises a config interrupt, and returns `204 No Content`. Hinting queue command acknowledgements can update `guest_cmd`, accepted current-command ranges use best-effort Darwin discard, stale/inactive ranges remain ignored, and completed guest `STOP(0)`/`DONE(1)` commands can automatically acknowledge host `DONE(1)`. |
| `PATCH` | `/balloon/hinting/stop` | implemented and verified; host command state, guest acknowledgement, and Darwin active-range discard | Routes through the VMM state/action policy without parsing the request body, matching Firecracker's stop-command parser behavior. Hinting stop is post-boot-only, requires a configured balloon with `free_page_hinting: true`, writes the Firecracker done command into host-owned active device state and config space, raises a config interrupt, and returns `204 No Content`. Hinting queue command acknowledgements can update `guest_cmd`, accepted active-run ranges use best-effort Darwin discard before a stop takes effect, and completed guest `STOP(0)`/`DONE(1)` commands can automatically acknowledge host `DONE(1)`. |
| `GET` | `/balloon/hinting/status` | implemented and verified; host and guest command status | Routes through the VMM state/action policy. Hinting status is post-boot-only, requires a configured balloon with `free_page_hinting: true`, and returns Firecracker-shaped `host_cmd` and `guest_cmd` fields from active device state. Current status reports the latest start/stop host command and the latest 4-byte guest command observed on the hinting queue; `guest_cmd` remains `null` until the guest sends a command descriptor. Accepted active-run ranges are validated and discarded best effort on Darwin but are not exposed in this response. |
| `PUT`, `PATCH` | `/pmem/{id}` | implemented direct MMIO-or-PCI startup, pmem root, non-root PCI runtime, capture-ready state, and aggregate live certification; optional serialization/restore excluded | `PUT /pmem/{id}` stores strict Firecracker-shaped pre-boot configuration and exposes accepted state through `GET /vm/config`. One root is permitted across ordinary block and pmem; pmem order selects `/dev/pmem<i>` and `ro`/`rw`, while conflicts fail before grant or configuration mutation. Direct paths remain unopened until startup; contained grant tags claim and retain an exact-ID, exact-access regular-file descriptor during successful pre-boot PUT, then move it into startup without reopening the tag. `InstanceStart` validates each nonzero regular backing, assigns a deterministic aligned guest range, and registers a cloned lease on the exact file/private-tail mapping before attaching the selected transport. Running/Paused public PCI PUT remains non-root-only: owner-side root/duplicate/shared-endpoint/inventory/PCI/BAR/MSI-X/dispatcher/metrics capacity preflight precedes contained grant claim or direct open/map, and configuration plus grant consumption commit only after mapping and endpoint publication succeed. Flush selects only the notified device and synchronizes exactly the file prefix with `MS_SYNC`; empty or malformed-only events do not sync, peer devices are not traversed, and the aligned tail remains volatile. Optional bandwidth/ops buckets and post-boot PATCH retain their atomic limiting/retry behavior. Signed focused and aggregate coverage proves pre-flush backing coherence, read-only enforcement, MMIO read-only and PCI writable root boot, contained descriptor identity after pathname replacement, runtime attach/delete, and exact same-ID/PCI-slot/guest-range reuse alongside Sync, Async, vhost-user, and virtio-mem. Live traversal retains exact backing/mapping/device/transport state. Exactly the two checked Wave 6 pmem composites retain optional-device serialization/restore. |
| `PUT` | `/entropy` | supported target; configuration storage, entropy rate limiting, startup attachment, and signed executable guest read validation implemented | Stores one Firecracker-shaped virtio-rng entropy configuration before boot. Missing, `null`, empty, and all-null `rate_limiter` objects remain unconfigured; valid configured `bandwidth` and `ops` buckets are stored, echoed through `GET /vm/config`, and applied to the HVF virtio-rng queue path. Throttled descriptors remain pending and are retried on later dispatch opportunities without sleeping or busy-waiting; runtime dispatch reports the earliest backend-neutral retry delay for pending limiter-throttled descriptors, and active HVF entropy queues schedule a per-session retry wakeup from that delay. Oversized byte requests are allowed once a bandwidth bucket is full so a guest cannot be permanently throttled by a request larger than the bucket size. `InstanceStart` attaches the existing HVF virtio-rng endpoint over the selected startup transport backed by the session-owned host OS randomness source. The signed executable HVF e2e target boots a direct-rootfs guest, checks that Linux selected `virtio_rng` as the current hardware RNG, reads non-empty data from `/dev/hwrng`, and writes a host-observable success marker. Post-start requests follow the pre-boot-only unsupported-state policy. Full Firecracker timerfd/eventfd shared event-source parity remains deferred; aggregate `entropy` runtime metrics cover implemented request, byte, host-randomness failure, event-failure, throttling, and limiter-event activity. |
| `GET`, `PUT`, `PATCH` | `/hotplug/memory` | implemented supported MMIO-or-PCI startup subset; runtime device deletion and snapshots excluded | `PUT` validates and stores block/slot/total sizing before boot; `GET` returns stored pre-start status or exact active requested/plugged status; `PATCH` validates and signals requested-size changes after start. Startup attaches one virtio-mem endpoint over the selected startup transport. Its queue validates request/response descriptors and complete block ranges, answers `STATE`, and applies `PLUG`, `UNPLUG`, and `UNPLUG_ALL` over exact block-owned guest/HVF mappings before ACK. Device state commits only after guest-visible completion; split/combined mappings and partial or late failures use reverse rollback and fail closed. Focused coverage crosses the conceptual slot boundary and adjacent mappings without claiming KVM slot identity. Signed direct-rootfs coverage proves Linux binding and public requested/plugged size `0 -> 128 MiB -> 0`. Runtime device deletion, broader public accounting, and optional-device snapshots remain deferred. |
| `PATCH` | `/vm` | implemented and verified API semantics; native-v1 snapshot ownership implemented | Parses the Firecracker-shaped VM state request with required `state` values `Paused` and `Resumed`, then routes valid requests through `Pause` or `Resume` VMM actions. Requests before startup fail as unsupported in `Not started` state. After startup, `Paused` transitions a `Running` instance to `Paused` only after a topology-wide active-run wakeup barrier drains every online vCPU and the process-owned boot worker closes its next-run gate. `Resumed` transitions it back to `Running` only after the worker accepts resume. Same-state `Paused` and `Resumed` requests return `204`, still require the retained process session, leave state unchanged, skip the backend command and generation, and record the successful API-request latency. Signed single-process and dual-process evidence repeats both requests, observes stable state and independent CPU0/CPU1 progress tokens, proves both stop while one process is paused as an isolated peer continues, and proves both resume without fixed sleeps. The native-v1 baseline layers a complete four-scheduler capture/publication transaction over this topology barrier; generic optional-device and multi-vCPU artifact ownership plus complete HVF state remain deferred. |
| `PATCH` | `/drives/{drive_id}` | supported target; file backing/rate-limiter update and ID-only direct/contained-vhost config refresh implemented | Parses the Firecracker-shaped block-device update request with required `drive_id`, optional `path_on_host`, and optional `rate_limiter`, then routes valid updates through `UpdateBlockDevice`. Empty or all-null rate limiter objects are file-backed no-op updates. Pre-boot requests fail as post-boot-only operations. An ordinary active drive obtains a replacement backing only when `path_on_host` is present and applies configured limiter buckets before stored-state commit; direct mode opens the path, while contained mode claims an exact unused grant without reopening its tag. For a vhost drive only an ID-only request is valid: it performs one bounded repeated `GET_CONFIG(0, 60)` on the existing direct or contained stream, validates the complete reply before transport mutation, replaces the exact config, increments one generation, delivers one configuration interrupt, and records optional latest-value `config_change_time_us`. It does not reconnect or consume contained directory authority. Path or limiter fields reject before backend I/O. Acquisition or confirmed pre-delivery failure preserves old config/generation/interrupt state; delivery ambiguity terminalizes the session. Ordinary limiter retry/wakeup semantics remain backend-neutral and do not claim Firecracker's exact Linux timerfd/eventfd identity. |
| `PATCH` | `/network-interfaces/{iface_id}` | runtime rate-limiter updates implemented | Returns unsupported-state before startup, validates the target interface after startup, and accepts omitted, `null`, empty, or all-null `rx_rate_limiter` and `tx_rate_limiter` objects as runtime no-ops. In `Running` or `Paused`, configured bandwidth and ops buckets update the matching startup or hotplugged live RX/TX limiter and stored config. Omitted inner buckets preserve both stored values and exact live budget, enabled buckets start with a fresh full budget at one update instant, and explicit disabled buckets clear only the selected bucket. Active-device mutation completes before stored config is committed, so lookup, worker-command, or handler failures leave stored state unchanged. Limiter updates do not change virtio queue state, pending-work flags, config generation, or interrupt status; later retained work is scheduled from the updated live state. Limiter-specific metrics and snapshots remain deferred. |
| `DELETE` | `/drives/{drive_id}` | PCI-only non-root file and direct/contained-vhost runtime removal implemented | Firecracker routes this bodyless operation in `parsed_request.rs` outside the v1.16 Swagger surface. In Running or Paused public all-virtio PCI sessions, bangbang removes MMIO/ECAM visibility, drains admitted work/messages, then releases endpoint, MSI-X, BAR, function, dispatcher, metrics generation, and config ownership before capacity reuse. File drives release their backing; vhost drives additionally close the frontend/protocol stream, call/kick notifiers, wakeup visibility, and cloned shared-memory descriptors. Contained vhost releases its child lease but retains the exact session directory authority for a later reinsertion. Recoverable preparation failure restores the same usable endpoint; cleanup uncertainty or post-commit corruption is terminal. Missing/root/default-MMIO failures are nonmutating. Linux must remove the PCI function through guest sysfs before DELETE. A body still fails first with `Empty Delete request.`. |
| `DELETE` | `/pmem/{id}`, `/network-interfaces/{iface_id}` | implemented for supported public PCI profiles | Both pinned bodyless hot-unplug routes retain normal unsupported-state behavior before startup, and body-bearing requests fail first with `Empty Delete request.`. In a Running/Paused public PCI session, pmem DELETE requires prior guest sysfs removal, prepares endpoint teardown, synchronizes the exact persistent prefix, unregisters only that direct mapping, commits endpoint teardown, then releases backing, metrics generation, guest range, grant state, and configuration. Network DELETE likewise prepares reversible endpoint teardown, takes and explicitly stops the exact packet-I/O owner, then commits release of queues, retry state, metrics, MMDS/vmnet ownership, PCI resources, and live config. Pre-commit failures restore the endpoint and retained owner when provable; failed pmem unmap retains its mapping lease, while incomplete restoration, uncertain vmnet stop, and post-boundary corruption are terminal. Default MMIO, root pmem, and missing-device requests are nonmutating. |

## Initial Field Handling Policy

Field policy is based on Firecracker `v1.16.0` schemas and parser behavior. The
future API should use these tables as golden/API test input once JSON models
exist.

Firecracker-shaped rate limiter objects reject duplicate `bandwidth` or `ops`
fields and duplicate token bucket fields before VMM dispatch.

| Endpoint | Field | Handling | Notes |
| --- | --- | --- | --- |
| `PUT /boot-source` | `kernel_image_path` | required | Host path or contained grant tag for the kernel image. Empty values fail before file IO. Direct paths open read-only/nonblocking during startup; contained tags claim an exact `KernelImage` read-only descriptor during the successful request. Both paths reject inaccessible, non-regular, or empty payloads and redact path/tag details from API-facing errors. |
| `PUT /boot-source` | `initrd_path` | optional | Optional host path or contained grant tag for an initrd. Explicitly empty values fail before file IO; direct paths retain startup-time opening, while contained tags use the same request-time exact read-only claim and redacted validation policy as the kernel. |
| `PUT /boot-source` | `boot_args` | optional | Firecracker uses its default kernel command line when omitted. The API/VMM storage path validates the 2048-byte aarch64 limit including the trailing NUL byte and rejects embedded NUL bytes. |
| `PUT /boot-source` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /machine-config` | `vcpu_count` | required | Representable JSON reaches VMM validation. Firecracker and bangbang accept `1..=32`; out-of-range values return the typed value-redacted machine fault. On supported Apple Silicon hosts, HVF `InstanceStart` then admits `1..=min(32, host_max)`. A count above the host-reported maximum returns a stable capacity fault before a session is retained or `Running` is committed. |
| `PUT /machine-config` | `mem_size_mib` | required | Representable JSON reaches VMM validation. Bangbang accepts `1..=1046528` MiB and rejects a larger value before storage, while pinned Firecracker stores/echoes it and later truncates realized aarch64 memory to the same 1022-GiB maximum. Bangbang's accepted GET value therefore remains identical to balloon, allocation, FDT, HVF mapping, and snapshot memory. A configured balloon target must fit or the previous machine and balloon state is preserved. Dynamic host-free-memory preflight is not promised. |
| `PUT /machine-config` | `smt` | optional when `false`; rejected when `true` | Firecracker defaults this to `false`; the Apple Silicon target accepts explicit no-SMT config and returns `machine smt is not supported` when enabled. On combined-invalid aarch64 candidates this check precedes vCPU and memory. |
| `PUT /machine-config` | `cpu_template` | omitted/`null` preserves; `None` clears; `V1N1` retained pending; x86 names rejected | Explicit `None` transactionally clears static or custom selection. `V1N1` replaces custom state, remains visible through machine/VM GET, and can be replaced by a later custom PUT; if still effective, `InstanceStart` fails before its executor or HVF VM because the documented Neoverse V1 source model is unavailable on Apple Silicon. `C3`, `T2`, `T2S`, `T2CL`, and `T2A` remain foreign AWS/Linux policies rejected before mutation. Deprecated-field metrics retain their existing provenance rules. |
| `PUT /machine-config` | `track_dirty_pages` | optional boolean; default `false`; `true` implemented | A true value installs one shared epoch before normal boot population. Boot-loader, bounded VMM/current-device writes, conservative balloon discard, dynamic RAM, and signed-proven guest-CPU writes enter the same bitmap. PUT replacement and all semantic failures remain transactional and value-redacted. |
| `PUT /machine-config` | `huge_pages` | optional when `None`; exact `2M` platform-limited | Explicit `None` matches Firecracker's default. `2M` means exact Linux hugetlbfs backing, not alignment or an IPA granule. Odd MiB returns `machine mem_size_mib must be an even value when huge_pages is 2M`; an otherwise valid candidate returns `machine huge_pages 2M requires exact Linux hugetlbfs backing, which is unavailable on arm64 macOS/HVF`. Both are transactional and precede allocation/HVF construction. |
| `PUT /machine-config` | unknown or duplicate fields | rejected | Matches Firecracker's strict request model behavior. |
| `PATCH /machine-config` | `vcpu_count` | optional | When present, updates the stored vCPU count with the same `1..=32` bounds as `PUT`; omitted fields keep their current values. Startup applies the same runtime host-capacity bound as `PUT`. |
| `PATCH /machine-config` | `mem_size_mib` | optional | When present, updates the stored memory size with the same `1..=1046528` MiB target bound and configured-equals-realized policy as PUT; omitted/null fields keep their current values. A configured balloon target must fit or the previous machine and balloon state is preserved. |
| `PATCH /machine-config` | `smt` | optional when `false`; rejected when `true` | Matches the current `PUT` policy for the Apple Silicon target and currently returns `machine smt is not supported` when SMT is enabled. |
| `PATCH /machine-config` | `cpu_template` | same transactional preserve/clear/pending policy as PUT | Omitted/`null` preserves static or custom state, explicit `None` clears it, and `V1N1` replaces custom state but remains subject to the pre-backend start gate. X86 names and any invalid combined candidate preserve the previous selection. |
| `PATCH /machine-config` | `track_dirty_pages` | optional boolean; implemented | Partially updates the retained pre-boot value with the same complete runtime contract as PUT; omitted/null preserves the current value and a failed candidate does not mutate it. |
| `PATCH /machine-config` | `huge_pages` | optional when `None`; exact `2M` platform-limited | Matches PUT's even-MiB validation and exact hugetlbfs platform result without mutating the current machine or balloon state. |
| `PATCH /machine-config` | unknown or duplicate fields; empty patch | rejected | Unknown/duplicate fields remain strict JSON faults. `{}` and null-only candidates return Firecracker's stable `Empty PATCH request.` fault and do not silently succeed. |
| `PUT /snapshot/create` | `snapshot_type` | optional; `Full` supported, `Diff` rejected | Accepts `Full` and `Diff`, defaulting to `Full`. Only `Full` passes the native-v1 gate; `Diff` returns the snapshot-specific unsupported fault before namespace or capture work. |
| `PUT /snapshot/create` | `snapshot_path` | required; redacted, opened or anchor-adopted after preflight | Retained with redacted `Debug`; an admitted direct create opens its parent/final namespace only after paused/profile preflight. In contained mode an exact `bangbang-grant:<GrantId>/<SnapshotOutputChild>` claims or reuses a matching `SnapshotOutputDirectory`/`CreateChildren` anchor after complete preflight. The UTF-8 child is 1–255 bytes, contains no NUL or `/`, and is not `.` or `..`. It is never logged or echoed. |
| `PUT /snapshot/create` | `mem_file_path` | required; redacted, opened or anchor-adopted after preflight | Uses the same redaction, child grammar, and gate ordering as `snapshot_path`; one shared grant with distinct children, two distinct grants, or a mixed ordinary/granted pair is supported. Guest memory streams directly into the destination-anchored staging inode. |
| `PUT /snapshot/create` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /snapshot/load` | `snapshot_path` | required; redacted, opened or grant-adopted after preflight | The direct native-v1 loader opens it only after pristine/profile preflight. In contained mode an exact file tag selects `SnapshotStateInput`/`ReadOnly`; it is duplicated for bounded state decode without consumption and later atomically adopted with every tagged memory/root input. Diagnostics expose neither form. |
| `PUT /snapshot/load` | `mem_backend` | required unless deprecated `mem_file_path` is present; redacted | Parsed as a strict `backend_path`/`backend_type` object. Exactly one backend form is required. Direct `File` uses the no-follow loader; a contained exact tag selects `SnapshotMemoryInput`/`ReadOnly` and is loaded from its atomically adopted descriptor. |
| `PUT /snapshot/load` | `mem_backend.backend_type` | required when `mem_backend` is present | Accepts `File` and `Uffd`; only `File` passes the native-v1 gate, while `Uffd` returns the same snapshot-specific unsupported fault. |
| `PUT /snapshot/load` | `mem_file_path` | deprecated-compatible alternative; normalized | Must not be combined with `mem_backend`; it is normalized to a redacted `File` backend and retains deprecated-usage provenance. |
| `PUT /snapshot/load` | `enable_diff_snapshots` | deprecated-compatible optional boolean; normalized and implemented for tracking | ORed with `track_dirty_pages`; only true counts as deprecated usage. The effective value activates destination tracking but does not enable Diff artifact serialization. |
| `PUT /snapshot/load` | `track_dirty_pages` | optional boolean; implemented | The destination request overrides the source snapshot's active flag. Tracking attaches after image population and before mapping/protection, runner creation, VMGenID replacement, or guest progress. |
| `PUT /snapshot/load` | `resume_vm` | optional; implemented | Load always commits an initially paused real session first. `false` returns `204` in `Paused`; `true` then uses the ordinary process/session resume path and returns only in `Running`. |
| `PUT /snapshot/load` | `clock_realtime` | optional; retained, rejected when true | Retained through VMM policy; native-v1 rejects clock adjustment before any VM construction. |
| `PUT /snapshot/load` | `network_overrides` | optional; retained/redacted, rejected when nonempty | Required entry fields are retained but both interface ID and host device name are redacted; native-v1 does not apply overrides. |
| `PUT /snapshot/load` | `vsock_override` | optional; retained/redacted, rejected when present | The UDS path is retained but redacted; native-v1 does not apply the override. |
| `PUT /snapshot/load` | unknown `network_overrides` or `vsock_override` fields | accepted by parser | Matches Firecracker's current nested override parser, which ignores unknown fields in these objects while preserving typed validation for required fields. |
| `PUT /snapshot/load` | unknown or duplicate top-level fields; unknown or duplicate `mem_backend` fields | rejected | Matches Firecracker's strict top-level and memory-backend request model behavior. |
| `PUT /balloon` | `amount_mib` | required; stored pre-boot | Stored as an unsigned 32-bit Firecracker-shaped target balloon size and returned by `GET /balloon` and `GET /vm/config`. Values larger than current configured guest memory fail without mutating any prior balloon config. The internal virtio-balloon foundation converts this value to 4 KiB `num_pages` with checked arithmetic and exposes it through the startup-attached config space. Runtime `PATCH /balloon` can update the same stored target and active config-space value after startup. `GET /balloon/statistics` reports this current target through the required `target_*` fields and can add optional guest-reported fields from statistics queue reports. Completed inflate ranges use best-effort inward-aligned Darwin zero/free advice; partial edges and failures are measured without a synchronous footprint guarantee. |
| `PUT /balloon` | `deflate_on_oom` | required; stored pre-boot | Stored as a boolean and returned by `GET /balloon` and `GET /vm/config`. The internal foundation advertises `VIRTIO_BALLOON_F_DEFLATE_ON_OOM` only when this is enabled. Real guest OOM deflation behavior remains deferred. |
| `PUT /balloon` | `stats_polling_interval_s` | optional; stored pre-boot | Missing values follow Firecracker's parser default shape and are stored as `0`. Nonzero values add the internal statistics feature bit and queue metadata. Runtime dispatch can record bounded guest statistics queue reports, and process-level periodic scheduling can complete a pending report descriptor when runtime policy requests an update. `PATCH /balloon/statistics` can update nonzero intervals after startup without changing whether statistics are enabled. Runtime statistics enable/disable transitions remain deferred. |
| `PUT /balloon` | `free_page_hinting` | optional; stored pre-boot | Missing values follow Firecracker's parser default shape and are stored as `false`. `true` adds the internal free-page hinting feature bit and queue metadata. Runtime `PATCH /balloon/hinting/start`, `PATCH /balloon/hinting/stop`, and `GET /balloon/hinting/status` can update and report host-owned command state and 4-byte guest command acknowledgements when this is enabled. Runtime dispatch validates active-run range descriptors and applies the same best-effort Darwin discard used by inflate; stale/inactive runs remain ignored. |
| `PUT /balloon` | `free_page_reporting` | optional; stored pre-boot and dispatched at runtime | Missing values follow Firecracker's parser default shape and are stored as `false`. Explicit `false` or `true` is stored and returned by `GET /balloon` and `GET /vm/config`. `true` advertises the reporting feature and adds a compacted reporting queue. Runtime and HVF notification paths accept bounded device-writable reporting descriptors, validate non-empty mapped ranges with checked arithmetic, run best-effort inward-aligned Darwin discard before used-ring completion, and record requested, advised, skipped, and failed work separately. Invalid or unserviceable descriptors fail independently without blocking later available chains. |
| `PUT /balloon` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `GET /balloon/hinting/status` | response body | runtime host and guest command status implemented | Before startup this remains a state-specific unsupported action. After startup, requests without a configured balloon or with `free_page_hinting: false` return the existing balloon unsupported fault. With `free_page_hinting: true`, bangbang returns Firecracker-shaped `host_cmd` and `guest_cmd` fields from active device state; initial state is `host_cmd: 0` and `guest_cmd: null`, start advances `host_cmd`, stop reports Firecracker's done command, and a 4-byte hinting queue descriptor updates `guest_cmd`. Active-run range descriptors are validated and discarded best effort on Darwin, and completed guest `STOP(0)`/`DONE(1)` commands can automatically acknowledge host `DONE(1)`. |
| `PATCH /balloon` | `amount_mib` | required; runtime target update implemented | Parsed as an unsigned 32-bit Firecracker-shaped target balloon size before VMM dispatch. After startup with a configured balloon device, the value replaces the stored `amount_mib`, updates active virtio-balloon `num_pages`, increments config generation, and raises a config interrupt. Values larger than configured guest memory or not representable as 4 KiB pages fail without mutating stored config. |
| `PATCH /balloon` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PATCH /balloon/statistics` | `stats_polling_interval_s` | required; runtime nonzero interval update implemented | Parsed as an unsigned 16-bit Firecracker-shaped polling interval before VMM dispatch. After startup with a configured balloon, unchanged intervals are accepted, nonzero-to-nonzero changes update stored and active balloon state, and zero/nonzero enabled-state changes fail without mutation. The updated interval feeds process-level periodic scheduling, which can complete a pending statistics descriptor when runtime policy asks for an update. |
| `PATCH /balloon/statistics` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PATCH /balloon/hinting/start` | body | optional when absent or empty | Missing or empty bodies use Firecracker's default hinting start command before VMM dispatch. An empty JSON array is also accepted as a default command, matching the current Firecracker Serde parser behavior. After startup with `free_page_hinting: true`, valid requests update host command state and return `204 No Content`. |
| `PATCH /balloon/hinting/start` | `acknowledge_on_stop` | optional | Missing values follow Firecracker's default `true` command shape. The current implementation preserves the value in host-owned device state for automatic host `DONE(1)` acknowledgement when the guest later sends `STOP(0)` or `DONE(1)`. Hinting queue command acknowledgements, active-run range validation, and best-effort Darwin discard are implemented. |
| `PATCH /balloon/hinting/start` | unknown fields | accepted by parser | Matches Firecracker's current hinting start command parser, which ignores unknown fields while preserving typed validation for `acknowledge_on_stop`. |
| `PUT /drives/{drive_id}` | path `drive_id` | required | The API parser captures this value, and the internal model validates it as nonempty alphanumeric or `_`, matching Firecracker's `checked_id` rule. |
| `PUT /drives/{drive_id}` | body `drive_id` | required | The API parser rejects requests where this does not match the path `drive_id`. |
| `PUT /drives/{drive_id}` | `is_root_device` | required | Identifies whether this drive is the boot device. |
| `PUT /drives/{drive_id}` | `path_on_host` | required for file-backed virtio-block; omitted for vhost-user-block | The API/VMM path records this value only after rejecting empty paths. File-backed pre-boot paths retain deferred startup opening; ordinary runtime PCI paths open on the API thread before owner submission. Direct macOS opening accepts only an existing regular file or exact block-special node with final-component no-follow and matching access. Regular capacity is metadata length; block capacity is checked public `DKIOCGETBLOCKSIZE * DKIOCGETBLOCKCOUNT` geometry. In contained mode an exact grant tag claims one regular or block-special `DriveBacking`; BBG2 binds kind, device/inode/rdev, exact access/status, block geometry/capacity, and the transferred descriptor. The worker independently rechecks fstat/fcntl and never reopens the tag. Because App Sandbox denies disk geometry and cache-sync ioctls, fresh contained inspection and persistence use only the launcher's retained exact descriptor through the fixed session/sequence/grant-bound block-control facet. Runtime PCI insertion reserves an unused initial tag under rollback ownership until endpoint publication commits. Failure restores authority with no ambient fallback; success consumes it once. Same-ID regular/block replacement is failure-atomic. Native-v1 capture remains regular-only: block-special capture-ready inspection is in-memory and rejects before artifact publication, while regular contained restore retains its existing exact identity checks. All socket-backed requests must omit this field; mixed file/socket bodies fail before connection or mutation. Errors and debug output redact paths, tags, identities, geometry values, and descriptors. |
| `PUT /drives/{drive_id}` | `is_read_only` | optional for file-backed drives; omitted for vhost-user | File-backed drives default to read-write. A socket-backed request must omit this field because read-only capability is negotiated from the backend and then exposed to the guest; an explicit value fails before connection or mutation. |
| `PUT /drives/{drive_id}` | `partuuid` | optional | Only meaningful for root-device boot selection and supported for either backend; signed PCI vhost evidence boots an MBR partition through the exact PARTUUID argument. |
| `PUT /drives/{drive_id}` | `cache_type` | optional when `Unsafe`; supported when `Writeback` | Both backends accept omitted/default `Unsafe` and explicit `Writeback`. File-backed `Unsafe` suppresses FLUSH and `Writeback` advertises it. Regular-file flush uses `sync_data`; direct block-special flush uses public `DKIOCSYNCHRONIZECACHE`; contained block-special flush uses the same ioctl only through the launcher's exact retained-descriptor control facet because App Sandbox rejects it in the worker. Vhost discovery excludes FLUSH from the Unsafe requested intersection and permits it for Writeback only when the backend offers it. |
| `PUT /drives/{drive_id}` | `rate_limiter` | optional bandwidth/ops token buckets for file-backed drives; omitted for vhost-user | File-backed missing/null/empty/all-null values are unconfigured; valid buckets are stored, reported, and applied without sleeping. A socket-backed request must omit this field because vhost limiting is not implemented. |
| `PUT /drives/{drive_id}` | `io_engine` | optional `Sync` or `Async` for file-backed drives; omitted for vhost-user | File-backed omission defaults to `Sync`; explicit `Sync` and `Async` are stored and exactly reported. `Async` uses one lazy bounded portable executor per VM session, with owner-thread completion publication over MMIO or PCI, instead of claiming Linux io_uring on macOS. Startup, path PATCH, same-ID engine/backing replacement, runtime PCI hotplug/DELETE/reuse, reset, pause, and shutdown are generation-safe. Native-v1 remains Sync-only: paused create drains, captures, and reopens the live Async generation before rejecting the profile without artifact creation. A socket-backed request must omit the field because I/O execution belongs to the external backend. |
| `PUT /drives/{drive_id}` | `socket` | optional; direct and contained startup plus eligible PCI runtime vhost-user block implemented | A nonempty socket selects the vhost backend only when `path_on_host`, explicit `is_read_only`, `io_engine`, and `rate_limiter` are absent; drive ID, root selection, `partuuid`, and cache mode remain valid. Direct mode accepts an operator path. Contained mode accepts only `bangbang-grant:<GrantId>/<SocketChild>` backed by an exact repeatable `VhostUserSocketDirectory + ConnectChildren` grant and never attempts ambient access. Successful pre-boot requests store and exactly report the submitted socket without inventing a path. `InstanceStart` obtains one bounded redacted stream, performs strict feature/protocol/CONFIG discovery before VM construction, selects shared RAM, and activates one queue over MMIO or PCI. Virtio-mem may be configured in either pre-boot order; its complete aperture is reserved before block preparation and exported with boot RAM in one immutable table while offline bytes stay outside guest CPU/HVF/current accounting. In Running or Paused all-PCI state, a new non-root ID may connect only after the owner proves the live profile is already shared and all deterministic publication capacity is available; publication commits last and caller-coordinated DELETE releases the complete endpoint without removing the VM-owned aperture. Contained runtime requests perform the same owner preflight before reserving a child lease or using the dedicated broker. ID-only PATCH refreshes the existing active stream; same-ID PUT remains duplicate rejection without a broker request. Failure drops candidates and preserves public/live configuration. One contained directory authority may serve multiple exact children, retry after a broker `Failed`, and reinsertion after DELETE. Native-v1 capture remains incompatible. |
| `PUT /drives/{drive_id}` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PATCH /drives/{drive_id}` | path `drive_id` | required | The API parser captures this value before building the runtime update action. |
| `PATCH /drives/{drive_id}` | body `drive_id` | required | The API parser rejects requests where this does not match the path `drive_id`. |
| `PATCH /drives/{drive_id}` | `path_on_host` | optional | When present at runtime, the process opens or adopts the replacement regular or block-special backing before committing stored configuration. In contained mode it reserves an exact still-unused startup-batch drive grant and passes the opened descriptor plus narrow block-control capability without reopening the tag. Open, geometry, access, Async preparation/quiescence, broker inspection, and handler failures leave the old backing, capacity/config generation, engine, stored configuration, and grant authority intact. Successful regular-to-block, block-to-regular, or block-to-block replacement atomically commits capacity and backing-derived GET_ID; the old Async generation and descriptor owners then tear down. When omitted, the existing backing and engine are retained and no grant is claimed. |
| `PATCH /drives/{drive_id}` | `rate_limiter` | optional bandwidth/ops token-bucket update | Missing, `null`, empty-object, or all-null `bandwidth`/`ops` values are accepted as no-op updates. Configured Firecracker-shaped buckets are validated and applied per bucket to the existing stored and active drive limiter; omitted buckets keep their previous values, while disabled buckets clear the corresponding limiter bucket. |
| `PATCH /drives/{drive_id}` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /pmem/{id}` | path `id` | required | The API parser captures the path ID for path/body validation before routing valid requests through the VMM state/action policy. Invalid path IDs continue to fail as invalid path/method, and the runtime model also rejects empty IDs or IDs containing characters other than alphanumeric characters and `_`. |
| `PUT /pmem/{id}` | body `id` | required | The API parser rejects requests where this does not match the path `id`. |
| `PUT /pmem/{id}` | `path_on_host` | required; directly mapped, range-assigned, guest-attached, HVF-registered, and synchronizable | Required Firecracker-shaped host backing path. The value is retained and reported in `GET /vm/config` after rejecting empty paths. Pre-boot ordinary paths open at startup; runtime PCI ordinary paths open before the owner command so failure cannot commit configuration. In contained mode an exact grant tag claims one `PmemBacking` descriptor with access matching `read_only`; pre-boot PUT retains it by pmem ID for startup, while runtime PUT uses a rollback claim and consumes it only after live publication. Both paths avoid reopening the tag and are failure-atomic. The owner maps the nonzero regular file plus volatile alignment tail into one retained 2 MiB-rounded range, assigns matching guest physical range/config-space metadata, and shares cloned leases with HVF while attaching the selected transport. Runtime first-fit allocation excludes DRAM, the full virtio-mem reservation, and every live pmem range; DELETE synchronizes the exact file prefix and releases that exact dynamic mapping and range only after teardown. Errors redact path, tag, descriptor, and host-address details. Path normalization remains deferred. |
| `PUT /pmem/{id}` | `root_device` | optional; supported before startup | Missing values default to `false` and are reported in `GET /vm/config`. A `true` request selects the device's stable pmem-list index for `root=/dev/pmem<i>` plus `ro` or `rw`. Validation permits exactly one root across ordinary block and pmem devices, excludes the same ID during failure-atomic replacement, preserves list position, and completes before grant claim or backing preparation. Runtime root insertion, replacement, and removal remain rejected. |
| `PUT /pmem/{id}` | `read_only` | optional; backing access, host mapping, HVF permission access, guest attachment, and persistence policy | Missing values default to `false` and are reported in `GET /vm/config`. Direct paths open at startup or before a runtime owner command; contained tags claim during successful pre-boot PUT or reserve through a rollback claim during runtime PUT. Every path requires read-only descriptor access when this is `true` and read/write access when it is `false`. HVF guest registration is read-only or read/write and always non-executable. HVF requires the host address itself to be write-capable, so a read-only backing uses a private host mapping whose accidental writes are copy-on-write; a writable backing uses the shared mapping observed by the file. Queue flush and graceful teardown call exact-prefix `MS_SYNC` for either profile without claiming the volatile tail. |
| `PUT /pmem/{id}` | `rate_limiter` | optional bandwidth/ops token-bucket configuration | Empty, all-null, or omitted limiter shapes do not create stored limiter state. Valid configured buckets are normalized, stored, reported through `GET /vm/config`, and applied per device to coalesced flush events. |
| `PUT /pmem/{id}` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PATCH /pmem/{id}` | path `id` | required | The API parser captures the path ID for path/body validation before routing valid requests through the VMM state/action policy. Invalid path IDs continue to fail as invalid path/method. |
| `PATCH /pmem/{id}` | body `id` | required | The API parser rejects requests where this does not match the path `id`. |
| `PATCH /pmem/{id}` | `rate_limiter` | optional bandwidth/ops token-bucket update | Missing, `null`, empty, or all-null objects are runtime no-ops. Present enabled buckets replace the corresponding live and stored bucket, disabled buckets clear it, and omitted inner buckets preserve the existing bucket. Exact-device handler or delivery failures leave stored configuration unchanged. |
| `PATCH /pmem/{id}` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /network-interfaces/{iface_id}` | path `iface_id` | required | The API parser captures this value, and the internal model validates it as nonempty alphanumeric or `_`, matching Firecracker's `checked_id` rule. |
| `PUT /network-interfaces/{iface_id}` | body `iface_id` | required | The API parser rejects requests where this does not match the path `iface_id`. |
| `PUT /network-interfaces/{iface_id}` | `host_dev_name` | required | The API/VMM path records this value only after rejecting empty values and enforcing the current 16-interface bangbang limit; it does not open, stat, or otherwise touch host networking resources during configuration. `InstanceStart` later accepts only `vmnet:host`, `vmnet:shared`, and `vmnet:bridged:<interface>` for vmnet packet I/O startup. |
| `PUT /network-interfaces/{iface_id}` | `guest_mac` | optional | The internal model accepts six colon-separated two-hex-digit octets, normalizes display to lowercase hex, and rejects duplicate configured MAC addresses across different interface IDs. |
| `PUT /network-interfaces/{iface_id}` | `mtu` | optional | The internal model accepts Firecracker-compatible `68..=65535` values, stores them with the interface config, advertises `VIRTIO_NET_F_MTU`, and exposes the value through virtio-net config space. This guest-advertised value is not reconciled with Apple's separately returned vmnet MTU; host vmnet MTU changes remain out of scope. |
| `PUT /network-interfaces/{iface_id}` | `rx_rate_limiter`, `tx_rate_limiter` | optional; initial enabled buckets implemented | Missing, `null`, empty, and all-null limiter objects are unconfigured. Buckets with zero `size`, zero `refill_time`, or an overflowing millisecond conversion are explicit disabled controls and normalize away. Enabled bandwidth/ops values round-trip through `GET /vm/config` and create independent directional device budgets. Admission consumes one op plus complete guest-visible frame bytes atomically; one oversized frame can progress from a full byte bucket, and only successful MMDS TX detours refund the reservation. Runtime bucket updates and per-session HVF timed retry wakeups are implemented; pending work is retried on the boot-session owner thread after earliest-deadline replenishment without claiming Firecracker's Linux timerfd/eventfd identity. |
| `PUT /network-interfaces/{iface_id}` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PATCH /network-interfaces/{iface_id}` | path `iface_id` | required | The API parser captures this value before routing valid requests through the runtime lifecycle policy. |
| `PATCH /network-interfaces/{iface_id}` | body `iface_id` | required | The API parser rejects requests where this does not match the path `iface_id`. |
| `PATCH /network-interfaces/{iface_id}` | `rx_rate_limiter`, `tx_rate_limiter` | optional; runtime bucket updates implemented | Omitted, `null`, empty, or all-null rate limiters are runtime no-ops for existing interfaces after startup. A missing inner `bandwidth` or `ops` bucket preserves its stored value and exact live budget. An enabled bucket replaces only that bucket with a fresh full budget, while zero-sized, zero-refill, or overflowing-refill buckets explicitly disable only the selected bucket. RX and TX replacements are staged at one instant before assignment; active mutation succeeds before stored config commit. |
| `PATCH /network-interfaces/{iface_id}` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /vsock` | `vsock_id` | optional and deprecated | Firecracker `v1.16.0` accepts this field but treats it as deprecated. The internal model accepts it when present and rejects empty or control-character values. `GET /vm/config` omits this deprecated field. |
| `PUT /vsock` | `guest_cid` | required | Firecracker's published schema requires a 32-bit guest CID with minimum value `3`; smaller values are rejected before state mutation. |
| `PUT /vsock` | `uds_path` | required | Host Unix socket path used for startup listener preparation. Direct mode records the value after rejecting empty paths and control characters without opening or creating a socket; relative paths remain accepted to match Firecracker's documented `./v.sock` examples. Contained mode additionally classifies exact `bangbang-grant:<GrantId>/<SocketChild>` references, validates the one-component ASCII child, and claims/retains the exact typed directory authority during the successful request without creating or reopening the submitted value. Startup later binds a direct listener or publishes the supplied granted listener. Authorized `GET /vm/config` retains the submitted value; diagnostics redact it. |
| `PUT /vsock` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /metrics` | `metrics_path` | required | Host path to the metrics output file or FIFO. The runtime opens it as per-process observability state and redacts path details from API-facing open errors. |
| `PUT /metrics` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /logger` | `log_path` | optional | Host path to the logger output file or FIFO. When present, the runtime opens it as per-process observability state and redacts path details from API-facing open errors. When omitted, the existing sink is left unchanged. |
| `PUT /logger` | `level` | optional | Case-insensitive values `Off`, `Trace`, `Debug`, `Info`, `Warn`, `Warning`, and `Error` are accepted. `Warning` is normalized to `Warn`. |
| `PUT /logger` | `show_level` | optional | When true, implemented API request, action, and boot-timer log lines include a `level=Info` prefix. |
| `PUT /logger` | `show_log_origin` | optional | When true, implemented API request, action, and boot-timer log lines include an `origin=<file>:<line>` field for the callsite. |
| `PUT /logger` | `module` | optional | Filters implemented logger events with Firecracker-style module-path prefix matching. API request method/path lines use `bangbang_runtime::api_server`, action logs use `bangbang_runtime::vmm_action`, and boot-timer logs use `bangbang_runtime::boot_timer`; non-matching filters suppress those lines without failing the action. |
| `PUT /logger` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /serial` | `serial_out_path` | optional | Host path to the serial output file or FIFO. The runtime stores it before boot, startup opens it as per-process observability output, and API-facing open errors redact path details. Omit the field or set it to `null` to clear the configured public output path. |
| `PUT /serial` | `rate_limiter` | optional token bucket | Missing or `null` values are accepted. Firecracker-shaped token buckets with `size`, optional `one_time_burst`, and `refill_time` are stored before boot. At startup, `size=0`, `refill_time=0`, or overflowing millisecond-to-nanosecond refill intervals disable the limiter; otherwise the limiter starts full, applies the optional one-time burst, refills over time, and drops exhausted output bytes without blocking. |
| `PUT /serial` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /entropy` | `rate_limiter` | optional bandwidth/ops token buckets | Missing, `null`, empty-object, or all-null `bandwidth`/`ops` values are accepted as unconfigured. Configured Firecracker-shaped rate limiter objects with non-null `bandwidth` or `ops` buckets are validated, stored before startup, echoed through `GET /vm/config`, and applied to virtio-rng queue dispatch without sleeping. |
| `PUT /entropy` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /hotplug/memory` | `total_size_mib` | required; semantically validated and stored | Required Firecracker-shaped hotpluggable-memory total size. The parser accepts syntactically valid unsigned integer values, then runtime validation requires the value to be at least the slot size and a multiple of slot size before storing the pre-boot-only memory-hotplug config. Startup exposes this size in the virtio-mem config space; broader public guest-memory accounting remains deferred. |
| `PUT /hotplug/memory` | `block_size_mib` | optional; semantically validated and stored | Missing values use Firecracker's default `2` MiB shape. Present values must be unsigned integers at least `2` MiB and powers of two before storage. |
| `PUT /hotplug/memory` | `slot_size_mib` | optional; semantically validated and stored | Missing values use Firecracker's default `128` MiB shape. Present values must be unsigned integers at least `128` MiB and multiples of block size before storage. |
| `PUT /hotplug/memory` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PATCH /hotplug/memory` | `requested_size_mib` | required; runtime requested-size update implemented | Required Firecracker-shaped target hotpluggable-memory size. The parser accepts syntactically valid unsigned integer values, then runtime validation requires the value to be no larger than the configured total size and a multiple of the configured block size. Successful post-start requests update stored status and active virtio-mem config-space requested size; active plugged-block status is reported through `GET /hotplug/memory`, and accepted guest `PLUG`/`UNPLUG` requests apply HVF dynamic memory mutations. Broader public guest-memory accounting remains deferred. |
| `PATCH /hotplug/memory` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /actions` | `action_type=InstanceStart` | process-routed; internal startup execution across bounded step windows implemented | Validates stored boot source and state, prepares an owned HVF session with configured serial TX or bounded internal capture, starts the worker, and commits `Running` after retaining its handle. Success returns `204`. The action logger record is best effort; its failure increments `missed_log_count` but cannot undo startup or replace the response. Preparation or worker-start failure returns a fault without committing the session. Public serial RX/streaming and run-loop control beyond the current pause/resume subset are absent. |
| `PUT /actions` | `action_type=FlushMetrics` | runtime-only explicit execution implemented | Rejected before startup. After startup, an unconfigured sink is a `204` no-op; a successful configured sink appends one interval/store line and returns `204`; and a sink failure returns the metrics fault while retaining the previous-success baseline. The parsed request and successful action logger records are unrestricted and best effort. Automatic initial, 60-second Running/Paused periodic, and normal-terminal attempts use the same payload transaction but are not `/actions` requests and create no action logger record. |
| `PUT /actions` | `action_type=SendCtrlAltDel` | intentionally unsupported; parser rejected | Firecracker gates this on x86 keyboard behavior; the first target is Apple Silicon. The request is still counted in `put_api_requests.actions_count` without an `actions_fails` increment. |
| `PUT /actions` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |

The API and VMM state path implement the `PUT /machine-config` and
`PATCH /machine-config` field policies above. Valid pre-boot
`PUT /machine-config` requests replace the stored full machine configuration,
while valid pre-boot `PATCH /machine-config` requests update only provided
fields and preserve omitted stored values. Both return `204 No Content` on
success; invalid updates leave the stored configuration unchanged.
`GET /machine-config` returns the stored or default configuration. The stored
values are applied during `InstanceStart` startup.

Machine JSON syntax, representation, required fields, strict fields, and enum
names are parser-owned. Representable semantic candidates reach one VMM
validator. Its owned aarch64 precedence is SMT, vCPU, memory range, selected
page compatibility, CPU-template policy, dirty tracking, and the final exact-2M
platform result. See the checked
[machine-memory contract](../compat/firecracker/v1.16.0/machine-memory-contract.md)
for pinned sources, the oversized-policy comparison, exact 2M platform evidence,
alternatives, stable errors, and validation anchors.

Entropy support is tracked by #797 and needs to move as one guest-visible
capability, not only a configuration endpoint. The current supported subset is
one Firecracker-shaped virtio-rng device configured before startup with optional
Firecracker-shaped `bandwidth` and `ops` rate-limiter buckets, attached as a
virtio-mmio device in the arm64 FDT, backed by host OS randomness, and
dispatched from the HVF boot loop. A backend-neutral virtio-rng queue handler
and runtime MMIO activation/notification layer can fill writable guest
descriptor chains from an injected entropy source under unit tests, including
malformed-buffer, source-failure, reset, rate-limited retry, and queue-interrupt
completion paths. The public API can now store the empty entropy configuration,
include it in `GET /vm/config` as `"entropy": {}`, and pass it into
`InstanceStart` so the existing HVF startup path attaches the device. The
config-file startup path accepts the same missing, null, empty-object, or
all-null bucket rate-limiter entropy configuration and rejects configured rate
limiters before publishing readiness. The signed
executable HVF e2e target now validates the guest-visible path by booting the
generated direct-rootfs image, checking that Linux selected `virtio_rng` as the
current hardware RNG, and reading non-empty data from `/dev/hwrng` before
writing a host-observable marker. Aggregate entropy metrics plus per-session
limiter retry scheduling are implemented; Firecracker's shared Linux
timerfd/eventfd event-source identity remains outside this supported subset.

The API and VMM state path route valid snapshot requests through explicit
actions. `PUT /snapshot/create` remains an unsupported state before startup and
while running; while paused, unsupported shapes/profiles fail before artifact
work and the accepted native-v1 Full profile invokes production publication.
The worker holds scoped supervisor admission plus acknowledged block, PMEM,
network, and entropy retry quiescence through aggregate capture, complete memory
streaming, publisher verification/synchronization, the exclusive memory-first/
state-last commit, and a successful-publication hook. SIGINT/SIGTERM can cancel
before the atomic commit seal; after the seal the publisher preserves its exact
typed visibility result before shutdown continues. The synchronous process call
serializes API/MMDS/controller and periodic work until release. Success returns
`204` with the source still paused. Contained create uses retained
output-directory anchors and bounded children for the same transaction; direct
create keeps pathname parents. `PUT /snapshot/load` is pre-boot-only,
validates process freshness and the committed pair before VM construction,
commits a real restored session as `Paused`, and optionally uses ordinary
resume. Contained load preinspects granted state, atomically adopts tagged
state/memory/persisted-root inputs, and never reopens their selectors; direct
load keeps no-follow pathname adapters. Malformed bodies are rejected by the
parser first, while execution faults remain typed and path/reference-redacted.

Firecracker's implementation saves
microVM state, KVM VM state, vCPU state, and device-manager state, writes a
separate guest-memory image, can load memory from a file or Linux userfaultfd,
can enable KVM dirty-page tracking for diff snapshots, and can apply
network/vsock restore overrides before optionally resuming the VM. bangbang
supports only its one-vCPU/read-only-root native-v1 baseline, including public
paused handoff, optional resume, and recoverable-versus-terminal cleanup
evidence. Its shared page epoch combines exact owned HVF guest-CPU write faults
with bounded boot, VMM, device, discard, and dynamic-memory mutations. Normal
boot starts tracking before population; load-time tracking starts after the
image baseline and records VMGenID replacement. Visible Full publication
transactionally re-protects restored pages before clearing and advancing the
epoch; failed rollback poisons the paused VM and prevents resume without
misreporting artifact visibility. Optional resources, overrides, `Diff`
artifacts, and broader portability remain deferred; unknown HVF feasibility
should not be reported as a platform limit by default. The baseline excludes network/vsock devices and
joins transient vsock polling before the pause boundary. Bangbang therefore
quiesces its own access but neither freezes nor persists vmnet/vsock peer-owned
host or kernel buffers. bangbang has a native
fixed outer state envelope with exact `1.0.0`, arm64, 4096-byte page-size and
CRC-64/Jones validation. It also has internal handle-level memory image and
state-binding primitives: exact GPA ranges map to canonical absolute offsets,
full bytes stream through a bounded buffer, and a validated image loads into an
explicit internal anonymous or shared profile only after identity, length,
CRC, and EOF checks. Public restore still selects anonymous memory. The public
process create transaction publishes a close-proven composite capture, and the
public load transaction decodes that HVF payload and commits an initially
paused restored session.
Firecracker on-disk compatibility was
explicitly rejected because KVM/device state has no proven HVF mapping. The
current feasibility boundary and follow-up split are documented in
[Snapshot Feasibility](snapshot-feasibility.md).

`GET /vm/config` returns the accumulated supported VM configuration subset
without side effects. It includes the stored/default `machine-config`, includes
`boot-source` only after it is configured, and always includes a `drives` array
for configured virtio-block drives plus a `network-interfaces` array for stored
network interface configs. It includes `vsock` only after `PUT /vsock` stores a
valid configuration. It includes `mmds-config` after successful MMDS
configuration storage. It includes `entropy` as an empty object after
successful `PUT /entropy` configuration, includes `memory-hotplug` after
successful `PUT /hotplug/memory` configuration, and includes `balloon` after
successful `PUT /balloon` configuration. Firecracker sections without stored
configuration models, including snapshots and remaining hotplug state, are
omitted until those models exist.
Metrics and logger output configuration are also omitted because they are
process observability state rather than guest configuration.

The API and VMM state path implement the `PUT /boot-source` field policy above.
Valid pre-boot requests replace the stored boot-source configuration and return
`204 No Content`; invalid requests fail without mutating existing state or
echoing host path and boot-argument values. The public API path stores path
values at configuration time; `InstanceStart` opens kernel and initrd host paths
read-only/nonblocking, rejects inaccessible, non-regular, or empty payloads
without echoing the private path in API-facing load errors, loads accepted
payloads, builds an FDT, configures vCPU registers, and retains the owned HVF
boot run-loop worker only after preparation succeeds.

The API and VMM state path implement the `PUT /actions` field policy above for
`InstanceStart` and `FlushMetrics` and rejects malformed bodies before VMM state
mutation. Parsed actions now route to explicit runtime VMM actions.
`InstanceStart` validates that a boot source exists in `Not started` state before
startup preparation is attempted; when preflight succeeds, the process VMM owner
prepares an owned HVF boot session, starts a process-owned internal boot
run-loop worker across bounded step windows, and marks the instance `Running`
only after that worker handle is retained.
The process startup path and API/VMM state path implement the metrics field
policy above as a pre-boot-only per-process output sink. Startup CLI can
initialize the metrics sink before the API socket is served. Duplicate
initialization fails without replacing the original sink. Configuration alone
writes nothing. After the first retained session, the process makes one
best-effort initial attempt, arms a session-epoch-based 60-second scheduler for
both Running and Paused, and makes one best-effort final attempt on normal
convergence. Every periodic attempt rearms even when output is unconfigured or
fails. None of these automatic paths creates an `/actions` record or changes
the process result.

Explicit `FlushMetrics` remains runtime-only and fallible. It fails before
startup, succeeds without output when unconfigured, writes one transactional
line on success, and returns a configured-sink error while retaining the last
successful baseline. The detailed producer classes, increment/store mapping,
sparse omission, generation replacement, and ambiguous at-least-once retry
rules are defined in the observability contract above. API request fields that
Firecracker does not define, absent device producers, and empty optional
families remain absent rather than being fabricated for shape completeness.
The process startup path and API/VMM state path implement the logger field
policy above as pre-boot-only per-process observability configuration. Startup
CLI flags can configure the initial logger before the API socket is served.
Repeated pre-boot `PUT /logger` requests update only the fields they provide,
including after startup CLI configuration. Runtime requests fail without opening
a new output path. The configured logger sink records the method and path for
successfully parsed API requests before dispatch, without logging request
bodies. It also records minimal successful `InstanceStart` and `FlushMetrics`
action lines when the logger level allows `Info`. When `--boot-timer` is
enabled, the same sink records the Firecracker-shaped `Guest-boot-time` line
after the guest writes the boot timer magic byte. `show_level` adds
`level=Info`, and `show_log_origin` adds the API server, runtime action, or
boot timer callsite as `origin=<file>:<line>`. `module` filters API request logs
against `bangbang_runtime::api_server`, action logs against
`bangbang_runtime::vmm_action`, and boot timer logs against
`bangbang_runtime::boot_timer`. Request and action records are unrestricted;
boot-timer records use the ten-per-five-second limiter and recovery warning.
Sink contention or failure increments `missed_log_count` and never changes the
functional outcome. No global process logging, panic/fatal writer, rotation, or
external telemetry backend is claimed.
The API and VMM state path implement the `PUT /vsock` field policy above as a
pre-boot-only guest configuration section. Valid requests replace the stored
vsock config and return `204 No Content`; invalid requests fail without
mutating previous vsock state. The current response reports `guest_cid` and
`uds_path` while omitting the accepted deprecated `vsock_id`, matching
Firecracker's effective config output. Startup preparation attaches the
configured virtio-vsock device as guest-visible FDT/MMIO metadata. The runtime
also models Firecracker's 44-byte little-endian virtio-vsock packet header and
can parse guest-readable TX descriptor chains into validated packet metadata and
payload segments. Header byte parsing rejects payload lengths above the 64 KiB
maximum packet buffer length, and TX parsing rejects payload lengths larger
than readable descriptor bytes after the header. The handler can dispatch TX
queue notifications into descriptor completions and can dispatch RX work into
pending host request-header delivery, including retrying delivery for pending
host requests after host-side `CONNECT` polling. Startup can dispatch those
notifications through the boot loop while signaling the allocated vsock
interrupt line when completed descriptors require it. Startup can also bind and
own a nonblocking host listener at `uds_path`. The runtime also parses
Firecracker-shaped host `CONNECT <PORT>` requests, allocates Firecracker-shaped
host local ports, retains host-initiated accepted streams in an internal table,
can expose a one-shot guest-facing `VSOCK_OP_REQUEST` packet header for a
retained host connection, can dispatch that pending request header into
writable guest RX descriptors with used-ring completion metadata, can accept
one pending host connection per dispatch pass into an owned nonblocking stream,
can retain bounded accepted streams across partial handshakes and retained
connection records, can drop invalid accepted-stream handshakes without path
exposure, can retry RX delivery when pending host requests exist without
requiring a fresh guest RX notification, and can acknowledge guest
`VSOCK_OP_RESPONSE` packets for delivered host requests by writing
`OK <local_port>\n` to the retained host stream. The runtime can also queue
bounded zero-payload `VSOCK_OP_RST` headers for unsupported or orphan
host-destined guest TX packets and deliver those reset headers through the
existing RX queue path. Supported guest `VSOCK_OP_REQUEST` packets can attempt
nonblocking connects to Firecracker-shaped `${uds_path}_${PORT}` sockets,
retain successful guest-initiated streams, and deliver guest-visible
`VSOCK_OP_RESPONSE` headers; established host-initiated or guest-initiated
connections can forward bounded `VSOCK_OP_RW` payload bytes to the retained
host stream, keep a bounded four-packet per-connection guest-to-host retry queue
for partial or would-block nonblocking writes, and retry pending bytes on later
notification dispatch before accepting more guest `RW` data for the same
connection;
established host-initiated and guest-initiated connections can retain a bounded
four-packet per-connection backlog of host `VSOCK_OP_RW` payloads and deliver
one queued payload at a time into guest RX buffers. Both initiation directions
track dynamic 64-KiB credit windows with wrapping counters, reserve queued peer
bytes before publication, release locally forwarded bytes, and exchange credit
requests/updates when a peer window is exhausted. Guest `VSOCK_OP_RST` packets
drop matching retained connections without queuing guest-visible RX output;
partial guest `VSOCK_OP_SHUTDOWN` packets record receive/send closure state and
apply TX shutdown control before same-window RX host-payload delivery, while
full guest shutdown drains pending writes before cleanup. Clean host-stream EOF
drains queued payloads, queues a guest-visible shutdown, and arms a two-second
terminal deadline; incomplete host requests use a two-second deadline, and
terminal stream failures still queue guest-visible resets. Host- and
guest-initiated tables each retain at most 256 connections.

This is an **implemented supported live MMIO-or-PCI startup/Unix-socket subset**.
`EVENT_IDX` suppresses RX/TX notifications when negotiated, indirect descriptors
are a supported bangbang extension, and the event queue otherwise accepts no-op
notifications. Signed executable validation incrementally verifies at least
1 MiB in each direction for both guest- and host-initiated streams, explicit
write-half-close/EOF and terminal cleanup, path/payload-redacted failure output,
and independent two-stream exchanges. Repeatable pre-boot `PUT /vsock` replaces
stored configuration; post-start PUT is stably rejected. PATCH, DELETE, runtime
hotplug, broader CID routing, general performance/Firecracker artifact parity,
and full event payload dispatch remain outside the live subset. Native-v1
snapshot UDS override, event-queue `TRANSPORT_RESET`, and post-restore RX gating
remain the precise #543 exclusions, so this classification does not imply
snapshot compatibility.
`SendCtrlAltDel` is rejected at parse time for the first aarch64 target while
still contributing to the `/actions` request count metric.

Future implementation PRs should derive unit or golden tests from these tables.
User documentation should keep the same support and field-status vocabulary when
API behavior ships. Security review must cover host paths, socket-like fields,
device identifiers, and error messages. Performance review must cover boot path
setup, memory size, and block device I/O when those surfaces are implemented.

## Internal Virtio-Balloon Foundation

The runtime crate can derive a backend-neutral virtio-balloon prepared model
from stored `PUT /balloon` configuration. The model includes virtio device ID
5, `VIRTIO_F_VERSION_1`, optional deflate/statistics/hinting/reporting feature
bits, compacted inflate/deflate/statistics/hinting/reporting queue metadata with 256
descriptors per queue, and Firecracker's 12-byte config space:
`num_pages`, `actual_pages`, and `free_page_hint_cmd_id`.
Validated API and config-file configuration can enable the reporting feature and
queue.

Startup can attach this model through the selected virtio-MMIO/FDT or modern
PCI transport with the configured identity, feature, queue, and config-space
registers. Guest config-space writes
update only the local device register state. Runtime hinting start and stop
commands update only host-owned command state, mirror it into the active
config-space command field, and raise a config interrupt. The hinting status API
reports that host command state plus the latest 4-byte guest command observed on
the hinting queue; `guest_cmd` remains `null` until the guest sends a command
descriptor. The backend-neutral inflate queue dispatcher
reads bounded PFN descriptor payloads, compacts them into page ranges,
publishes zero-length used-ring entries, and passes completed ranges to the
guest-memory discard owner; the deflate queue dispatcher
also reads bounded PFN descriptor payloads, compacts them into page ranges, and
publishes zero-length used-ring entries. The hinting queue dispatcher records
the latest 4-byte command descriptor as `guest_cmd`, validates and records
active-run range descriptors in dispatch state, and publishes zero-length
used-ring entries. Accepted current-command hint ranges use the same discard
owner; stale, missing-command, STOP, and DONE ranges remain ignored. Reporting
queue dispatch accepts only device-writable descriptor buffers, validates each
non-empty address range with checked arithmetic, sends valid mapped ranges to
the discard owner, and treats malformed or unserviceable descriptors as failed
best-effort attempts without blocking later chains. A reporting descriptor is
published used only after its discard attempt returns; a publication failure
therefore retains the completed discard outcome without claiming completion.
Boot runtime resources and the HVF boot loop can drain pending balloon
inflate/deflate/hinting/reporting notifications and signal the allocated balloon
interrupt line when the runtime dispatch summary reports queue-interrupt intent.
Completed inflate/deflate descriptors update internal inflated-page accounting
on the owning balloon device after PFN ranges are validated against mapped guest
memory, and reset clears that accounting. On Darwin, completed inflate and
accepted hint ranges are validated in full, segmented by owned mmap, aligned
inward to host-page interiors, zeroed with `MADV_ZERO`, and then marked clean and
reclaimable with `MADV_FREE`. Partial host-page edges are skipped so a 4-KiB
guest range cannot alter a neighboring guest page inside one 16-KiB host page.
Advice failures are best effort and do not rewrite queue completion or balloon
accounting; unsupported targets report failed attempts rather than simulated
success. Compact paired inflate/deflate accounting is prepared before used-ring
publication, reset with the device, and retained in detached capture state. It
makes no synchronous RSS or footprint guarantee.
Runtime `PATCH /balloon` updates the active `num_pages` config-space value,
increments config generation, and raises a config interrupt. The
`GET /balloon/statistics` endpoint returns required target fields from the
current stored target and required actual fields from the internal inflated-page
accounting, and it includes optional guest-reported fields after bounded
statistics queue reports record them.
Runtime `PATCH /balloon/statistics` updates stored and active nonzero polling
intervals while rejecting runtime statistics enable/disable transitions.
Process-level API and no-api runtime loops use that interval to complete a
pending statistics descriptor and mark queue-interrupt intent while the VM is
running. Linux can notify the statistics queue after committing `FEATURES_OK`
and before `DRIVER_OK`; virtio-MMIO admits that healthy transition, while the
balloon handler preserves the pending notification until activation before
dispatching it. Metrics report separate inflate/hint/report discard attempts, actual
advised bytes, skipped-edge bytes, failures, and reporting-requested bytes.
The live paired accounting and capture-ready state are implemented; byte
encoding, restore construction, artifact portability, and synchronous footprint
guarantees remain deferred.

## Internal Virtio-Block Request And Queue Dispatch

The runtime crate can parse internal virtio-block request descriptor chains from
guest memory for future device handlers. It reads the 16-byte header, classifies
`IN`, `OUT`, `FLUSH`, `GET_ID`, and unsupported request types, validates the
required data/status descriptor direction and length rules, checks 512-byte
sector alignment and capacity bounds for `IN`/`OUT`, and checks the 20-byte
minimum `GET_ID` buffer.

The runtime crate can also execute one already-parsed request against
`GuestMemory` and `BlockFileBacking`. `IN` reads from the host backing into
guest memory, `OUT` writes guest memory into the host backing, `FLUSH` syncs the
host backing, `GET_ID` writes a fixed 20-byte device ID, and unsupported request
types write the virtio unsupported status. Completion metadata records the head
descriptor index and the bytes written to guest memory, including the status
byte when status writing succeeds.

For a file-backed drive, the device ID matches Firecracker v1.16.0: it is the
decimal concatenation of `st_dev`, `st_rdev`, and `st_ino` from the metadata of
the same opened backing descriptor, truncated or NUL-padded to 20 bytes.
Successful path or same-ID backing replacement publishes the replacement ID
with the replacement backing; limiter-only updates retain the current ID.
Contained startup and replacement therefore derive identity from the
launcher-opened file rather than from a later pathname lookup. Native-v1 keeps
its fixed layout: new captures persist this metadata-derived value, while load
accepts and preserves either that exact value or the legacy drive-ID-derived
value written by earlier bangbang artifacts. Any third value fails validation.

The runtime crate can drain an internal virtio-block queue by popping available
descriptor chains, parsing and executing each request, publishing used-ring
completion elements, and returning queue-interrupt intent when at least one
completion is published. Parse failures after a valid descriptor head publish a
zero-length used element, matching Firecracker's discard shape for malformed
block requests.
The queue can be built from a ready `VirtioMmioQueueState` by reusing the
guest-selected descriptor table, driver ring, device ring, and queue size. The
builder rejects not-ready queues and wraps invalid ring metadata before guest
memory is touched.

The runtime crate also has an internal virtio-block device state that owns the
host-file backing, fixed 20-byte device ID, and optional active queue. It can
activate queue 0 from a `DRIVER_OK` virtio-mmio activation snapshot, reject
duplicate activation without replacing the existing queue, and clear the active
queue on virtio-mmio reset.

The composed runtime handler can drain recorded virtio-mmio queue notifications
and dispatch queue 0 through the active internal block queue. The dispatch
summary preserves the drained notification list, reports queue-interrupt
intent, marks the virtio-mmio queue interrupt status bit when completed work
needs an interrupt, and surfaces queue-dispatch errors with partial completion
metadata.
Boot runtime resources can also dispatch pending notifications for registered
boot block devices by using the same MMIO handler instances that guest register
writes mutated, while returning drive, region, and interrupt-line metadata for
backend interrupt signaling.

This is not runtime public `/drives` behavior and does not wire block queue
notifications into public continuous HVF runner loops or support indirect
descriptors yet. Startup preparation can register initial block MMIO devices,
and internal HVF boot sessions can consume this dispatch metadata and signal
needed block SPI interrupts.

## Guest Memory Address Space

The runtime crate models the backend-neutral guest physical address space used
by later allocation, HVF mapping, boot, and device work. The current model
contains guest physical addresses, checked RAM ranges, ordered non-overlapping
layouts, the first aarch64 DRAM layout and boot placement helpers, safe byte
slice access by guest address, and owned selectable host memory allocation for
validated page-aligned layouts.

The aarch64 layout helper follows Firecracker's `v1.16.0` ARM layout shape:

- guest RAM starts at `0x8000_0000` (2 GiB)
- the architectural DRAM maximum is 1022 GiB
- RAM crossing the 256-512 GiB MMIO64 gap is split around that gap
- zero requested memory is rejected by the layout helper
- requests above the architectural maximum are capped inside the defensive
  layout helper, while public machine configuration rejects them before storage
  so every successful configured size equals the realized layout

The default allocation model creates one anonymous read/write private host
mapping for each validated guest RAM range. An internal startup resource can
instead select descriptor-backed shared RAM before allocation. That profile
preflights the largest retained object against `RLIMIT_FSIZE`, accounts every
retained descriptor against `RLIMIT_NOFILE`, creates exact-sized sparse
owner-only files, unlinks each name before publication, and maps them
`MAP_SHARED`. A bounded export clones only checked descriptor, offset, and
length metadata; debug and errors do not expose a pathname, descriptor number,
or host address. When virtio-mem is configured, startup selects this profile
even without an initial vhost device and reserves the complete deterministic,
pmem-aware aperture as one additional shared object. The reservation does not
enter the active region list, current total, byte access, dirty metadata, FDT,
or initial HVF mappings. Plugged blocks are exact offset views that retain the
reservation owner. All mappings and descriptors close with runtime ownership
cleanup.

Both profiles use the same HVF map/protect/unmap, dirty bitmap, byte access,
balloon, virtio-mem, and native snapshot streaming paths without a copy shadow.
Darwin anonymous discard retains `MADV_ZERO` followed by `MADV_FREE`; shared
file mappings use `F_PUNCHHOLE`, which tests require to produce immediate zero
reads while deallocating the range. The native image loader has an explicit
internal shared-profile entry point, while public native-v1 restore remains
anonymous. macOS provides no `memfd` sealing equivalent, so an external
recipient of a writable descriptor is an explicit trusted capability boundary.
A separate `bangbang-vhost-user` crate implements the closed Firecracker v1.16
block frontend request set over an already connected Unix stream:
owner/feature negotiation, CONFIG and optional REPLY_ACK, exact memory-table
and vring setup, native-endian bounded framing, first-header-byte SCM_RIGHTS,
absolute deadlines, terminal synchronization failure, and directional
eight-byte nonblocking pipe notifications with Darwin kqueue evidence. Direct
startup owns the path connector and discovers every configured backend before
VM construction. Guest activation transfers one guest-address-ordered
immutable table containing boot RAM plus, when configured, the complete
virtio-mem aperture, followed by one validated queue. The arm64 topology has at
most three regions and contains no unrelated mapping. Online virtio-mem blocks
remain the only aperture ranges mapped into HVF or admitted to current/dirty
accounting, but the backend can read or write offline bytes through the full
reservation. Grow/shrink never sends a second vhost memory table; exact
best-effort shared discard uses each view's file offset after mutation commit.
Backend calls use the same GIC/MSI interrupt abstraction as file-backed block.
The selected backend is trusted with initial RAM plus the configured maximum
aperture; the strict regular-file peer used by tests is not a shipped storage
backend, and backend policy/jailing remains operator-owned. Ordinary-only VMs
remain anonymous, both pre-boot configuration orders are accepted, eligible
dynamic-memory PCI VMs support runtime insertion, and native-v1 capture still
rejects vhost before artifact staging. Direct pmem remains a separate
classified host mapping. The runtime does not use Firecracker's `vm-memory` or
`vhost` crates.

Guest memory byte access validates the whole requested guest address range
before copying. Overflow, unmapped holes, and the aarch64 MMIO64 gap fail
without partial copies, while zero-length reads and writes are no-ops. This
gives later boot-loading code a safe runtime-owned path for copying kernel,
initrd, command line, and FDT bytes without exposing additional raw host-memory
pointers.

The first arm64 placement helpers match Firecracker's published aarch64 layout
shape: system memory occupies the first 2 MiB of DRAM, the kernel load address
starts at `0x8020_0000`, the command-line size constant is 2048 bytes, the FDT
window is 2 MiB at the end of the first DRAM range when there is room, and
initrd placement is page-aligned immediately before the FDT window when it fits.
A zero-byte initrd resolves to the FDT address, matching Firecracker's helper
behavior.

## Boot Source and Payload Loading

The public `PUT /boot-source` API accepts a strict Firecracker-shaped model with
a required kernel image path, optional initrd path, and optional boot arguments.
The API converts it into the runtime-owned configuration transaction; successful
pre-boot replacement is reported through `GET /vm/config`, and public
`InstanceStart` consumes that exact retained state. Direct mode opens the
configured payloads during startup. Contained mode consumes the matching
launcher-authorized descriptors without reopening submitted paths. A validation,
open, load, placement, FDT, CPU/cache admission, or later construction failure
does not publish a partial VM session.

When boot arguments are omitted, the runtime uses Firecracker's default aarch64
kernel command line. Custom boot arguments follow Firecracker's `linux-loader`
command-line parsing shape: leading and trailing boot/init-argument whitespace
is trimmed, the first unquoted ` -- ` separates init args, and the normalized
bytes must fit in the 2048-byte aarch64 command-line capacity including the
trailing NUL byte. Embedded NUL bytes and init args without boot args are
rejected. The validated command-line text is published by the startup FDT
builder as `chosen.bootargs`; signed public executable tests exercise custom
initrd and direct-rootfs command lines through the API.

The internal loader supports the arm64 Linux `Image` header shape used by
Firecracker's aarch64 boot path. It validates the Image magic, text offset, and
legacy zero-size image behavior, then copies the complete kernel file into
guest memory at `kernel_load_address + text_offset`. The kernel range must be
fully backed by guest memory and must not overlap the reserved FDT address.

An explicitly configured initrd must be a non-empty regular file. It is placed
with the aarch64 initrd helper immediately before the FDT reservation, must be
fully backed by guest memory, and must not overlap the loaded kernel range.
Host path and file errors stay structured and public failures redact submitted
paths.

The loader intentionally uses bangbang's safe `GuestMemory::write_slice` API and
does not expose new raw host-memory pointers. Direct `linux-loader`/`vm-memory`
integration is deferred until the project decides whether to add a narrow
adapter or adopt `vm-memory` more broadly.

## Internal Drive Configuration

The API crate has strict Firecracker-shaped `PUT /drives/{drive_id}` and
`PATCH /drives/{drive_id}` request parser and body models. Initial drive
configuration accepts the documented drive fields, rejects unknown fields,
rejects malformed or incomplete JSON bodies, rejects extra path segments, and
rejects path/body `drive_id` mismatches without echoing host paths. The initial
parser treats a body with neither `path_on_host` nor `socket` as incomplete, but
accepts either a file-backed `path_on_host` body or the strict socket-backed
vhost-user matrix. Drive update
requests parse the Firecracker-shaped `drive_id`, `path_on_host`, and
`rate_limiter` fields, reject invalid or mismatched bodies, and route valid
runtime updates to the process-owned block-device refresh path. The running API server
converts parsed initial drive requests into a VMM action; valid pre-boot
requests are recorded as VM configuration state and return `204 No Content`.
Replacing an existing pre-boot drive ID preserves its Firecracker-shaped device
ordering slot, while newly configured root drives are still kept first.

Firecracker v1.16.0's developer-preview runtime drive PUT and DELETE behavior
requires `--enable-pci` and PCI transport. The operator must rescan the guest PCI
bus after attach and remove the guest PCI device before host DELETE because the
feature has no automatic guest notification. bangbang supports transactional
file-backed non-root PUT and bodyless DELETE on the retained all-virtio PCI bus
in Running or Paused state. A same-ID file PUT can replace backing, Sync/Async
engine, and exact limiter while preserving immutable identity fields; the old
generation quiesces before public configuration commits. Existing drive
backing/rate-limiter PATCH operates through the selected MMIO or PCI startup
handle. A direct or contained vhost drive admits an
ID-only PATCH that refetches the active backend's exact 60-byte config before
advancing one transport generation and delivering one configuration interrupt.
An all-PCI VM whose immutable live memory profile is already shared may attach
a new non-root socket drive after owner-side profile/capacity preflight and may
DELETE it after caller-coordinated guest removal. Public configuration commits
only after owner publication; removal releases the frontend, notifier and
shared-memory descriptors, metrics generation, BAR, MSI-X, dispatcher region,
and PCI function. Root, duplicate, anonymous-memory, unavailable-session, and
capacity failures precede socket connection. Same-ID runtime PUT remains a
duplicate only for vhost-user, matching pinned Firecracker v1.16 rather than
inventing reconnect.

The runtime crate has an internal, Firecracker-shaped drive configuration model
for the initial virtio-block subset. It validates path and body `drive_id`
values as nonempty alphanumeric strings with `_`, requires the two IDs to
match, and uses an exhaustive file/vhost backend enum. File-backed input
requires one nonempty `path_on_host`, normalizes omitted `is_read_only` to
read-write, accepts Sync or Async plus optional rate limits, and rejects a socket.
Vhost-user input requires one nonempty socket, accepts root/partuuid/cache, and
rejects `path_on_host`, explicit `is_read_only`, `io_engine`, or rate limiting.
Invalid mixed shapes fail before host access or stored-state mutation.

Both backends accept omitted/default `cache_type=Unsafe` or explicit
`cache_type=Writeback`. For vhost, startup connects under one bounded deadline,
requires VERSION_1 plus vhost protocol negotiation and CONFIG, intersects the
reviewed block/ring features with the backend, and preserves the exact 60-byte
config. `GET /vm/config` emits only the selected backend fields. Displayed
errors and debug output avoid echoing paths, sockets, descriptors, guest
addresses, or backend payloads.

The runtime crate opens the normalized `path_on_host` as either a regular host
file or, on macOS, one exact block-special descriptor. It preserves configured
read-only mode and performs bounded positioned reads/writes and flushes for
internal virtio-block request execution. Regular capacity comes from metadata;
block capacity comes only from checked logical-block-size/count ioctls and all
requests remain sector and bounds checked. Direct Writeback flush uses
`DKIOCSYNCHRONIZECACHE` because ordinary `fsync` is not a valid substitute for
the supported macOS block descriptor. Directories, FIFOs, sockets, character
devices, symlinks, write-only descriptors, wrong access, invalid geometry, and
all other object kinds fail before data I/O; read-only writes fail before
mutation. Backing errors avoid echoing `path_on_host`, descriptor identity, or
geometry. Public startup opens configured backing paths during `InstanceStart`,
and runtime drive updates prepare replacement backings before mutating the
active virtio-block handler or stored VMM configuration.

Contained BBG2 drive grants bind regular-versus-block kind, exact
device/inode/rdev, access and normalized status flags, block size/count/capacity,
and the transferred descriptor. The worker independently validates that tuple.
App Sandbox allows real pread/pwrite but returns `EPERM` for the public disk
geometry/cache ioctls, so descriptor 7 carries only fixed 256-byte `BBC1`
`Inspect` and `SynchronizeCache` exchanges to the launcher's retained exact
grant descriptor. The facet is lifecycle-session, monotonic-sequence, grant,
role, access, identity, and geometry bound, uses a two-second worker deadline,
accepts no rights in responses, and poisons on ambiguity. The launcher re-fstats
and rechecks access/status for every operation and has no pathname, enumeration,
physical-media, or generic-ioctl service.

Sync executes positioned file operations on the owner thread. Async lazily
creates one bounded portable host executor for the VM session, preflights task
and staging capacity before consuming limiter tokens, and submits immutable
generation-bound work without publishing a used entry. The owner watches one
completion descriptor alongside device and API wakeups; completion applies
status and partial-byte semantics, copies read data, marks guest dirty ranges,
publishes the used ring, raises the selected SPI or MSI-X interrupt, and updates
per-drive latency/byte/failure/pressure metrics. A central generation registry
routes multiple devices without consuming another drive's ready work and
re-arms readiness when a later device becomes publishable in the same monitor
pass. Reset, path PATCH, same-ID replacement, DELETE, rollback, and shutdown
quiesce or discard only the selected generation while releasing global task and
buffer leases. This preserves Firecracker's observable Sync/Async choice on
macOS without claiming Linux io_uring or timerfd/eventfd identity.

When any startup drive uses vhost-user, startup selects shared guest RAM before
allocation, carries every connected frontend as a move-only resource, and
activates the existing concrete block device over MMIO or PCI. Virtio-mem also
selects shared RAM and reserves its complete aperture before block preparation,
so either configuration order and a later eligible PCI insertion use the same
topology. Only after guest feature and full ring-range validation does the
frontend send the final feature mask, the immutable boot-RAM-plus-aperture
table, vring state, call/kick descriptors, and queue enable. Offline aperture
bytes remain outside guest CPU/HVF/current accounting but inside the trusted
backend's writable authority. Plug/unplug changes active views, dirty metadata,
HVF mappings, and exact shared discard without changing the table. Backend
calls wake a blocked vCPU and raise the normal queue interrupt; backend closure
terminalizes only that device path, records a per-drive event failure, and
leaves the process API responsive. Native-v1 capture rejects vhost before
artifact staging. Runtime insertion reuses this constructor only after an
already-shared live owner preflights all deterministic publication capacity; it
never converts anonymous RAM or copies a shadow. DELETE/backend death releases
frontend clones without removing the VM-owned aperture. Active ID-only refresh
polls repeated CONFIG requests, publishes only a complete validated reply, and
records optional `config_change_time_us` after successful guest notification.

Contained mode additionally recognizes only an exact private grant tag during
successful drive `PUT`/path-changing live `PATCH`. It binds exact ID,
`DriveBacking` role, and access derived from the immutable read-only mode,
constructs the same backing from the transferred file, and never reopens the
tag. Pre-boot same-ID replacement and active backing refresh keep public state
failure-atomic; startup consumes prepared backings through an exact-ID move-only
bundle. Live replacement retains a rollback claim until the owner transition
succeeds, so a rejected transition preserves both the old live drive and grant
authority. Limiter-only updates claim nothing and retain the current backing.
Direct mode continues treating the tag bytes as a path.
Contained socket-backed requests instead require an exact
`bangbang-grant:<GrantId>/<SocketChild>` reference. A repeatable connect-only
directory grant is adopted by ID and retained for the session; each drive owns
only one child lease. The dedicated descriptor-6 `BBU1` broker asks the launcher
for a connected stream bound to exact lifecycle session, sequence, grant, and
child values. The launcher connects relative to its retained anchor after
no-symlink/current-user/socket/single-link checks, revalidates the target and
peer, restores its cwd, and returns either exactly one stream or a stable
redacted retryable failure. Startup preflights all contained dependencies and
broker health before the first request. Runtime owner/capacity preflight occurs
before child reservation or broker I/O; failed preparation restores a fresh
lease, ID-only PATCH reuses the existing stream, duplicate PUT makes no broker
request, and DELETE releases only the drive lease so the retained directory can
serve later same-ID reinsertion. There is no ambient connect or steady-state
helper.

Virtio-block feature negotiation follows the selected cache mode:
`cache_type=Unsafe` keeps the flush feature hidden, while
`cache_type=Writeback` advertises `VIRTIO_BLK_F_FLUSH` and uses the existing
backing-file flush path for guest flush requests. A vhost backend supplies its
read-only capability and maximum reviewed feature set; Unsafe excludes FLUSH
from the requested intersection, while Writeback permits it when offered.

Runtime PATCH can replace an existing file-backed drive's host file and update
its per-device rate limiter without changing its Sync/Async engine. Same-ID PUT
can atomically replace the file backing, engine, and exact limiter while
preserving root/read-only/partuuid/cache identity. Public PCI additionally supports transactional
Running/Paused insertion and removal of non-root file-backed drives after
manual guest rescan/removal. A successful backing refresh updates the matching
MMIO or PCI handler's backing, engine generation, config space, config
generation, and config interrupt status after old Async work quiesces. A
limiter-only update does not reopen the backing or raise a config interrupt.
Failures preserve the previous backing, engine, limiter, grant authority, and
stored config. Vhost path/limiter/engine mutation remains rejected before
connection or device mutation.

## Internal Network Interface Configuration

The API, runtime, process, and HVF crates implement the supported
virtio-MMIO/MMDS-only network subset from Firecracker-shaped pre-boot
configuration through guest-visible packet handling. The API parser accepts
`PUT /network-interfaces/{iface_id}`, rejects path/body ID mismatches and
unknown fields, and forwards the supported request shape through the VMM action
boundary. The runtime validates path and body `iface_id` values as
nonempty alphanumeric strings with `_`, requires the two IDs to match, requires
a nonempty `host_dev_name`, accepts optional `guest_mac` values only when they
are six colon-separated two-hex-digit octets, replaces existing entries with
the same `iface_id`, and rejects duplicate configured guest MAC addresses across
different interface IDs. Displayed validation errors avoid echoing invalid IDs,
host device names, and MAC strings.

The internal model accepts configured `mtu` values in the Firecracker-compatible
`68..=65535` range, preserves them in stored configs and `GET /vm/config`,
and exposes them to the guest through `VIRTIO_NET_F_MTU`. A signed executable
MMDS-only case configures `1280`, requires the Linux interface to report that
value, and then completes the guest MMDS fetch through the same device. Initial
Firecracker-shaped `rx_rate_limiter` and `tx_rate_limiter` bandwidth/ops
buckets are retained in stored configs and `GET /vm/config`. Zero-sized,
zero-refill, and overflowing-refill buckets are explicit disabled controls and
normalize away. Each prepared device owns independent RX and TX live budgets;
queue admission atomically consumes one op plus complete guest-visible frame
bytes before TX publication/sink work or RX guest writes/publication/source
consumption. A valid frame larger than byte capacity can progress once from a
full bucket. Throttled work remains pending for a later explicit dispatch, and
successful MMDS TX detours restore their exact admission reservation while
forwarded frames and failures remain charged. Runtime PATCH can update
individual RX/TX bandwidth and ops buckets while preserving omitted live
budgets, queue state, and pending-work flags; explicit disabled buckets clear
only their target. Pending limiter work exposes its earliest retry duration
through queue, device, runtime, and HVF dispatch results. Each active network
session owns a coalesced deadline scheduler that requests a normal coordinator
wakeup and redispatches retained work on the owning thread; terminal paths
cancel pending publication. Limiter-specific metrics, snapshot state, and
direct vmnet rate-limit evidence remain deferred. bangbang currently limits
stored network interfaces to 16.
Firecracker `v1.16.0` does not publish a separate network-interface count
limit. The bangbang value is a generic scaffold cap, not enforcement of
Apple's separate vmnet provisioning limits.
Configuration storage does not open host networking resources or change host
vmnet MTU settings. Stored network interface configs are returned from
`GET /vm/config` in the `network-interfaces` array. During `InstanceStart`, the
process crate revalidates the count before selecting packet I/O. If every
configured interface is listed in MMDS config, startup still validates
`host_dev_name` syntax against the supported vmnet forms but can use
process-local MMDS-only packet I/O without opening vmnet resources. Otherwise,
it maps `host_dev_name` values `vmnet:host`, `vmnet:shared`, and
`vmnet:bridged:<interface>` to vmnet host, shared, and bridged configurations,
with the bridged suffix required to be nonempty and free of NUL bytes and ASCII
control characters. Startup does not verify that the named macOS interface
exists before building cleanup-owning packet I/O for each configured interface.
Other nonempty names are still accepted before boot but fail startup before
`Running` is committed.

With public PCI enabled, post-start PUT validates and reserves a new interface
without replacing an existing ID, rejects a duplicate guest MAC, and submits
one transaction to the boot-run-loop owner. The process packet-I/O registry
keeps each existing entry and its queue/MMDS/vmnet state intact. Its immutable
startup policy selects a later entry: an initially mixed session continues to
use vmnet for all additions, while an initially empty or all-MMDS session can
use MMDS-only packet I/O for an ID listed in immutable pre-boot MMDS config and
uses vmnet otherwise. Contained MMDS-only entries need no vmnet authority;
contained vmnet entries must match the exact mode/bridge and fit the count of
actual live vmnet owners. Packet I/O publishes immediately before the PCI
endpoint and live config commits last.

Bodyless network DELETE requires the operator to remove the PCI function in the
guest first. The owner transaction prepares reversible endpoint teardown,
takes and explicitly stops the exact packet-I/O entry, and then commits PCI
teardown. Success releases queue, callback/event, limiter retry, generation-safe
metrics, MMDS detour or vmnet handle, slot/BAR/MSI-X/dispatcher, and live-config
state. Recoverable preparation failures restore prior reachability; uncertain
system vmnet stop, failed restoration, or failure after the irreversible commit
boundary makes the worker terminal. Snapshot quiescence and shutdown close
ordinary mutation admission, and native-v1 continues to reject every PCI
profile before artifact mutation.

A signed two-interface MMDS-only case configures distinct API IDs and guest
MACs, selects both IDs in MMDS config, finds the Linux devices by MAC, and binds
one bounded request to each device through a replaced `/32` MMDS route. The two
results occupy separate fixed data-drive sectors, and both `net_<iface_id>`
metric objects report RX and TX activity. Focused tests prove the matching
detours retain independent split-request buffers and response queues while the
shared data store and top-level `mmds` metrics remain process-local aggregates;
second-interface packet I/O also retains its own interrupt line and network
metric key. The case completes without opening vmnet resources or using the
restricted direct-network entitlement.
A signed two-process MMDS-only case gives each executable unique API sockets,
interface IDs, V2 data and token authority, packet/session state, metrics, and
scratch drives. A process-local release gate keeps the surviving guest pending
while its peer exits; after teardown, the survivor uses the same token to
re-fetch its retained value and publish a terminal marker. File-byte and metric
key assertions detect cross-process writers, peer socket cleanup cannot remove
the survivor, and failure diagnostics omit tokens, values, guest bytes, private
paths, and raw worker output.

Separate signed direct-executable and normal production-bundle cases remove one
startup MMDS-selected PCI function and then complete two runtime rounds with the
same ID and MAC. Each round performs host PUT, guest rescan, modern virtio-net
identity and BDF check, real MMDS curl exchange, guest sysfs removal, and host
DELETE; the second PUT occurs while Paused. Both require exact slot reuse and
clean shutdown. The production worker keeps the exact networkless
App-Sandbox/Hypervisor signature, consumes no vmnet authority, and additionally
rejects a non-MMDS bridged insertion without changing live config. This is
packet-path and transaction evidence, not an external vmnet connectivity claim.

Direct vmnet remains a separate conditional foundation. Apple's current
[vmnet documentation](https://developer.apple.com/documentation/vmnet)
describes returned guest MAC/MTU values and limits of 32 interfaces overall,
four per guest operating system, and bounded read/write batches. The current
bangbang start callback discards vmnet's MAC, MTU, and maximum-packet-size
parameters; the FFI does not register the packet-available callback, so it has
no asynchronous RX-readiness integration. It does retain synchronous
single-packet adapters, injected start/stop/read/write tests, and stop-on-drop
cleanup. No signed guest test uses Apple's restricted
[`com.apple.vm.networking`](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.vm.networking)
authorization or proves external packet movement, and the 16-interface config
cap does not enforce Apple's per-guest resource policy.

The operator-owned live vmnet host policy boundary is documented in
[`docs/security.md`](security.md#vmnet-host-policy-boundary).

## Internal Vsock Configuration

The API and runtime crates implement pre-boot, Firecracker-shaped vsock
configuration storage and internal virtio-vsock device work. The API parser accepts
`PUT /vsock`, rejects unknown fields, and forwards the supported request shape
through the VMM action boundary. The runtime requires `guest_cid >= 3`, accepts
the deprecated optional `vsock_id` when it is nonempty and contains no control
characters, and requires a nonempty `uds_path` with no control characters.
Displayed validation errors avoid echoing configured socket paths. Contained
mode reserves the exact `bangbang-grant:<GrantId>/<SocketChild>` form; the
child is one 1–64 byte ASCII `[A-Za-z0-9._-]` component other than `.` or `..`,
while direct mode treats identical bytes as an ordinary path.

Stored vsock configuration replaces any previous pre-boot vsock configuration
and is returned from `GET /vm/config` as `vsock` with `guest_cid` and
`uds_path`; the deprecated input-only `vsock_id` is omitted. The configuration
request itself does not create the configured socket. Direct mode leaves the
path unopened until startup. Contained mode claims and retains the exact
singleton directory scope/anchor during the successful request, with complete
validation before one-time consumption and failure-atomic replacement. Startup
later binds the direct nonblocking listener or exclusively publishes and
supplies the granted listener without reopening the reference.

The runtime crate has an internal virtio-vsock prepared resource, MMIO
registration helper, config-space, 44-byte little-endian packet header model,
guest-readable TX descriptor packet parser, TX available-ring drain helper with
used-ring descriptor completion,
MMIO handler skeleton with active queue metadata retention, RX/TX notification
dispatch, no-op event notification handling, and startup FDT attachment. It uses the
virtio device id `19`, three 256-entry RX, TX, and event queues, Firecracker's
`VERSION_1`, `IN_ORDER`, and `EVENT_IDX` feature bits, and a guest-CID config
field that supports Firecracker-shaped 8-byte and 4-byte-half reads. Config
writes are rejected. The packet header model preserves Firecracker's header
field order and rejects header byte parsing when payload length exceeds
Firecracker's 64 KiB maximum packet buffer length. The TX packet parser reads
headers across descriptor boundaries, validates readable descriptor ranges, and
returns payload segment metadata trimmed to the advertised payload length. The
TX drain helper consumes available TX descriptor chains from the active queue
into parsed packet metadata while preserving queue progress and publishes
zero-length used-ring completions for consumed descriptor heads. When
`EVENT_IDX` is negotiated, RX and TX dispatch use the available-ring
`used_event` value to decide whether completed descriptors require a queue
interrupt. The handler can
drain RX, TX, and no-op event queue notifications, dispatch the active RX queue
for pending host request headers, dispatch the active TX queue, preserve
completed RX/TX dispatch metadata on errors, and mark the virtio queue
interrupt status when completed descriptors require guest notification. Boot
runtime resources can dispatch the registered vsock MMIO handler's RX/TX
notifications plus no-op event notifications, and internal HVF boot sessions can
signal the allocated vsock SPI line from those dispatch summaries. The prepared
resource preserves the validated guest CID, socket path, optional supplied
listener/guest connector, config-space, and inactive device state. Arm64 startup
resource assembly can bind a direct nonblocking listener at `uds_path` or consume
the already published granted listener and fixed launcher connector, retain that
ownership in the internal vsock device resource, and expose one configured vsock device in
the guest FDT. The runtime can parse Firecracker-shaped host `CONNECT <PORT>`
requests into a guest port, allocate Firecracker-shaped host local ports,
retain host-initiated accepted streams in an internal table, expose a one-shot
guest-facing `VSOCK_OP_REQUEST` packet header for a retained host connection,
dispatch that pending request header into writable guest RX descriptors with
used-ring completion metadata, accept one pending host connection per dispatch
pass into an owned nonblocking stream, retain bounded accepted streams across
partial handshakes and retained connection records, drop invalid
accepted-stream handshakes without path exposure, retry RX delivery when
pending host requests exist without requiring a fresh guest RX notification,
and acknowledge guest `VSOCK_OP_RESPONSE` packets for delivered host requests
by writing `OK <local_port>\n` to the retained host stream. The runtime can
also queue bounded zero-payload `VSOCK_OP_RST` headers for unsupported or orphan
host-destined guest TX packets and deliver those reset headers through the
existing RX queue path. Supported guest `VSOCK_OP_REQUEST` packets can attempt
nonblocking connects to Firecracker-shaped `${uds_path}_${PORT}` sockets in
direct mode or ask the fixed contained connector for the same validated port,
retain successful guest-initiated streams, and deliver guest-visible
`VSOCK_OP_RESPONSE` headers; established host-initiated or guest-initiated
connections can forward bounded `VSOCK_OP_RW` payload bytes to the retained
host stream, keep a bounded four-packet per-connection guest-to-host retry queue
for partial or would-block nonblocking writes, and retry pending bytes on later
notification dispatch before accepting more guest `RW` data for the same
connection;
established host-initiated and guest-initiated connections can retain a bounded
four-packet per-connection backlog of host `VSOCK_OP_RW` payloads and deliver
one queued payload at a time into guest RX buffers. Dynamic 64-KiB credit windows
use wrapping received/forwarded/sent counters, bounded reservations, and
guest-visible credit requests/updates to resume exhausted directions. Guest
`VSOCK_OP_RST` packets drop matching retained connections without queuing
guest-visible RX output; partial guest `VSOCK_OP_SHUTDOWN` packets record
receive/send closure state and apply TX shutdown control before same-window RX
host-payload delivery, while full guest shutdown drains pending writes before
cleanup. Clean host EOF queues a shutdown after queued payloads drain; request
and shutdown cleanup each have two-second deadlines, terminal failures still
queue resets, and each initiation direction retains at most 256 connections.

The resulting **implemented supported live MMIO-or-PCI startup/Unix-socket subset** uses
`EVENT_IDX`; indirect descriptors are a supported bangbang extension, while
event queue notifications otherwise remain no-op metadata. Signed executable
cases verify ≥1-MiB deterministic bidirectional streams, both sides'
write-half-close/EOF sequence, terminal cleanup, redacted diagnostics, and
two-stream isolation for both initiation paths. Repeatable pre-boot PUT replaces
configuration and post-start PUT is stably rejected; PATCH, DELETE, runtime
hotplug, broader CID routing, general performance/artifact parity, and full
event payload dispatch remain excluded. Native-v1 snapshot UDS override,
event-queue `TRANSPORT_RESET`, and post-restore RX gating remain #543 exclusions.

The runtime crate also has the first backend-neutral virtio-net config-space,
activation, TX frame parser, RX buffer parser, prepared device resources, and
MMIO registration helpers. They define the
Firecracker-shaped virtio network device id, RX/TX queue indexes, two queue
sizes, the guest-MAC feature bit, a `VirtioMmioDeviceConfigHandler` for reading
a configured guest MAC through the existing virtio-mmio register handler, a
`DRIVER_OK` activation handler that validates and retains dispatchable RX and TX
queues, an internal TX frame parser for the 12-byte virtio-net header plus
guest-readable payload segments, and an internal RX buffer parser for
guest-writable receive buffer segments with the current 1526-byte Firecracker
non-merged-RX minimum. Preparation can build ordered owned resources from stored
configs, preserving `iface_id`, `host_dev_name`, guest-MAC config space, and an
inactive `VirtioNetworkDevice` without opening host networking resources. The
prepared resources can be consumed into deterministic virtio-mmio regions and
handlers in a fresh or existing internal `MmioDispatcher`, returning read-only
registration metadata. Startup preparation can pair those registrations with
caller-provided interrupt lines and write matching inert virtio-mmio descriptors
into the guest FDT while preserving interface order and host device names.
Internal network notification dispatch can drain pending TX and RX queue
notifications and can choose injected packet I/O per configured interface at the
boot-runtime boundary. The HVF boot-session wrapper can invoke that injected
path. Process-owned startup uses a no-op provider when no network interfaces are
configured, can build process-local MMDS-only packet I/O when every configured
interface is selected by MMDS config, and otherwise builds vmnet packet I/O for
configured interfaces during `InstanceStart`. TX dispatch walks the TX
available ring, parses descriptor chains into `VirtioNetworkTxFrame` metadata,
publishes used-ring
completions with length 0, delivers parsed frames to an injected internal packet
sink, preserves parse, sink, and partial-dispatch errors, and marks queue
interrupt status when descriptor heads complete unless negotiated `EVENT_IDX`
suppresses the notification. RX dispatch uses an injected
internal packet source, can perform one bounded post-TX RX retry when that
source reports already-ready packets, copies a zeroed 12-byte virtio-net header
plus packet payload into validated guest-writable RX buffers, publishes
used-ring completions with the written length, preserves malformed-buffer and
partial-dispatch metadata, and marks queue interrupt status when RX buffers
complete unless negotiated `EVENT_IDX` suppresses the notification. On macOS,
the process crate also defines internal vmnet descriptor,
lifecycle, start owner, packet descriptor, and concrete system start/stop
backend boundaries with vmnet mode, status, operation error, XPC descriptor
configuration, retained dispatch queue ownership, completion-status mapping,
backend start/stop ownership, packet `iovec` layout, single-packet system
`vmnet_read`/`vmnet_write` wrappers, count validation, owned cleanup models,
an internal cleanup-owning packet backend that can delegate read/write while
retaining vmnet stop-on-drop ownership, an internal virtio-net packet I/O
adapter that copies TX guest-memory payload segments into vmnet writes and
caches one vmnet RX packet until consumed, an MMDS-only packet I/O adapter that
detours MMDS TX frames and drops non-MMDS TX frames without opening vmnet, and
a bounded generation-aware per-interface registry with explicit vmnet stop,
exact take/restore, and independent entry publication. It also defines an
internal `host_dev_name` mapping for `vmnet:host`, `vmnet:shared`, and
`vmnet:bridged:<interface>`, where bridged interface names are nonempty and
contain no NUL bytes or ASCII control characters. Startup with configured
network interfaces revalidates the 16-interface limit before selecting packet
I/O, can use the MMDS-only adapter when every configured interface is selected
by MMDS config, and otherwise opens vmnet resources through those supported
forms and retains stop-on-drop cleanup. Startup without network interfaces
starts with an empty registry that can accept later PCI entries. These helpers
can advertise configured guest-visible MTU values and support the documented
public PCI attach/remove transaction, but they do not change host vmnet MTU
settings or prove direct vmnet host connectivity. Active HVF sessions schedule retained limiter work
through the session-owned retry wakeup described above rather than through
Linux timerfd/eventfd identities.

The runtime crate can prepare owned internal block-device resources from a
validated list of stored drive configs. Preparation opens each backing file,
derives the virtio-block config space, builds an inactive `VirtioBlockDevice`,
uses the drive ID as the fixed 20-byte virtio device ID with zero padding or
truncation, and preserves the configured drive order. If a later config fails
to prepare, the source drive configs remain unchanged and the error identifies
the drive ID without echoing `path_on_host`.

The runtime crate can also consume prepared block-device resources into a fresh
internal `MmioDispatcher`. This assigns deterministic 4 KiB virtio-mmio device
windows and region IDs from an explicit layout, registers one composed
virtio-mmio block handler per prepared device, and returns read-only
registration metadata. Invalid address strides, duplicate region-id strides,
address or region-range overflows, region-id overflows, or dispatcher
registration failures do not expose a partially registered returned bundle.

The runtime crate can derive an internal virtio-block configuration space from
the backing length. It reports capacity as full 512-byte sectors, matching
Firecracker's truncation of non-sector-aligned tails, exposes the virtio block
device id and one 256-entry queue shape, always advertises
`VIRTIO_F_VERSION_1` and `VIRTIO_RING_F_EVENT_IDX`, advertises
`VIRTIO_BLK_F_FLUSH` for `cache_type=Writeback`, and advertises
`VIRTIO_BLK_F_RO` for read-only drives.
The config handler supports bounded read-only capacity reads through the
existing virtio-mmio device-configuration path and rejects config writes.

The runtime model is wired to successful pre-boot `PUT /drives/{drive_id}` VMM
configuration storage. Public `InstanceStart` startup can call block-device
preparation, MMIO registration, and FDT device description for initial
configured drives. When a configured drive is the root device, startup appends
Firecracker-style root-drive kernel command-line arguments before writing FDT
`chosen.bootargs`: `root=PARTUUID=<partuuid>` when `partuuid` is configured,
otherwise `root=/dev/vda`, followed by `ro` for read-only drives or `rw` for
writable drives. The final command line is still checked against the arm64
2048-byte command-line limit after these automatic arguments are appended.
The internal boot run loop across bounded step windows can dispatch active
block queue notifications and signal interrupts. The signed `guest_boot`
integration target now validates that the pinned Firecracker arm64 kernel can
discover the first virtio-block device as `/dev/vda` and read a marker from a
temporary host backing file through the raw block device. The same target also
validates a basic writable-drive path by writing a marker from the guest to
`/dev/vda` and checking the scratch host backing file. It also attaches the
pinned Firecracker squashfs rootfs as a read-only root drive, mounts it from the
deterministic initrd, and reads `/mnt/etc/os-release` from the guest. Direct
root-drive boot validation is covered by a generated ext4 variant of the same
Firecracker rootfs that adds a deterministic test init, boots without an initrd,
mounts the virtio-block root drive as `/`, and reads `/etc/os-release`; the
signed `guest_boot` target validates guest-visible `root=/dev/vda ro`
command-line arguments through captured serial output, and the signed
executable HVF e2e target validates the same direct-rootfs path through public
API configuration and config-file startup plus a host-observed scratch block
marker. This does not yet claim arbitrary distro rootfs boot or default Ubuntu
systemd startup.
Both cache modes in the supported subset have explicit behavior: `Unsafe`
suppresses the flush feature, while `Writeback` advertises it and the signed
executable guest fsync scenario validates the backing flush path. Block limiter
retry is backend-neutral and active HVF sessions own their retry wakeups; this
does not claim Firecracker's exact Linux timerfd/eventfd event-source identity.
The same concrete device supports file-backed PCI hotplug and direct or
contained vhost-user execution. Signed direct cases boot an MMIO read-only root and a PCI
writable MBR/PARTUUID root, verify backend-derived config and scratch
read/write/flush, and prove redacted backend-death metrics while the API remains
responsive. A signed product-PCI lifecycle also proves live capacity refresh,
new-ID direct runtime attach, guest I/O, manual removal, Paused DELETE/PUT,
duplicate and anonymous-profile zero-connect rejection, complete closure, and
same-ID/slot reuse. Signed production-bundle cases boot an exact contained
vhost root and scratch child alongside vsock, prove scratch read/write/flush
and guest-observed CONFIG resize on the existing stream, then use an all-PCI
shared-memory guest to prove invalid-target and negotiation rollback, new-ID
runtime attach, duplicate zero-connect rejection, manual removal, DELETE,
Paused same-ID reuse through another exact child, resumed I/O, and complete
closure without a surviving helper. Separate signed file-backed cases exercise
Async MMIO startup and live path PATCH, Async config-file startup, concurrent
Async root/data drives, first-use PCI Async hotplug, DELETE/reuse, and paused
same-ID Sync-to-Async replacement. Signed production cases repeat contained
Async root/control startup, preauthorized backing/engine replacement, limiter
PATCH, and runtime hotplug/delete/reuse. The implementation uses a portable
bounded executor rather than Linux io_uring. Automatic PCI notification remains
unavailable. Four additional signed macOS block-special cases cover direct and
contained MMIO/PCI owners with complementary Sync/Async and Unsafe/Writeback
orders, read-only/read-write descriptors, live limiter retry, exact 4/6/8-MiB
capacity/config refresh, current backing-derived GET_ID, real guest
read/write/flush/readback persistence, regular-to-block, block-to-regular, and
block-to-block replacement, wrong-access and capture rollback, capture twice
without artifacts, manual removal, DELETE, same-ID/slot reuse, unchanged worker
entitlements, complete owner release, read-only reattach, and exact virtual-media
cleanup. Paused create now first traverses the live startup/runtime MMIO/PCI
storage owners. File-backed Async closes all generation admissions, drains and
publishes every entered operation, delivers each resulting MMIO SPI or PCI
MSI-X completion interrupt, retains exact generation/counter/pressure, queue,
limiter, backing, transport, and interrupt state, then reopens the same
generations. Pmem similarly retains exact config/range/protection, backing and
authoritative direct-mapping identity, limiter/retry, queue, and transport
state. A live vhost endpoint returns one typed redacted unsupported result
before those mutations or any grant/staging activity. Native-v1 retains the
separate regular-file one-read-only-root profile: a block-special owner is
freshly reinspected (through the contained broker when applicable) and then
rejected before artifact staging. This capture-ready handoff adds no serialized
block-special variant, restore promise, or vhost snapshot state.
Internal HVF boot sessions can signal block SPI or MSI interrupts after
boot-runtime block notification dispatch.

## Internal arm64 FDT Generation

The runtime crate can build a minimal Firecracker-shaped arm64 FDT using the
same `vm-fdt` writer crate that Firecracker uses. The generated tree currently
contains root properties, CPU data, memory, chosen, timer, PSCI, GIC nodes, and
optional RTC, serial, VMGenID, and sorted virtio-mmio device nodes from caller-supplied
descriptors. The optional RTC node uses Firecracker's aarch64 PL031 shape with
`compatible = "arm,pl031", "arm,primecell"`, `reg`, `clocks`, and
`clock-names = "apb_pclk"`, and intentionally omits `interrupts` because the
minimal RTC device does not implement alarm interrupts. PCI and other device
nodes remain deferred until the corresponding emulation paths exist.
The FDT deliberately retains Firecracker v1.15.1's `arm,psci-0.2` compatible
string and `method = "hvc"`. As in that Firecracker/KVM baseline, the runtime
`PSCI_VERSION` call reports PSCI 1.0. The HVF backend decodes arm64 HVC
exception exits and handles `HVC #0` through one PSCI/SMCCC responder. The
aggregate boot-session path coordinates `CPU_SUSPEND32/64`, `CPU_ON32/64`,
`CPU_OFF`, and `AFFINITY_INFO32/64` against the ordered topology. The immediate
path returns `MIGRATE_INFO_TYPE` as the PSCI value for a trusted OS that is
MP-capable or not present, where migration is not required, and translates
`SYSTEM_OFF` and `SYSTEM_RESET` into guest-requested terminal boot run-loop
outcomes. `PSCI_FEATURES` returns zero only for the delivered PSCI functions
and `SMCCC_VERSION`; both CPU_SUSPEND IDs therefore declare original
power-state format and platform-coordinated mode. `SMCCC_VERSION` reports 1.1,
and its mandatory `SMCCC_ARCH_FEATURES` query returns success only for VERSION
and itself. Optional architecture workarounds, SoC ID, KVM paravirtual time,
vendor calls, and TRNG remain safely unsupported. Successful
`CPU_OFF` does not return to the caller or write X0; the last committed online
CPU receives `DENIED`. `CPU_SUSPEND` retains the caller's context and power
affinity, deliberately ignores all three ABI arguments like KVM's retained
standby path, and defers X0 `SUCCESS` until the caller's enabled,
guest-unmasked EL1 virtual timer becomes due and its PPI is pending. Wakeup and
pause cancellation keep the exact call pending for rearm; stop, shutdown, and
terminal drains do not synthesize success. The FDT does not publish CPU idle
states, and SGI/SPI/direct IRQ/FIQ wake is outside this timer-only subset.
Optional PSCI 1.0 power/statistics calls, PSCI 1.1+, other unsupported firmware
calls, and nonzero HVC immediates write `NOT_SUPPORTED` to X0.
Early boot also traps the guest's `OSDLR_EL1` and `OSLAR_EL1` OS lock
system-register accesses through the AArch64 SYS64 exception class (`0x18`),
not through SMCCC. The HVF runner handles only those observed
debug-register accesses with KVM-like RAZ/WI semantics: reads return zero,
writes are ignored, and other trapped system registers still fail closed.

The memory node excludes the first 2 MiB system area from the first DRAM range
and preserves later DRAM ranges from the runtime layout, but direct FDT
configuration must match the aarch64 DRAM layout helper for its total guest RAM
size. Sparse layouts, ranges overlapping the aarch64 MMIO64 gap, and total RAM
beyond the aarch64 maximum are rejected. The chosen node carries boot arguments,
optional initrd start/end properties from loaded boot-source metadata,
Firecracker's `linux,pci-probe-only = 1` property, and a Firecracker-shaped
64-byte `rng-seed` generated from host OS randomness during FDT construction.
`rng-seed` generation failures are reported before guest memory is mutated
during FDT writes. Emitting `linux,pci-probe-only` matches Firecracker's arm64
FDT shape but does not imply PCI device support.
Direct FDT configuration still validates that `bootargs` fits in the 2048-byte
aarch64 command-line capacity including the trailing NUL byte and contains no
embedded NUL bytes. The GIC node consumes backend-neutral distributor and
redistributor metadata and advertises `arm,gic-v3`. MSI-free metadata emits no
MSI child or GICv3 MBI/ITS property. Optional HVF MSI metadata instead emits one
hardware-described `arm,gic-v2m-frame` child with its own `msi-controller`,
phandle, and exact MMIO `reg`; it deliberately emits no software range
overrides. The builder requires the frame to contain the GICv2m TYPER, SETSPI,
and IIDR registers, restricts its SPI range to Linux's accepted domain ending
before INTID 1019, and rejects overlap with GIC, guest-memory, and device MMIO.
The FDT builder also rejects empty or oversized CPU sets, duplicate CPU `reg`
values, initrd ranges outside
guest-advertised memory or overlapping the reserved FDT window, and GIC MMIO
regions that are invalid, overlap each other, or overlap guest RAM. It also
rejects unexpected GIC compatibility strings and PPI collisions between the GIC
maintenance interrupt and timer interrupts.

Optional virtio-mmio nodes follow Firecracker's aarch64 FDT shape: node names
are `virtio_mmio@{base:x}`, each node has `dma-coherent`,
`compatible = "virtio,mmio"`, `reg = [base, size]`,
`interrupts = [SPI, line - 32, edge-rising]`, and `interrupt-parent` pointing
at the GIC phandle. Direct FDT configuration validates that each device range is
non-empty, does not overflow, does not overlap guest RAM, GIC distributor or
redistributor ranges, or another virtio-mmio range, and that each interrupt
line is an actual SPI INTID before encoding Firecracker's legacy GSI-style FDT
cell. The internal boot-resource assembly path composes block MMIO
registrations and caller-provided interrupt lines into these descriptors, while
HVF startup wiring allocates matching block SPI lines and can signal them after
boot-runtime notification dispatch.

An optional serial node follows Firecracker's aarch64 `ns16550a` shape: the
builder emits the shared `apb-pclk` fixed-clock node and a `uart@{base:x}` node
with `compatible = "ns16550a"`, `reg = [base, size]`,
`clocks = <apb-pclk>`, `clock-names = "apb_pclk"`, and
`interrupts = [SPI, line - 32, edge-rising]`. The serial node inherits the root
`interrupt-parent`, matching Firecracker's serial-specific node shape. Direct
FDT configuration validates that the serial region is non-empty, does not
overflow, does not overlap guest RAM, GIC distributor or redistributor ranges,
or any virtio-mmio range, and that the serial interrupt line is an SPI INTID
before encoding it. The internal boot-resource assembly path can register one
optional serial MMIO handler and pass matching serial FDT metadata from the
same placement and interrupt line.

## RTC-Adjacent Time And Identity Devices

bangbang currently implements only the guest-visible PL031 RTC subset described
above. Runtime metrics can emit a non-empty Firecracker-shaped `rtc` object
with `error_count`, `missed_read_count`, and `missed_write_count` for PL031
MMIO error paths. Signed executable direct-rootfs coverage checks that Linux
exposes `/dev/rtc0` as a character device and reports PL031 RTC evidence
through sysfs, procfs, or dmesg. RTC alarm interrupts are intentionally
unsupported in that subset because Firecracker's aarch64 PL031 node is exposed
without an interrupt line; guest flows that depend on RTC alarm interrupts
should not be treated as supported by the current compatibility surface.

PVTime/steal-time is a separate platform capability rather than an RTC feature.
Firecracker implements ARM steal-time by allocating per-vCPU memory and
registering it through KVM ARM vCPU device attributes. bangbang should not claim
PVTime until an HVF-specific capability and guest ABI design exists; for now it
is platform-limited and deferred.

VMGenID/SysGenID and VMClock are supported-target device families, but they are
not part of the minimal RTC device. The backend-neutral arm64 FDT builder emits
Firecracker's VMGenID DeviceTree shape: node name `vmgenid`, compatible string
`microsoft,vmgenid`, a 16-byte `reg` region, and `interrupts = [SPI, line - 32,
edge-rising]`. Direct FDT configuration validates that the VMGenID region is
exactly 16 bytes, does not overflow, does not overlap GIC, RTC, serial,
virtio-mmio ranges, or RAM advertised through the FDT `/memory` node, and that
the interrupt line is an SPI INTID. During startup, bangbang places the initial
VMGenID buffer immediately before the reserved VMClock page, writes a non-zero
16-byte generation ID from host randomness, and passes the same region and an
allocated SPI interrupt line to the FDT. The same builder also emits
Firecracker's startup VMClock DeviceTree shape: node name `ptp@{guest_address}`,
compatible string `amazon,vmclock`, a 4 KiB `reg` region, and `interrupts =
[SPI, line - 32, edge-rising]`. Direct FDT configuration validates that the
VMClock page is exactly 4 KiB, does not overflow, does not overlap GIC, RTC,
serial, virtio-mmio, VMGenID, or FDT-advertised RAM ranges, and that the
interrupt line is an SPI INTID. During startup, bangbang places the VMClock
page at the end of the reserved arm64 system-memory area, writes the minimal
Firecracker VMClock ABI fields for guest discovery, and leaves unsupported time
fields zeroed. Signed executable direct-rootfs coverage checks that Linux
observes the startup `amazon,vmclock` `ptp@...` device-tree node with a 16-byte
`reg` property tuple and 4 KiB region size through the public `bangbang` startup
path. Internally, a prepared never-run boot session can generate a distinct
nonzero VMGenID, write the complete guest buffer, commit retained metadata, and
then assert the edge-rising SPI. Random/preflight/write failures send no edge;
a signal failure is reported after commit and requires another replacement or
session discard. Public native-v1 load uses that transaction after aggregate
interrupt restore, and signed cross-process coverage proves the guest observes
both saved 64-bit VMGenID halves change before continued execution. VMClock
generation-counter updates, signaling, and mutable restore semantics remain
outside the narrow native-v1 profile; optional-device snapshot profiles and
broader time portability remain deferred.

FDT writes first reject mismatches between the layout used to describe guest RAM
and the allocated guest memory object. FDT bytes are then built before guest
memory is touched, checked against the reserved 2 MiB FDT window, and copied
with `GuestMemory::write_slice` at the aarch64 FDT address. Oversized,
overflowing, or unbacked writes fail before a partial copy. Memory layouts whose
memory `reg` property alone cannot fit in the FDT window are rejected before FDT
construction. The write result records the FDT guest address and byte size for
future boot-register setup.

## Internal Boot Resource Assembly

The runtime crate can assemble internal arm64 boot resources from stored VMM
controller configuration and caller-provided backend boot metadata. This path
requires a configured boot source, applies `mem_size_mib` to the aarch64 DRAM
layout, allocates guest memory, loads the arm64 Linux `Image` and optional
initrd, prepares configured block and network devices, registers their
virtio-mmio regions in a fresh internal `MmioDispatcher`, optionally registers
one PL031 RTC handler and one TX-only serial MMIO handler in the same dispatcher,
pairs block and network registrations with supplied SPI interrupt lines, and
writes the arm64 FDT with matching RTC, serial, and virtio-mmio metadata.

The assembled bundle owns the guest memory, loaded boot metadata, FDT write
metadata, MMIO dispatcher, optional RTC metadata, optional serial metadata/output
sink, and block and network FDT device metadata needed by later HVF startup
wiring. It fails with typed errors
for missing boot source, memory size
overflow or a memory size above the arm64 architectural maximum,
layout/allocation failure, boot-source loading failure, block-device preparation
failure, RTC, serial, block, or network MMIO registration failure, interrupt-line count
mismatch, or FDT write failure.

The assembled bundle is used by owned HVF startup preparation. HVF owns the
mapped guest memory while runtime metadata, the MMIO dispatcher, optional RTC
metadata, optional serial metadata, and block/network metadata stay available to
the retained session. bangbang
now starts an internal boot run-loop worker across bounded step windows after successful startup and retains internal active, paused, terminal-outcome, or error worker status, but
does not yet provide full Firecracker run-loop control beyond the current pause/resume subset, signal backend
interrupts outside the internal boot block and network notification paths,
or prove guest boot with an integration test.

The runtime crate also contains an internal MMIO region registry, operation
model, and handler dispatch boundary for future real devices. It reuses
`GuestMemoryRange`'s end-exclusive semantics instead of Firecracker's
inclusive-end `BusRange` representation.
Region registration rejects zero-sized or overflowing ranges and accepts
adjacent non-overlapping ranges. Lookups validate that the whole access range is
owned by one region before returning the region owner and offset; accesses that
hit a hole, overflow, or cross a region boundary are rejected. A resolved access
can be wrapped as a read or write operation with bounded 1-, 2-, 4-, or 8-byte
data, and write construction rejects data whose length does not match the
resolved access size. A runtime dispatcher can route those checked operations
to registered internal handlers by region owner. HVF-specific helpers can now
dispatch one resolved HVF MMIO exit through those runtime handlers and complete
successful read results back into the trapped guest GPR. The runner can also
perform one `hv_vcpu_run` step, resolve a resulting MMIO exit against a shared
dispatcher, and dispatch or complete it on the vCPU-owning thread. This is
still not continuous run-loop policy, complete device emulation, or interrupt
delivery.

The runtime crate also contains a TX-only `ns16550a`-shaped serial MMIO output
device model. It supports one-byte transmit-register writes, divisor-latch
writes when DLAB is set, deterministic status/configuration reads, and explicit
errors for unsupported widths, invalid offsets, read-only writes, and output
sink failures. Output is captured through an injected sink instead of global
state, and the provided in-memory sink has an explicit byte limit, so
independent device instances do not share guest console data or grow host
memory without a caller-chosen bound. A shared sink lets the internal
boot-resource assembly path register a serial handler while retaining an output
handle for default internal capture or a configured file-backed output path.
The internal arm64 FDT builder can describe the same serial MMIO descriptor as
a Firecracker-shaped `uart@...` node. Public `/serial` supports pre-boot
`serial_out_path` storage, startup-time host output redirection, rate limiting,
and Firecracker-shaped metrics for implemented TX output paths; kernel
`earlycon` wiring, serial input/RX, and public serial streaming remain
deferred. The first internal guest boot integration test uses the bounded
capture path directly.

The runtime crate can decode checked MMIO operations into typed virtio-mmio
generic-register or device-configuration accesses for the Firecracker `v1.16.0`
transport window. The generic register decoder accepts only exact 4-byte reads
or writes at the Firecracker-supported common register offsets and rejects
unsupported offsets, unsupported widths, cross-register accesses, and accesses
outside the 4 KiB virtio-mmio device window before future device-specific state
can be mutated. A backend-neutral common-register state model can return
Firecracker-shaped identity values, expose selected 32-bit device feature
pages, accept selected driver feature pages only in the pre-`FEATURES_OK`
driver state, and enforce the cumulative VirtIO status transition sequence
plus reset-on-zero behavior. A separate backend-neutral queue-register model
tracks selected queue state, validates queue sizes, records queue ready state,
and composes descriptor, driver, and device ring address halves with the
alignment required by the virtqueue model. A queue-notification register model
records valid `QueueNotify` writes after `DRIVER_OK`, rejects notifications for
unsupported queues or invalid device states, and can drain the coalesced pending
queue indexes for future device handlers. A separate backend-neutral interrupt
register model can expose pending queue/configuration interrupt bits through
`InterruptStatus` and clear selected bits through `InterruptAck` after
`DRIVER_OK`, while rejecting unknown acknowledgement bits at the checked runtime
boundary. A composed backend-neutral register handler routes checked common
register reads and writes through those state models, implements the runtime
MMIO handler boundary, and exposes the notification drain without exposing
mutable nested state. Device-configuration accesses are classified by offset and
length and can be delegated through a backend-neutral config handler; config
writes are delegated only after the `DRIVER` status bit is set and while
`FAILED` and `DEVICE_NEEDS_RESET` are clear. The composed handler can invoke a
backend-neutral activation hook when `DRIVER_OK` is accepted and call its
reset hook when the virtio-mmio status is reset to zero or the handler is
explicitly reset. Activation failure marks the device as needing reset, but
concrete device activation effects, device config layouts, config generation
policy, and general runner-loop device-backed notification dispatch are still
deferred. Activated queue metadata can now feed the internal virtio-block,
virtio-net, and virtio-vsock queue dispatchers. Boot runtime resources
can dispatch registered block-device, virtio-net, and virtio-vsock queue notifications
against caller-supplied guest memory. Internal HVF boot sessions can signal
needed block, network, and vsock SPI interrupts from those dispatch summaries, but
future public scheduler and device policy remain deferred. The
shared virtqueue descriptor-chain reader supports direct chains by default and
negotiated `VIRTIO_RING_F_INDIRECT_DESC` indirect descriptor tables for
virtio-block, virtio-net, and virtio-vsock RX/TX queues, while preserving the
main descriptor head for used-ring completions. The virtqueue model can publish
one used-ring completion element with validated layout, mapped-memory checks,
wrapping, and release ordering. Virtio-block queue dispatch, network RX/TX
dispatch, and vsock RX/TX dispatch honor negotiated used-event interrupt
suppression for each published completion and publish a shared used-ring
`avail_event` hint for available-buffer notification suppression, while
batching and device-backed completion loops are still deferred.

The runtime crate also contains backend-neutral interrupt signaling groundwork.
It can validate nonzero guest interrupt lines, represent queue and
configuration pending-status bits, acknowledge selected pending bits, and let a
device-facing trigger record pending state before delegating backend signaling
to an injected sink. The HVF crate can allocate deterministic guest interrupt
lines from the validated GIC SPI range, signal validated SPI levels through
`hv_gic_set_spi`, and set or clear validated GIC PPI pending bits through
redistributor pending registers on the vCPU-owning thread. Internal HVF boot
sessions use the SPI signal path for block queue interrupts and virtio-net
queue interrupts after boot-runtime notification dispatch. This follows Firecracker's separation between
device-facing interrupt triggers and KVM-specific irqfd/GSI routing, but it is
not yet device interrupt masking, timer EOI policy, runner-loop interrupt
dispatch, or guest-visible device delivery.

The HVF backend can decode candidate MMIO accesses from arm64 data-abort
exception exits and decode trapped AArch64 SYS64 system-register exits. The
MMIO decoder converts supported ESR and IPA metadata into a checked access
range, direction, width, register number, and read-extension metadata while the
raw exit snapshot still preserves FAR. Unsupported exception classes, missing
instruction-syndrome metadata, table-walk aborts, cache-maintenance aborts, and
overflowing access ranges fail closed before runtime dispatch or later HVF
completion can use them. Decoded accesses can also be resolved against the
runtime MMIO registry to identify the owning region, offset, and preserved HVF
access metadata. Whole vCPU exits can be classified into resolved MMIO, SYS64,
HVC, virtual-timer, canceled, or unknown events while preserving typed decode
and bus-resolution errors. A single resolved HVF MMIO exit can be converted into
a runtime read/write operation by reading the trapped guest GPR for writes,
dispatched to a runtime handler, and completed back into the
trapped guest GPR for successful reads with zero/sign extension and 32-bit or
64-bit target width handling.
Guest GPR 31 is rejected explicitly so it is not confused with HVF's PC
register. The runner uses a non-blocking dispatcher lock after a run step
returns an MMIO exception; it does not hold the dispatcher while `hv_vcpu_run`
is blocked. There is still no continuous run-loop policy, public interrupt
delivery, or real device emulation beyond the internal boot block and
virtio-net notification signal steps.

The HVF backend can map allocated guest memory regions into an existing
Hypervisor.framework VM with read/write/execute guest RAM permissions. The
backend-owned mapping owner consumes the `GuestMemory` allocation, unmaps mapped
regions on explicit unmap, partial failure, drop, and VM destruction, and keeps
cleanup local to the backend instance. The internal HVF boot-session preparation
path maps the guest memory after runtime boot-resource assembly and releases the
mapping with VM-owned state when the session shuts down or is dropped. An owned
internal boot-session handle can keep the prepared HVF backend/session resources
as one storable value for future process startup wiring.

On macOS 15.0 or newer, the HVF backend can create a GIC v3 device after VM
creation and before vCPU creation. It dynamically resolves the macOS 15 GIC
symbols so older hosts can return structured unsupported errors instead of
failing at process load time. The backend exposes internal boot metadata for the
future FDT path: distributor and redistributor regions below the 1 GiB MMIO32
boundary, the supported SPI range, timer interrupt IDs, and the `arm,gic-v3`
compatibility shape. An internal SPI signaler validates guest interrupt lines
against that supported range before setting explicit GIC SPI levels with
`hv_gic_set_spi`. A narrow internal PPI pending primitive validates real GIC
PPI INTIDs before writing `GICR_ISPENDR0` or `GICR_ICPENDR0` through
`hv_gic_set_redistributor_reg` on the vCPU-owning thread. HVF timer INTIDs are
converted to FDT PPI cells for the runtime timer node.
An explicit internal boot option additionally loads only the public HVF MSI
geometry/configuration/send symbols, places the GICM region below the
redistributor, and reserves a demand-sized message-only SPI suffix while
preserving one contiguous legacy prefix. HVF reports SPIs 32 through 1019 on
the tested host, but Linux's GICv2m driver rejects a frame reaching 1019, so the
partition leaves that terminal INTID unallocatable and reserves downward from
1018. Typed allocator tokens retain range provenance, and the cloneable,
mutex-serialized sender accepts only a token from its exact configured range
before calling `hv_gic_send_msi` at the frame's SETSPI address. Configuration,
metadata, tokens, allocator state, and sender diagnostics redact counts,
addresses, and INTIDs. Default creation does not query or publish MSI state;
exact `--enable-pci` selects a demand-sized GICv2m frame and complete modern
virtio-pci startup composition on the supported product path. There is no
GICv3 MBI/ITS, PCI snapshot persistence, or delivery rollback guarantee.
Runtime block, pmem, and network PUT/DELETE use the retained bounded
PCI/GICv2m resource manager and require manual guest coordination; pmem
additionally owns an exact direct HVF mapping lease and reusable guest range,
while network coordinates an independent packet-I/O and metrics generation.
Separately, typed owner-thread HVF commands get or set CPU-level IRQ/FIQ pending
injection levels, capture both in IRQ-then-FIQ order, and reapply the complete
typed value in that same order. Those levels are not GIC state and HVF clears
them after a vCPU run returns, so individual mutation and aggregate restore are
pre-run injection primitives rather than durable delivery state.

The HVF backend also has an internal ordered vCPU-topology prerequisite. It
queries `hv_vm_get_max_vcpu_count`, rejects requests outside both the portable
`1..=32` ceiling and the current host maximum before starting an owner, and can
create affinities `0..vcpu_count` only after one VM and its GIC exist. Every
idle vCPU stays on a permanent owner thread, writes and reads back its MPIDR
there before publication, and participates in topology-wide cancellation and
reverse-order shutdown. Construction returns no successful prefix; a later
owner or affinity failure shuts down every retained owner and preserves both
the primary failure and indexed cleanup failures. Internal boot sessions now
consume this topology: the complete MPIDR list drives FDT CPU nodes, PSCI
targets, indexed runs, and per-vCPU PPI routing. Public `InstanceStart` still
rejects counts other than one before host-resource work, and native-v1 remains
one-vCPU.

This still is not public guest startup. bangbang can now write an internal FDT
payload, create an internal size-one-or-many HVF arm64 boot session, retain all
runner-owned `MPIDR_EL1` values as ordered boot metadata, allocate deterministic
block and optional serial SPI interrupt lines, and map the assembled guest
memory into HVF. Only the primary initially receives the arm64 Linux boot
register state: PC points at the loaded kernel entry, X0 points at the FDT guest
address, X1-X3 are zero, and CPSR/PSTATE is `0x3c5`. Each runner path sets and
verifies its ordered `MPIDR_EL1` affinity before redistributor access; primary
boot-register setup remains owner-thread-only and rejects duplicate setup,
setup during shutdown, setup while a run is in flight, and setup after a run has
started. If setup fails after partially writing registers, the runner rejects
guest runs until setup is retried successfully. The runner also exposes explicit
single-exit MMIO commands, physical-timer capture, and virtual-timer mask,
offset, control, and CVAL commands that run on the vCPU-owning thread. One
command dispatches an already resolved MMIO access after a run has started, and
another command starts one
vCPU run, resolves a resulting MMIO exit, and dispatches or completes it through
a caller-provided shared dispatcher. The virtual-timer commands expose HVF's
explicit mask bit after `HV_EXIT_REASON_VTIMER_ACTIVATED`, its raw
host-time-relative offset, and raw `CNTV_CTL_EL0`/`CNTV_CVAL_EL0` values; CPU
IRQ/FIQ commands expose and can reapply complete pending injection levels; and
GIC PPI pending commands can set or clear a validated timer PPI bit on the
runner thread. The internal boot-session
run-loop now handles virtual timer exits by asserting the EL1 virtual timer PPI
through that runner-thread command. The same owner-local state backs PSCI
`CPU_SUSPEND` retained standby: the suspended online member participates in
normal coordinator generations through an interruptible timer wait, and a due
timer publishes the PPI before the deferred PSCI result. Full timer delivery
policy, including how to detect EOI/deactivation and unmask the HVF virtual
timer, and non-timer suspend wake sources remain future work.
These commands reject overlapping metadata reads, runs, boot-register setup,
MMIO dispatches, core-register operations, timer operations, or generalized
interrupt operations. The general-register capture command returns only after
X0-X30, PC, and CPSR have all been read. The same typed value can be passed to
a separate owner-thread restore operation, which writes X0-X30, PC, and CPSR
in architectural order. Hypervisor.framework does not make those 33 writes
transactional: a typed failure identifies the failed register and completed
write count, and callers must retry the complete value or discard the vCPU
before execution. A second capture command reads raw `SP_EL0`,
`SP_EL1`, `ELR_EL1`, and `SPSR_EL1` in that order. Its typed value has a paired
owner-thread restore operation that writes the same four fields in capture
order. A reusable system-register failure identifies the exact failed register
and completed-write count; the writes are nontransactional and require the same
complete-retry-or-discard rule. A third capture reads all 16 bytes of
Q0-Q31 in ascending order, then raw FPCR and FPSR. Its typed value has a paired
owner-thread restore that writes the same 34 fields in capture order. A
dedicated typed failure distinguishes the SIMD/FP and scalar register spaces,
identifies the completed prefix, and requires the same complete-retry-or-
discard rule. A fourth reads raw
`TPIDR_EL0`, `TPIDRRO_EL0`, and `TPIDR_EL1`. Its typed value has a paired
owner-thread restore that writes the same three fields in capture order under
the reusable system-register partial-write contract. A fifth reads raw `SCTLR_EL1`,
`TTBR0_EL1`, `TTBR1_EL1`, `TCR_EL1`, `MAIR_EL1`, `AMAIR_EL1`, and
`CONTEXTIDR_EL1`. Its typed value has a paired owner-thread restore that writes
the same seven fields in capture order under the reusable system-register
partial-write contract. A sixth reads raw `AFSR0_EL1`, `AFSR1_EL1`, `ESR_EL1`,
`FAR_EL1`, `PAR_EL1`, and `VBAR_EL1`. Its typed value has a paired owner-thread
restore that writes the same six fields in capture order and reuses the typed
system-register failure with exact partial progress. A seventh reads raw
`ACTLR_EL1` then `CPACR_EL1`; complete capture requires macOS 15 for
ACTLR.EnTSO. Its typed value has a paired owner-thread restore that writes both
fields in capture order under the same typed partial-write contract. An eighth
reads the low and high halves of APIA, APIB, APDA, APDB, and APGA and publishes
five 128-bit pointer-authentication keys. Its redacted typed value has a paired
owner-thread restore that writes the same ten halves in capture order through
the reusable system-register partial-write contract. A ninth reads guest-visible `MIDR_EL1`,
`MPIDR_EL1`, PFR0/1, DFR0/1, ISAR0/1, and MMFR0/1/2 compatibility metadata. A
tenth reads raw `MDCCINT_EL1` then `MDSCR_EL1`; its typed value has a paired
owner-thread restore that writes both registers in capture order through the
reusable system-register partial-write contract without changing the separately
owned Hypervisor.framework debug-trap settings. An eleventh reads raw
`CSSELR_EL1`; its typed value has a paired owner-thread restore that writes the
same selector through the reusable system-register partial-write contract
without consuming its selected `CCSIDR_EL1` view. A twelfth reads
`ID_AA64DFR0_EL1`, derives `BRPs + 1`, then
reads every implemented `DBGBVR<n>_EL1` / `DBGBCR<n>_EL1` pair in ascending
order without writing or enabling debug state. A thirteenth reads
`ID_AA64DFR0_EL1`, derives `WRPs + 1`, then reads every implemented
`DBGWVR<n>_EL1` / `DBGWCR<n>_EL1` pair in ascending order under the same
observation-only constraints. A fourteenth calls Hypervisor.framework's
debug-exception trap getter then its debug-register-access trap getter, exposing
the two host TDE/TDA-equivalent policy booleans. Its typed value has a paired
owner-thread restore that calls the matching setters in the same order and
reports exact value-free partial progress. The fifteenth reads optional
`ID_AA64ZFR0_EL1` then `ID_AA64SMFR0_EL1`
compatibility metadata and requires macOS 15.2. A sixteenth calls the macOS
15.2+ `hv_vcpu_get_sme_state` getter once and returns the guest's `PSTATE.SM`
streaming-mode and `PSTATE.ZA` storage-enable flags without invoking the setter.
A seventeenth reads raw `SMCR_EL1`, `SMPRI_EL1`, and `TPIDR2_EL0` in that order
on macOS 15.2+ and publishes them through a value whose `Debug` output redacts
all three registers. An eighteenth reads raw `SCXTNUM_EL0` then `SCXTNUM_EL1`
on macOS 15.2+ and redacts both guest software context numbers from `Debug`;
its typed value has a paired owner-thread restore that writes both fields in
capture order through the reusable system-register partial-write contract.
The nineteenth first observes `PSTATE.SM`, then, only while streaming mode is
active, queries the maximum SVL, fallibly allocates one contiguous `32 * max`
buffer, and runtime-resolves `hv_vcpu_get_sme_z_reg` for exact Z0-Z31 reads.
Its detached value exposes bounded maximum-width slices while redacting all
bytes from `Debug`. A twentieth performs the same owner-thread streaming-mode
preflight, requires a non-zero maximum SVL divisible by eight, fallibly
allocates one contiguous `16 * (max / 8)` buffer, and runtime-resolves
`hv_vcpu_get_sme_p_reg` for exact P0-P15 reads. Its detached value exposes
bounded predicate-width slices while redacting all bytes from `Debug`. A
twenty-first observes `PSTATE.ZA` without requiring `PSTATE.SM`, then queries a
non-zero maximum SVL, fallibly allocates its checked square, and runtime-
resolves `hv_vcpu_get_sme_za_reg` for one complete matrix read. Its detached
value exposes the raw square while redacting all bytes and dimensions from
`Debug`. A twenty-second observes the same `PSTATE.ZA` precondition without
requiring `PSTATE.SM`, then runtime-resolves `hv_vcpu_get_sme_zt0_reg` and
publishes its fixed 64 bytes only after the single aligned SDK read succeeds.
Its detached value redacts every byte from `Debug`. The twenty-two capture
commands plus the general-, core-system-, exception-register, execution-
control, cache-selection, debug-control, debug-trap-policy, thread-context, translation,
baseline SIMD/FP, pointer-authentication key, and system-context restore
operations form a thirty-four-operation command-owned core-register admission
domain.
Captures publish no partial state
after a read failure; restores explicitly may leave a written prefix after a
setter failure. Borrowed and owned HVF boot sessions expose all twelve restores
in this core domain and all captures for later lease-owned orchestration.
Separately, a no-handle `HvfBackend::arm64_sme_configuration()` query
runtime-resolves macOS 15.2+
`hv_sme_config_get_max_svl_bytes` and publishes the maximum streaming vector
length, in bytes, that guests may use. It can run before backend or VM creation,
does not enter the core-register admission domain, and preserves missing-symbol,
target, and exact HVF failures. This configuration maximum is the current
conditional Z-, P-, and ZA-register allocation basis; it is not the effective
SVL selected through `SMCR_EL1`,
feature metadata, PSTATE, or any Z/P/ZA/ZT0 content.
Another no-handle `HvfBackend::arm64_vcpu_cache_configuration()` query creates
and releases a fresh macOS 11+ default vCPU configuration, reads raw `CTR_EL0`,
`CLIDR_EL1`, and `DCZID_EL0` feature values in that order, and publishes only
the complete detached value. It does not alter the null/default configuration
used by vCPU creation or enter runner admission. The triple is separate from a
live guest `CSSELR_EL1` selector and the instruction/data `CCSIDR_EL1` arrays;
it defines no interpretation, feature mask, destination decision, cache
maintenance, persistence, schema, or restore behavior.
A separate no-handle `HvfBackend::arm64_vcpu_cache_geometry()` query creates
and releases another fresh default configuration, reads all eight raw data or
unified `CCSIDR_EL1` values followed by all eight instruction values, and
publishes only the complete detached geometry. It preserves every SDK entry
without selecting cache levels, interpreting fields, or entering runner
admission. Because the feature and geometry methods own independent
configurations, their results do not form one atomic manifest. The geometry
defines no feature mask, destination decision, synchronization, cache
maintenance, persistence, schema, or restore behavior.
A distinct internal startup query reads `ID_AA64MMFR2_EL1`, the feature triple,
and both geometry arrays from one retained default configuration. Ordinary
arm64 boot interprets and reconciles that same-configuration source for its FDT,
then retains both the source and validated presentation. Native-v1 capture
reuses the retained manifest after comparing its MMFR2 value with the runner's
guest-visible identification capture; it does not query a second default
configuration. The original public feature and geometry methods remain
independent raw diagnostic surfaces and do not form this atomic source.
TPIDR values can contain
guest TLS or kernel pointers; translation table bases, context ids, fault
addresses, and the vector base are sensitive; pointer-authentication keys are
cryptographic secrets; breakpoint values can reveal guest virtual addresses or
identities; watchpoint values reveal guest data virtual addresses; comparator
and debug controls plus host debug-trap policy are security-sensitive execution
state; SME PSTATE reveals mutable guest streaming/ZA execution mode; software
context numbers can identify guest execution contexts; and SME
system registers include mutable controls plus `TPIDR2_EL0` thread context that
remains outside the baseline thread-context subset. Streaming Z registers, P
predicates, the ZA matrix, and SME2 ZT0 can contain sensitive guest execution
and cryptographic material. The key, SME Z-register, SME P-register, SME
ZA-register, SME ZT0-register, SME system-register, and system-context values
redact all raw material from `Debug` but provide bounded or named accessors for
trusted internal composition. Identification values
describe the virtual CPU/HVF view, including bangbang's deterministic MPIDR
affinity zero; they are not
physical-host identity or a destination compatibility decision. The stable
baseline keeps macOS 15.2 ZFR0/SMFR0 metadata in a separate optional value;
newer beta-only IDs and broader configuration-time feature manifests remain
omitted. The separate maximum-SVL configuration query is runtime-resolved so a
pre-macOS-15.2 process returns a structured unsupported error instead of
failing to load. An available symbol preserves HVF's raw `HV_UNSUPPORTED` on
SME-incapable hardware, and the `size_t` result remains a Rust `usize` without
narrowing, caching, or architecture-specific inference. It defines no feature
or destination policy, effective-SVL selection, persistence, schema, or restore
behavior and does not itself allocate execution state.
The SME PSTATE getter is resolved at runtime so a pre-macOS-15.2 process returns
a structured unsupported error instead of failing to load. An available symbol
preserves HVF's raw `HV_UNSUPPORTED` result on SME-incapable hardware. The two
flags are separate from feature metadata and from the conditionally present
Z/P/ZA/ZT0 contents; no setter, transition, or restore ordering is defined.
The separate Z-register capture preflights that `PSTATE.SM` is enabled before
querying the configuration-wide maximum SVL or allocating memory. It then reads
Z0 through Z31 into exact maximum-width chunks and publishes only the complete
value. The maximum is an allocation width, not the effective `SMCR_EL1.LEN`;
baseline Q registers alias only each Z register's low 128 bits while streaming
mode is active. P predicates, ZA, and ZT0 are captured separately. Setters and
transitions, byte-layout interpretation,
feature/destination validation, encrypted persistence, schema, restore
ordering, orchestration, and multi-vCPU association remain deferred.
The separate P-register capture likewise preflights `PSTATE.SM`, then requires
the configuration-wide maximum SVL to be non-zero and divisible by eight. It
reads P0 through P15 into exact `maximum / 8`-byte chunks and publishes only the
complete value. The maximum remains an allocation basis rather than the
effective `SMCR_EL1.LEN`; Z registers, ZA, and ZT0 are captured separately.
Setters and transitions, byte-layout and inactive-lane interpretation,
feature/destination validation, encrypted persistence, schema, restore
ordering, orchestration, and multi-vCPU association remain deferred.
The separate ZA-register capture preflights `PSTATE.ZA` but does not require
streaming mode. It then queries a non-zero configuration-wide maximum SVL,
checked-squares that byte count, fallibly allocates the exact result, and calls
the runtime-resolved getter once. The raw complete value is published only on
success and redacts bytes and dimensions from `Debug`. The maximum is an
allocation dimension, not an effective-SVL or row/tile interpretation. ZT0 is
captured separately. Setters and transitions, layout interpretation,
feature/destination validation,
encrypted persistence, schema, restore ordering, orchestration, and multi-vCPU
association remain deferred.
The separate SME2 ZT0-register capture preflights `PSTATE.ZA` without requiring
streaming mode, then calls its runtime-resolved getter once through a private
64-byte, 16-byte-aligned SDK-compatible output value. It does not query maximum
SVL. The detached fixed-size value is published only on success and redacts all
bytes from `Debug`. Setters and transitions, SME2 feature/destination policy,
lane interpretation, encrypted persistence, schema, restore ordering,
orchestration, and multi-vCPU association remain deferred.
The separate SME system-register capture uses the macOS 15.2 SDK register ids
through the existing owner-thread getter and preserves each raw backend error.
It performs no writes and defines no writable-bit or feature validation,
maximum-SVL policy, persistence, schema, or restore ordering with PSTATE and
the conditionally present Z/P/ZA/ZT0 contents.
The separate system-context capture uses macOS 15.2 SDK register ids through the
same owner-thread getter and preserves raw backend errors. Its paired restore
writes `SCXTNUM_EL0` then `SCXTNUM_EL1` through the same owner and reports the
exact failed register and completed prefix without formatting either value.
The two writes are nontransactional, so failure requires a complete retry or
vCPU discard before execution. The primitive defines no interpretation,
feature or destination validation, protected persistence, rollback, schema,
or wider restore ordering with TPIDR and `CONTEXTIDR_EL1` state.
The separate cache-selection capture uses the stable `CSSELR_EL1` SDK id
through the same owner-thread getter. Its paired restore writes the complete
typed selector once through the owner and reports the exact failed register,
zero completed writes, and backend source without formatting the value.
Failure requires a complete retry or vCPU discard before execution. The raw
apply does not validate an encoding or destination cache manifest, issue ISB,
guarantee a dependent `CCSIDR_EL1` view, perform maintenance, persist state,
roll back, or define a portable snapshot schema.
The translation value omits table memory, feature and destination validation,
TLB/cache maintenance, barriers, and a safe MMU transition sequence. Its paired
restore merely reapplies the complete raw capture in field order and may leave
a written prefix on failure. The exception value omits vector-table memory,
semantic validation, and safe restore ordering. Signed validation leaves the
MMU disabled, uses an aligned unused VBAR without an intervening guest exception,
and accepts implementation-defined AMAIR and AFSR readback after guest writes.
Execution-control validation writes only EnTSO and baseline FPEN, executes ISB,
and does not cover optional CPACR features. Key validation uses visibly fake
values and does not enable or execute PAC. Identification validation compares
two captures and the existing MPIDR getter without hard-coding an Apple CPU
model or claiming destination portability. Optional SVE/SME identification
validation reads ZFR0/SMFR0 twice from an idle macOS 15.2+ vCPU without enabling
SVE/SME, entering streaming mode, reading execution state, running the guest,
or treating equality as a destination policy.
The maximum-SVL configuration validation queries twice before constructing a
backend or VM, compares two successful values only through fixed failure
messages without logging the byte length, and accepts two exact raw
`HV_UNSUPPORTED` outcomes. Missing symbols, mixed outcomes, and unrelated
errors fail. It does not infer effective `SMCR_EL1.LEN`, create or run a vCPU,
change SME state, read SME data, or claim feature/destination compatibility.
SME PSTATE validation calls the
getter twice on the same idle vCPU and compares supported results without
assuming or logging either flag; documented missing-symbol and raw
`HV_UNSUPPORTED` outcomes are accepted, while unrelated errors fail. It never
calls the setter, enters streaming mode, enables ZA, reads Z/P/ZA/ZT0, or runs
the guest. SME Z-register validation accepts only a documented macOS/HVF
availability result, the topical inactive-streaming result, or two complete
equal maximum-width Z0-Z31 captures from the same idle vCPU. It verifies bounded
accessors and redacted `Debug` with fixed messages without logging bytes or
width, calling a setter, entering streaming mode, running guest code, or
inferring effective SVL or portability. SME P-register validation accepts the
same documented macOS/HVF availability and inactive-streaming outcomes, or two
complete equal maximum-derived P0-P15 captures from the idle vCPU. It verifies
the maximum and predicate widths, bounded accessors, and redacted `Debug` with
fixed messages without logging bytes or widths, calling a setter, entering
streaming mode, running guest code, or inferring effective SVL or portability.
SME ZA-register validation accepts the documented macOS/HVF availability or
topical inactive-ZA outcomes, or two complete equal maximum-square captures
from the idle vCPU. It verifies the reported maximum, exact checked-square
length, raw accessor, and redacted `Debug` with fixed messages without logging
bytes or dimensions, calling a setter, enabling ZA or streaming mode, running
guest code, or inferring layout, effective SVL, or portability.
SME ZT0-register validation accepts the documented macOS/HVF availability or
topical inactive-ZA outcomes, or two complete equal fixed 64-byte captures from
the idle vCPU. It verifies the fixed-size accessor and fully redacted `Debug`
with fixed messages without logging bytes, calling a setter, enabling ZA or
streaming mode, querying maximum SVL, running guest code, or inferring SME2
feature/destination, lane, or portability semantics.
SME system-register validation captures all three registers twice
from the same idle vCPU, compares them only with fixed failure messages, and
checks redacted `Debug` output. It does not log raw values, write registers,
query maximum SVL, read Z/P/ZA/ZT0, or run the guest.
System-context validation captures both registers twice from the same idle
vCPU, compares them only with fixed failure messages, and checks redacted
`Debug` output. It then restores and recaptures the complete first value twice
without logging raw values, running guest code, hard-coding a reset value, or
inferring feature or destination compatibility. Debug-control validation
captures the original pair from an idle real vCPU, restores and recaptures that
exact pair twice, and compares whole values without assuming or logging either
register, manufacturing a control change, altering comparator or host trap
state, enabling debug behavior, or executing the guest.
Breakpoint and watchpoint comparators and their respective DFR0-reported counts
are captured through separate values. HVF's separate debug-exception and debug-
register-access trap booleans are captured through another value and correspond
to host TDE/TDA-equivalent policy rather than guest EL1 register contents.
Comparator validation only observes every reported pair on an idle vCPU without
logging raw values, writes, enablement, trap changes, guest instructions, or
guest execution. Debug-trap validation captures the original pair from an idle
vCPU, restores and recaptures that exact pair twice, and compares whole values
without assuming or logging either Boolean, manufacturing a policy change,
altering guest debug state, activating debug behavior, or executing the guest.
Cache-selection validation
captures the selector twice from an idle real vCPU, then restores and
recaptures the first complete value twice through fixed whole-state messages.
It does not assume or log a reset value, issue ISB, query CCSIDR, perform cache
maintenance, run guest code, or infer topology or destination compatibility.
Default-cache-configuration validation queries CTR_EL0/CLIDR_EL1/DCZID_EL0
twice before constructing a backend or VM and compares only through fixed
messages without logging raw values. It creates/runs no vCPU, touches no live
selector, queries no CCSIDR itself, performs no maintenance, and infers no
topology or destination policy. Separate default-cache-geometry validation
queries both complete eight-entry arrays twice before backend or VM creation
and compares them only through fixed messages without logging raw values. It
also creates/runs no vCPU, touches no live selector, issues no live CCSIDR read
or ISB, and performs no maintenance. The selector is not cache topology: the
default feature triple and geometry are independent fresh-configuration
queries, not one atomic compatibility manifest. #1392's combined startup
source is a separate internal path and does not change these standalone test
contracts.
The SIMD getter uses an explicitly 16-byte-aligned HVF output value. The SDK
setter instead accepts a Clang vector by value, which stable Rust cannot declare
through `extern "C"`; one macOS arm64 C shim accepts an ordinary 16-byte pointer
and invokes the SDK with Clang's matching vector ABI. The separate SME PSTATE
observation determines whether streaming mode is active, where Q writes and
reads alias the low 128 bits of Z registers. The baseline restore defines no
ordering with that wider state. The separate
maximum-width Z capture contains Z0-Z31 only when streaming mode is already
active; the separate maximum-derived P capture contains P0-P15 under the same
precondition. The separate maximum-square ZA and fixed-size SME2 ZT0 captures
instead require only `PSTATE.ZA`; neither requires streaming mode. A separate
command reads raw `CNTKCTL_EL1`, `CNTP_CTL_EL0`, `CNTP_CVAL_EL0`, and
`CNTP_TVAL_EL0` in that order, publishes no partial state if any read fails, and
shares generalized timer admission with every virtual-timer command. Both boot-
session forms expose the immutable value. CNTP access requires macOS 15 and GIC
creation before the vCPU.
Control ISTATUS is derived, the absolute CVAL is compared against a continuing
physical count, and the architecturally signed 32-bit relative TVAL is returned
as raw `u64` and changes while the sequential reads proceed. The value therefore
has no simultaneous CVAL/TVAL guarantee, portable elapsed-time adjustment,
interrupt-delivery, writable-bit, or restore policy. A separate command
reads the virtual-timer mask, raw offset, control, and CVAL in that order,
publishes no partial state if any read fails, and keeps command-owned admission
until all four reads finish even if the caller abandons its response. Both
boot-session forms expose that immutable subset. The raw offset follows HVF's
`CNTVCT_EL0 = mach_absolute_time() - offset` relation, while control ISTATUS is
derived and may change as virtual time advances. This capture does not include
GIC state and does not define portable offset adjustment
or control-restore policy. A separate native policy samples both raw timer
domains against one `mach_absolute_time()` value, stores virtual count and
physical CVAL distance, strips ISTATUS, ignores TVAL, and retains only
ENABLE/IMASK. Its never-run owner command reconstructs the destination offset
and CVAL after complete read/clock preflight, then applies a ten-write safe
order with typed value-free partial progress. Snapshot downtime is frozen;
after restore, both domains advance by the same host-counter interval. Retry
uses a fresh sample, and a partially updated destination must never run. The baseline and optional SVE/SME identification,
SME PSTATE, SME Z-register, SME P-register, SME ZA-register, SME system-register,
breakpoint, watchpoint, and physical-timer
subsets are raw, getter-only observations and likewise have no restore
validation, snapshot schema, or Firecracker on-disk compatibility.
The core system-register, EL1 exception, execution-control, cache-selection,
debug-control, debug-trap policy, thread-context, and translation subsets plus system-context,
baseline SIMD/FP, and pointer-authentication keys have paired ordered,
nontransactional restore operations but likewise have no validation, schema,
dependent-memory, maintenance, feature-transition, SVE/SME alias, or wider
ordering policy.
Identification capture is compatibility metadata rather than mutable restore
state and defines no feature-mask or destination policy. Guest debug-control
capture/apply and host debug-trap capture/apply remain separate and define no joint
feature, writable/status-bit, security, trap-coordination, synchronization, or
composite restore policy.
Cache-selection capture-order apply defines no atomic topology manifest,
selector or destination validation, ISB/dependent CCSIDR visibility,
maintenance, compatibility, persistence, rollback, schema, or portable restore
policy.
Pointer-authentication capture and raw apply additionally have no feature or
destination validation, zeroization, protected persistence, rollback, or safe
SCTLR enable ordering. Public native-v1 create and load use the production
aggregate commands rather than invoking these standalone operations
independently. Composite capture persists the fixed inactive baseline subset
with redacted diagnostics; aggregate restore validates destination
identification and inactive optional state before applying that persisted
subset under one never-run owner-thread admission window.

A separate failure-atomic command reads CPU IRQ then FIQ pending levels. Its
paired owner-thread restore writes the complete typed value in the same order
through a value-free typed failure that reports the exact interrupt type and
completed prefix. The two writes are nontransactional, so failure requires a
complete retry or vCPU discard before execution. Both boot-session forms expose
capture and restore under generalized interrupt admission with individual
IRQ/FIQ operations and GIC PPI mutation. HVF clears both injection levels after
a vCPU run returns, so one apply does not define automatic pre-run reassertion.
The two-field value does not represent distributor, redistributor, CPU-
interface, or device interrupt state and has no routing, delivery/EOI,
persistence, schema, orchestration, or portable restore contract.

Another command captures Hypervisor.framework's stable, versioned opaque GIC
device-state bytes except GIC CPU system registers. State-object creation,
sizing, fallible allocation, data copy, and retained-object release run on the
serialized owner loop. The command shares generalized interrupt admission with
CPU pending and GIC PPI operations, and the current single-vCPU runner enforces
Apple's stopped-VM condition against `hv_vcpu_run`. Both boot-session forms
expose the redacted immutable value and a separate owner-thread setter that
reapplies its exact non-empty pointer/`size_t` before any run has ever been
enqueued. Setter availability is loaded independently from capture, and every
HVF failure retains its original status without exposing bytes. Apple can still
reject an older opaque format after a host software update and publishes no
transactional rollback guarantee, so failure requires destination discard
before execution. The bytes remain opaque but are bounded and persisted inside
the bangbang-native composite schema. Standalone apply still releases its
interrupt admission before returning; native-v1 aggregate restore instead
composes blob, ICC, timers, and pending state in one never-run command. Public
snapshot orchestration uses that aggregate path rather than the standalone
capture or apply.

A companion failure-atomic command captures the ten EL1 ICC CPU-interface
registers exposed by Hypervisor.framework: PMR, BPR0, AP0R0, AP1R0, RPR, BPR1,
CTLR, SRE, IGRPEN0, and IGRPEN1. A paired pre-first-run owner command loads the
independent getter and setter capabilities before mutation, writes the nine
architecturally mutable registers in capture order, and reads the derived,
read-only RPR at its original position to validate equality. The operation is
nontransactional; its typed value-free failure identifies the register, write or
validation operation, completed-write prefix, and backend source. A failure
requires complete retry or vCPU discard before execution. Both commands share
generalized interrupt admission with CPU pending, GIC PPI, and opaque GIC
device-state operations, and both boot-session forms expose the fixed per-vCPU
value. Standalone callers still receive no cross-step lease. Native-v1
composite capture persists both values under one aggregate runner admission
window, and aggregate restore reapplies the opaque blob before ICC under a
matching never-run window. Public snapshot orchestration uses those aggregate
commands rather than the standalone operations. `ICC_SRE_EL2`, ICH/ICV,
cross-host policy, and multi-vCPU association remain future work.

The boot session submits bounded steps to every online idle vCPU through its
owning coordinator and one shared MMIO dispatcher, so each resulting MMIO exit
is handled on the corresponding owner thread without duplicating device state.
A primary-only cancellation handle remains for the explicit size-one
compatibility step, while aggregate execution exposes topology-wide wakeup and
stop control. Public `InstanceStart`
now starts a process-owned internal boot run-loop worker across bounded step windows with retained internal worker status and an owned
HVF boot session plus configured or default internal serial output after successful startup. A
bounded internal
boot-session run-loop pump now composes indexed aggregate steps with boot block,
virtio-net, and virtio-vsock notification dispatch between successful MMIO steps and virtual
timer PPI assertion on the completing vCPU after virtual timer exits. It stops explicitly on a step limit,
stop-token request, canceled run exit, PSCI `SYSTEM_OFF` or `SYSTEM_RESET`
guest request, unknown run exit, dispatch error, or timer handler error. This
remains internal runner-loop plumbing, not the future public guest scheduler.
For process-owned API-enabled and no-api runs, PSCI `SYSTEM_OFF` and
`SYSTEM_RESET` wake the process supervisor and let the process exit
successfully. Non-success terminal worker states wake the same supervisor path
and fail the process with the current coarse process-failure exit status.
An owned internal session handle preserves the same
session operations while avoiding a self-referential backend/session owner in
process-level state.
The boot session can also dispatch pending boot block, virtio-net, virtio-vsock,
and virtio-balloon queue notifications against mapped guest memory and signal
the corresponding block, network, vsock, or balloon SPI line when the runtime
dispatch summary reports queue-interrupt intent; per-device results preserve
dispatch, lookup, and signal failures for later runner-loop policy.
Boot notification dispatch locks the shared dispatcher only while draining
runtime notifications and releases it before HVF GIC signaling.

bangbang now wires `mem_size_mib` and public host-limited `vcpu_count` into
startup preparation. Topology tests inject host-capacity and partial-owner
failures; process/API tests prove those failures retain no session and do not
commit `Running`; signed executable tests prove public CPU0/CPU1 execution and
guest-directed CPU1 off/re-entry. The current scaffold still does not provide
full public run-loop control beyond pause/resume, non-timer PSCI suspend wake,
FDT CPU idle-state discovery, dynamic CPU topology, or full process exit-code
parity for error power actions. Like
Firecracker's aarch64 process boundary, `SYSTEM_RESET` is terminal rather than
an in-process reboot. Public
machine configuration rejects `mem_size_mib` above the current 1022 GiB Apple
Silicon/aarch64 DRAM maximum before storage; startup keeps its architectural
maximum check as a defensive guard. This deliberately differs from pinned
Firecracker, which accepts/echoes a larger request and truncates only the
realized layout. Dynamic host-memory availability is not a reliable preflight
contract with lazy/no-reserve mappings. Exact Firecracker 2-MiB hugetlbfs
backing is a certified public-platform exclusion; ordinary memory allocation,
mapping, protection, alignment, and resource-failure behavior are not.

## API State and Response Policy

The current scaffold implements the first HTTP API behavior for `GET /`,
`GET /version`, `GET /vm/config`, pre-boot `/machine-config` configuration
storage, pre-boot `PUT /boot-source`, `PUT /drives/{drive_id}`, and
`PUT /network-interfaces/{iface_id}`, `PUT /vsock`, `PUT /metrics`,
`PUT /logger`, and `PUT /serial` configuration storage, plus
process-routed `PUT /actions` startup with a bounded internal boot run-loop
worker and runtime metrics flush handling. The
policy below is the compatibility target for future request parsing, VMM action
mapping, state validation, and golden API tests.

The implemented `GET /version` path flows through the minimal VMM action model
as `GetVmmVersion` and returns VMM version data. The implemented `GET /` path
flows through the same boundary as `GetVmInstanceInfo` and returns
Firecracker-shaped instance information. Parsed `/machine-config` requests
flow through `GetMachineConfig`, `PutMachineConfig`, and `PatchMachineConfig`
and read, replace, or partially update stored machine configuration state.
`GET /vm/config` flows through
`GetVmConfig` and returns the supported accumulated configuration subset:
`machine-config`, `boot-source` when configured, the `drives` array, and the
`network-interfaces` array, plus `vsock` when configured.
Observability state such as metrics, logger, and serial output configuration is
omitted. Custom CPU-template contents are also omitted, while an effective
pending static `V1N1` selection is returned as `cpu_template: "V1N1"` in the
machine section. Empty custom input or explicit machine `cpu_template: None`
clears the effective selection. Supported nonempty custom input is retained for
startup; KVM-only or unsupported-register input is rejected without mutation.
Unsupported top-level sections are omitted until their models exist. The implemented pre-boot drive path flows
through `PutDrive` and records validated configuration state. The implemented
pre-boot network-interface path flows through `PutNetworkInterface` and records
validated configuration state without opening host networking resources. Parsed
`/vsock` requests flow through `PutVsock` and replace validated vsock
configuration state without opening host Unix socket resources. Parsed
`/boot-source` requests flow through `PutBootSource` and replace stored
boot-source configuration state. Parsed `/metrics` requests flow through
`PutMetrics` and initialize per-process metrics output state that is not part of
guest configuration. Parsed `/logger` requests flow through `PutLogger` and
initialize or update per-process logger configuration state that is not part of
guest configuration. Parsed `/serial` requests flow through `PutSerial` and
store pre-boot serial output configuration that is also omitted from
`GET /vm/config`. Parsed `/actions` requests flow through `InstanceStart` and
`FlushMetrics` VMM actions. `InstanceStart` first validates stored boot-source
and state preflight, then the process VMM owner prepares and starts an owned
HVF boot-session worker with the configured serial output path or the default
internal serial MMIO capture buffer. It marks the instance `Running` only after
the bounded internal worker handle is retained; `FlushMetrics` fails before
startup, then succeeds after startup and writes one minimal JSON line only when
metrics output was configured. Configuration itself is silent; one
best-effort initial attempt follows session retention, periodic output runs
every 60 seconds in Running and Paused, and normal convergence makes one
best-effort final attempt. These automatic paths do not route through
`/actions`; explicit `FlushMetrics` remains caller-visible and fallible.

### Initial API State Model

The first API implementation should model the same broad stages as Firecracker:

- pre-boot: configuration requests are accepted and stored before guest
  execution starts
- starting: `PUT /actions` with `InstanceStart` validates the accumulated
  configuration, prepares the owned HVF startup session with configured or
  default internal serial output, and transitions the process out of pre-boot
  state on success
- runtime: the microVM is running; pre-boot-only configuration requests should
  fail with a Firecracker-shaped unsupported-state error
- paused/resumed: `PATCH /vm` supports `Paused` and `Resumed` for the current
  process-owned boot-worker run-loop by pausing scheduling between bounded
  run-loop windows; same-state requests succeed without another backend command
  while still requiring the retained process session

### Initial Operation State Matrix

| Operation | Pre-boot behavior | Runtime behavior | Notes |
| --- | --- | --- | --- |
| `GET /` | implemented; `200` JSON | implemented; `200` JSON | Response state reflects the current microVM state. |
| `GET /version` | implemented; `200` JSON | implemented; `200` JSON | Body uses Firecracker's `firecracker_version` field shape. |
| `GET /vm/config` | implemented; `200` JSON | implemented; `200` JSON | Returns the accumulated supported configuration subset, including an always-present `pmem` array that is populated after successful pre-boot pmem configuration, `mmds-config` after successful MMDS config storage, `entropy` after successful entropy configuration, `memory-hotplug` after successful pre-boot memory hotplug configuration, and `balloon` after successful pre-boot balloon configuration. Startup applies the supported boot subset to an owned HVF session and internal boot run-loop worker across bounded step windows. |
| `GET /machine-config` | implemented; `200` JSON | supported target; `200` JSON | Returns the stored/default machine configuration. |
| `PUT /machine-config` | implemented; `204` empty response on successful config storage | unsupported after start; `400` `fault_message` | Pre-boot-only configuration. Representable invalid values return typed VMM faults. Accepted vCPU/memory state is applied exactly during startup; host vCPU capacity and ordinary memory allocation remain startup checks. |
| `PATCH /machine-config` | implemented; `204` empty response on successful partial config update | unsupported after start; `400` `fault_message` | Pre-boot-only partial configuration. Omitted/null fields preserve current stored values; empty/null-only candidates use `Empty PATCH request.`; invalid updates leave machine and balloon state unchanged. |
| `PUT /boot-source` | implemented; `204` empty response on successful config storage | unsupported after start; `400` `fault_message` | Records validated pre-boot config. Direct host paths open during startup preparation; contained grant tags claim exact read-only kernel/initrd descriptors during the successful request and move them into startup without reopening the tags. Host path and grant errors avoid leaking sensitive values. |
| `PUT /drives/{drive_id}` | implemented; `204` empty response for valid regular/block-special file-backed or direct/contained socket-backed pre-boot config | PCI-only non-root file-backed and eligible direct/contained vhost runtime attach implemented; `204` on success | File-backed requests accept an exact regular file or macOS block-special descriptor, retain optional limiters, deferred direct opening or atomic BBG2 grants, selected-transport startup, and transactional PCI runtime attachment. Direct block geometry/cache sync uses public ioctls; contained block-special control is restricted to fresh inspect/cache-sync on the launcher's retained exact grant descriptor. Same-ID file-backed PUT can atomically replace regular/block kinds, engine, cache policy, capacity/config, and backing-derived GET_ID. Socket-backed requests use the strict vhost field matrix and separate contained connector facet. In Running or Paused all-PCI state, a new non-root file or vhost ID attaches only after owner profile/capacity/authority preflight and commits public config after endpoint publication. Default MMIO, root, duplicate, anonymous/shared-profile, invalid backing/access/geometry/grant/socket/negotiation, unavailable session, and capacity exhaustion preserve state. Vhost same-ID PUT remains duplicate rejection. DELETE releases exact owners for same-ID/slot reuse; manual guest PCI rescan/removal is required. Native-v1 remains regular-only. |
| `PUT /pmem/{id}` | implemented; `204` empty response on successful config storage | implemented in Running/Paused public PCI; `204` after live commit | Pre-boot requests record Firecracker-shaped config and replace prior config for the same ID failure-atomically; empty or all-null limiter objects are unconfigured and valid bandwidth/ops buckets are stored and reported. One pmem or ordinary block root is accepted; pmem order supplies `/dev/pmem<i>` and `ro`/`rw`, and cross-family or second-root failure precedes grant/config mutation. Direct pre-boot paths remain unopened until startup; contained grant tags claim and retain exact-ID, exact-access nonzero regular-file descriptors. Startup moves provided descriptors or opens missing direct paths, maps each file/private-tail range once, registers a cloned lease directly with HVF, attaches the selected transport, and retains every owner. After startup, public PCI accepts only a new non-root ID. The owner preflights every shared and pmem-specific capacity before direct open/map or contained grant claim; only then does candidate preparation, exact mapping, endpoint publication, public configuration commit, and grant consumption proceed. Exact-prefix `MS_SYNC`, read-only guest protection, recoverable rollback, and failed-unmap lease retention preserve the prior projection and authority; incomplete cleanup is terminal. Default MMIO rejects runtime insertion before path use. Capture-ready traversal and direct/contained aggregate signed guests certify exact live pmem state alongside the other storage classes; only the two named Wave 6 pmem serialization/restore composites remain deferred. |
| `PATCH /pmem/{id}` | recognized post-boot-only operation; `400` `fault_message` | runtime rate-limiter updates supported; `204` empty response on successful no-op, replacement, or clear | Parses Firecracker-shaped pmem rate-limiter updates, rejects malformed or mismatched bodies first, returns unsupported-state before startup, and validates the exact active pmem ID after startup. Omitted, `null`, empty, or all-null limiters are no-ops; present buckets replace, clear, or preserve individual live and stored buckets under shared update rules. Handler or owner-thread delivery failures do not commit stored configuration, and a replacement that unblocks pending work schedules an immediate bounded retry. |
| `GET /hotplug/memory`, `PUT /hotplug/memory`, `PATCH /hotplug/memory` | `PUT` stores validated pre-boot config; `GET` reports configured or active status; `PATCH` is unsupported-state before start | Implemented supported runtime subset; post-start `GET` reports exact requested/plugged size and `PATCH` changes requested size | Startup attaches one virtio-mem endpoint over the selected startup transport with the configured block and region shape. Active queue handling validates complete block ranges, applies exact block-owned HVF map/unmap work before ACK, supports split and combined unplug, commits device state only after guest-visible completion, and rolls partial or late failures back in reverse order. Signed Linux coverage proves the public lifecycle `0 -> 128 MiB -> 0`. Runtime device deletion, broader public guest-memory accounting, optional-device snapshots, and Firecracker KVM slot identity remain excluded. |
| `PATCH /drives/{drive_id}` | recognized post-boot-only operation; `400` `fault_message` | supported target; `204` on successful file update/no-op or ID-only vhost refresh | Parses Firecracker-shaped updates and routes valid bodies through `UpdateBlockDevice`. Ordinary drives retain backing replacement, contained exact-grant transfer, per-bucket limiter mutation, success-last stored config, and backend-neutral retry scheduling. A vhost drive rejects path or limiter fields before backend I/O; an ID-only body instead fetches and validates one exact 60-byte active CONFIG reply, publishes it through the selected MMIO/PCI transport, increments one generation, and delivers one configuration interrupt. Acquisition or confirmed pre-delivery failure preserves old config/generation and records `update_fails`; success records `update_count` plus optional latest-value `config_change_time_us` for aggregate and drive generation. Delivery ambiguity is terminal. Parser/state/unknown-drive failures remain outside block-attempt metrics, while an ordinary pathless no-op remains unreported. The limiter path still does not claim Linux timerfd/eventfd implementation identity. |
| `DELETE /drives/{drive_id}`, `DELETE /pmem/{id}`, `DELETE /network-interfaces/{iface_id}` | recognized bodyless hot-unplug; `400` `fault_message` | PCI-only supported-device DELETE implemented | Bodyless requests route through one `HotUnplugDevice` VMM action. Pre-boot requests return the normal unsupported-state fault. In a Running/Paused public PCI session, each route removes an existing endpoint only after manual guest removal and commits configuration last. Regular/block-special file drives release queue/retry, Sync/Async generation, backing, direct-or-broker control, metrics, and PCI owners; vhost block also closes frontend/protocol, notifier pipes, wakeup visibility, and cloned shared-memory descriptors. A contained vhost DELETE additionally releases its exact child lease while retaining the session's directory authority, allowing later same-ID reinsertion through a fresh stream. Pmem synchronizes its exact prefix and unregisters its direct mapping lease. Network coordinates reversible PCI teardown with exact packet-I/O take/stop, then releases queue, retry, metrics, MMDS/vmnet, and PCI ownership. Recoverable preparation failures restore the prior endpoint; failed pmem unmap retains its mapping owner, while incomplete restoration, uncertain cleanup, or post-boundary corruption is terminal. Default MMIO, root block/pmem, and missing-device requests are nonmutating. Requests with a body fail first as malformed request shape. |
| `PUT /network-interfaces/{iface_id}` | implemented; `204` empty response on successful config storage | implemented in Running/Paused public PCI; `204` after live commit | Records up to 16 validated pre-boot configs without opening host networking resources. Startup attaches them over the selected transport and chooses all-MMDS or vmnet packet I/O. After startup, public PCI accepts only a new validated ID/MAC: the owner thread selects an independent entry class from immutable startup/MMDS policy, checks contained mode/bridge/actual-vmnet count, prepares packet I/O, publishes generation-bound metrics and the PCI endpoint, then commits live configuration. Existing interfaces retain their queues and host state. Default MMIO, duplicate ID/MAC, invalid host config, capacity, authority, and injected transaction failures preserve the prior projection; uncertain cleanup is terminal. Guest PCI rescan is manual. Signed direct and networkless-production guests prove two MMDS attach/exchange/sysfs-remove/DELETE rounds and exact slot reuse without vmnet authority. External vmnet connectivity, snapshots, and automatic guest notification remain excluded. |
| `PATCH /network-interfaces/{iface_id}` | recognized post-boot-only operation; `400` `fault_message` | runtime rate-limiter updates supported; `204` empty response on successful no-op, replacement, or clear | Parses Firecracker-shaped update requests, rejects malformed or mismatched bodies first, returns unsupported-state before startup, and validates that the target interface already exists after startup. Omitted, `null`, empty, or all-null directions are no-ops. In `Running` or `Paused`, configured RX/TX bandwidth and ops buckets update the active startup or hotplugged device before stored config commit; omitted inner buckets preserve existing stored config and exact live budget, enabled buckets start fresh and full at one shared update instant, and explicit disabled buckets clear only their target. Failures preserve stored configuration. Queue state, pending-work flags, config generation, and interrupt status remain unchanged, and later retained work is scheduled from the updated live limiter state. Limiter-specific metrics and snapshots remain deferred. |
| `PUT /vsock` | implemented; repeated valid requests replace stored config and return `204` | unsupported after start; `400` `fault_message` without mutation | Implements the supported live MMIO-or-PCI startup/Unix-socket subset. Direct requests leave the ordinary path unopened until startup. Contained requests atomically claim an exact `VsockSocketDirectory` plus safe child, retain scope/anchor without reopening the tag, and preserve prior public/private state on rejection. Startup binds a direct listener or exclusively publishes the supplied granted listener, attaches three 256-entry queues, and activates guest-/host-initiated connection handling with bounded handshakes/backlogs, dynamic 64-KiB wrapping-counter credit windows, partial/full shutdown, two-second request/shutdown cleanup, reset/error handling, `EVENT_IDX`, and no-op event notifications. Contained host initiation uses the supplied main listener; guest initiation uses only the fixed session-bound launcher port connector and connected-fd response, without guest payload brokerage or `network.client`. Signed cases verify deterministic bidirectional streams, write-half-close/EOF, cleanup, redaction, two-stream isolation, outside-container API/vsock publication, and no steady-state helper or entitlement change. Indirect descriptors are a supported bangbang extension. There is no PATCH, DELETE, runtime hotplug, broader CID routing, full event payload, or general performance/artifact contract; #543 owns native-v1 snapshot UDS override, `TRANSPORT_RESET`, and post-restore RX gating exclusions. |
| `GET /mmds` | implemented; `200` JSON | implemented; `200` JSON when the MMDS store exists, `400` `fault_message` when the store is absent | Returns the current process-local MMDS JSON object. Before startup, `GET /mmds` creates the MMDS store when absent and returns JSON `null` until data is initialized. After startup, `GET /mmds` requires an existing store: it returns JSON `null` for a present-but-uninitialized store and returns the Firecracker-shaped MMDS not-initialized fault when no pre-start MMDS action created the store. Initialized data is also used by the implemented guest-visible MMDS path when MMDS config selects startup network interfaces; guest-visible queries still fail if the data store value is uninitialized. Packet handling remains limited to the documented internal vmnet detour boundary. |
| `PUT /mmds` | implemented; `204` empty response on successful data storage | implemented only when the MMDS store already exists; otherwise `400` `fault_message` | Stores a JSON object in the process runtime using the effective MMDS data-store limit. Pre-start requests that parse successfully and reach the VMM action create the MMDS store before validating and storing data. Runtime requests require a pre-existing store, matching Firecracker's runtime MMDS handle check. Oversized data is rejected without replacing the previous value. |
| `PATCH /mmds` | implemented after data initialization; `204` empty response | implemented after data initialization; `204` empty response | Applies RFC 7396 merge-patch semantics to the stored JSON object using the effective MMDS data-store limit. Pre-start requests that parse successfully and reach the VMM action create the MMDS store before applying the patch, but patching still requires initialized data. Runtime requests return the same MMDS not-initialized fault when the store is absent or the store exists without initialized data. Oversized patched results are rejected without mutating the previous value. |
| `PUT /mmds/config` | implemented; `204` empty response on successful config storage | unsupported after start; `400` `fault_message` | Stores control-plane MMDS config before startup after runtime validation rejects empty interface lists and validates that each listed interface ID already exists in the configured network interface set. A successful config request creates the process-local MMDS store even when no data has been initialized. At startup, the configured interfaces can enable the implemented guest-visible MMDS packet path; runtime MMDS config updates and public packet movement remain deferred. |
| `PUT /metrics` | implemented; `204` empty response on successful output initialization | unsupported after start; `400` `fault_message` | Process observability state, omitted from `GET /vm/config`. Duplicate initialization and identifiable malformed requests are counted without replacing the sink; duplicate state is rejected before a contained grant claim. Configuration writes nothing. In contained mode an exact metrics-sink reference claims one `WriteOnly` regular-file descriptor, normalizes append/nonblocking status without reopening it, and retains it for the same initial, 60-second Running/Paused periodic, explicit fallible `FlushMetrics`, and best-effort terminal transaction/schema behavior. Direct paths retain current create/FIFO behavior. |
| `PUT /logger` | implemented; `204` empty response on successful pre-boot configuration | unsupported after start; `400` `fault_message` | Process observability state, omitted from `GET /vm/config`. Repeated pre-boot requests update provided fields. A contained path-bearing request claims an exact singleton `WriteOnly` logger-sink descriptor and atomically installs the adopted append/nonblocking sink plus requested fields; a path-free request retains the current sink and claims nothing. Direct paths retain current create/FIFO behavior. Unrestricted API method/path and action records omit bodies; bounded boot-timer records use suppression recovery. Filters apply before delivery, and sink misses never change functional results. No sink is configured by default. |
| `PUT /serial` | implemented; `204` empty response on successful pre-boot output configuration, rate-limiter configuration, or clear request | unsupported after start; `400` `fault_message` | Serial output is process observability state, not guest configuration. Direct valid `serial_out_path` values and token-bucket `rate_limiter` values are stored without opening host resources during the request; startup opens the path, wraps the configured or default output in the limiter when enabled, and routes guest TX serial bytes to it. A contained exact serial-sink reference instead adopts and retains one `WriteOnly` append/nonblocking regular-file descriptor; clear/replacement drops it, and startup moves it once without reopening the reference. A later startup failure leaves the grant consumed until validated serial reconfiguration. Malformed parser/input/grant failures preserve previous public and private state. |
| `PUT /entropy` | implemented; `204` empty response on successful configuration | unsupported after start; `400` `fault_message` | Stores virtio-rng configuration before startup, including valid configured `bandwidth` and `ops` rate-limiter buckets. `GET /vm/config` includes `"entropy": {}` for unconfigured limiters or an entropy `rate_limiter` object for configured buckets. `InstanceStart` attaches the existing HVF virtio-rng endpoint over the selected startup transport backed by the session-owned host OS randomness source and enforces the configured limiter in queue dispatch. |
| `PUT /balloon` | implemented; `204` empty response on successful pre-boot configuration | unsupported after start; `400` `fault_message` | Stores the complete Firecracker-shaped balloon configuration before startup, rejects targets larger than configured guest memory without mutating previous machine/balloon state, exposes exact committed state through `GET /balloon` and `GET /vm/config`, and attaches the endpoint over the selected startup transport. Runtime target and nonzero polling updates, required and optional statistics, hint start/automatic acknowledgement/explicit stop, reporting, and metrics are implemented. Inflate/deflate prepare compact paired PFN accounting before used publication and commit by move afterward. A paused supervisor transaction captures bounded validated device/queue/statistics/hint/accounting state through exactly one MMIO or PCI owner without serializing it. Signed Linux evidence covers live MMIO/PCI inflate, polling, optional fields, hinting/reporting, pause/capture/resume, zero-target convergence, and cleanup. Darwin discard remains best effort and does not promise synchronous RSS reduction; Wave 6 owns balloon encoding and restore. |
| `GET /balloon/hinting/status` | post-boot-only unsupported-state fault; `400` `fault_message` | implemented; `200` JSON with `free_page_hinting: true`, otherwise `400` `fault_message` | Requires a configured balloon with free-page hinting enabled and returns the active host command and guest command state. Start/stop commands update `host_cmd`; a 4-byte hinting queue descriptor updates `guest_cmd`, which remains `null` until the guest sends one. Guest `STOP(0)` and unexpected guest `DONE(1)` descriptors complete the current hinting run and, when the active run was started with `acknowledge_on_stop=true`, update `host_cmd` to `DONE(1)` through the same config-space/config-interrupt path as explicit stop. Accepted current-command ranges are validated and discarded best effort on Darwin; stale/inactive ranges remain ignored. |
| `PATCH /balloon/hinting/start`, `PATCH /balloon/hinting/stop` | post-boot-only unsupported-state fault; `400` `fault_message` | implemented; `204` with `free_page_hinting: true`, otherwise `400` `fault_message` | Start advances the host command id, skips Firecracker reserved command values, updates active config space, raises a config interrupt, and preserves `acknowledge_on_stop` in host-owned state. Stop writes Firecracker's done command, updates active config space, and raises a config interrupt. Hinting queue command acknowledgements can update `guest_cmd`, completed guest `STOP(0)`/`DONE(1)` commands automatically write host `DONE(1)` when `acknowledge_on_stop` is enabled, and accepted active-run ranges use best-effort Darwin discard. |
| `PUT /actions` with `InstanceStart` | process-routed; `204` after successful owned HVF startup or `400` preflight/preparation fault | unsupported after start; `400` `fault_message` | Commits `Running` after retaining the worker with configured serial TX or bounded internal capture. API/action logger delivery is best effort and cannot replace the startup result. The worker retains active, paused, terminal-outcome, or error status; guest PSCI `SYSTEM_OFF` or `SYSTEM_RESET` can terminate the owner successfully. Public serial RX/streaming and run-loop control beyond pause/resume are absent. |
| `PUT /actions` with `FlushMetrics` | VMM-routed; `400` unsupported-state `fault_message` | implemented; `204` empty response or `400` metrics output fault | Runtime-only explicit action. An unconfigured sink is a no-op; success writes one interval/store line; failure is returned while preserving the previous-success baseline. Parsed request and successful action logger records are best effort. Automatic initial, periodic, and terminal attempts share the payload transaction but create no `/actions` counter or action record. |
| `PUT /actions` with `SendCtrlAltDel` | intentionally unsupported; parser returns `400` `fault_message` | intentionally unsupported; `400` `fault_message` | Firecracker rejects this on aarch64; bangbang's first target is Apple Silicon. The request contributes to `put_api_requests.actions_count` but not `actions_fails`. |
| Non-initial endpoints from the endpoint matrix | `400` `fault_message` until their capability exists | `400` `fault_message` until their capability exists | Covers planned later and deferred endpoints; a later capability PR may define more specific state behavior. |
| Unknown endpoint or invalid method/path | `400` `fault_message` | `400` `fault_message` | Matches Firecracker's parser-level invalid path or method handling. Bodyless `PUT` and bodyless `PATCH` requests on unsupported paths or methods generally use Firecracker's method-level empty request faults without accepting the route; empty-body-compatible balloon hinting routes keep their route-specific behavior. |

### Response Policy

| Case | HTTP status | Body policy |
| --- | --- | --- |
| Successful data response | `200 OK` | JSON body with Firecracker-shaped field names. |
| Successful empty response | `204 No Content` | Empty body. |
| Invalid path, invalid method, invalid JSON, unknown field, invalid field, unsupported endpoint, or unsupported state | `400 Bad Request` | JSON object with `fault_message`. |
| Startup, configuration, or VMM action failure | `400 Bad Request` | JSON object with `fault_message`; exact strings can be refined with the implementation. |
| HTTP API request payload-limit failure | `413 Payload Too Large` | JSON object with `fault_message`. |
| MMDS data-store size-limit failure | `400 Bad Request` | JSON object with `fault_message`; this is a semantic data-store limit failure rather than an HTTP request-size parser failure. |

Future API work should use `fault_message` consistently where Firecracker does.
Exact message strings should be covered by golden tests once the API parser and
VMM action model exist, but this document only defines the initial status/body
shape.

The initial API implementation uses Firecracker's default `51200` byte HTTP
request payload limit unless `--http-api-max-payload-size <BYTES>` configures a
different per-process API socket body limit. The configured value applies to the
body declared by `Content-Length`, not to the request line and headers; bangbang
keeps request-head bytes bounded by a separate parser safety cap.
The MMDS data store uses the effective `--mmds-size-limit <BYTES>` value as its
serialized JSON limit. When that argument is omitted, the limit follows the
effective HTTP API payload limit like Firecracker; with default HTTP settings
this remains `51200` bytes. Startup `--metadata <PATH>` initializes the same
data store before API serving or no-api readiness and is subject to the same
serialized JSON limit after its file is parsed.
Internal MMDS guest GET response modeling checks the configured MMDS v2 token
requirement before reading metadata. Once a request is permitted to read
metadata, it follows the same uninitialized data policy: before `PUT /mmds`, it
returns a process-local `400` plain-text error value rather than a successful
response. Process-local guest request parsing currently accepts complete
HTTP/1.0 or HTTP/1.1 `GET` request buffers and
`PUT /latest/api/token` token request buffers, rejects request bodies and
transfer encodings, maps GET `Accept: application/json` to JSON output, and
defaults missing, empty, wildcard, or `text/plain` GET `Accept` headers to IMDS
text output. The runtime can also convert complete process-local guest HTTP
request buffers into deterministic response bytes, mapping unsupported methods
to `405 Method Not Allowed` with the current `Allow: GET, PUT` header and other
parse failures to `400 Bad Request` plain-text responses without echoing
malformed request bytes. The runtime also has a process-local opaque token
authority with Firecracker-compatible TTL bounds of `1..=21600` seconds and a
default `1024`-entry active-token store. Process-local guest token
`PUT /latest/api/token` handling requires either
`X-metadata-token-ttl-seconds` or `X-aws-ec2-metadata-token-ttl-seconds`,
rejects `X-Forwarded-For`, and returns a plain-text token response with the
accepted TTL header. When configured for MMDS v2, process-local guest GET
handling requires exactly one valid `X-metadata-token` or
`X-aws-ec2-metadata-token` value generated by the token authority; missing,
duplicate, unknown, or expired tokens return `401 Unauthorized`.
The runtime can also classify ARP requests for the configured MMDS IPv4 address
and raw Ethernet/IPv4/TCP guest packet bytes as MMDS candidates only when the
IPv4 destination matches the configured MMDS address and the TCP destination
port is `80`. Truncated, malformed, non-IPv4, non-TCP, fragmented, and non-MMDS
packets are treated as non-candidates without exposing metadata. For pure
empty-payload candidate TCP SYN packets, the runtime can synthesize
deterministic SYN-ACK frames, identify pure empty-payload ACK-only packets that
acknowledge that deterministic SYN-ACK, FIN close, guest packets carrying RST,
and unsupported control packets, synthesize ACK plus FIN-ACK frames for empty FIN
close packets, synthesize minimal RST frames for unsupported empty controls,
consume guest RST packets without response even when they also carry payload
bytes, and for non-empty candidate TCP payloads that acknowledge that
deterministic SYN-ACK and do not carry unsupported SYN or FIN payload control
flags, it can also produce the same process-local HTTP response bytes as the
existing guest HTTP helper, including token PUT and MMDS v2 GET token
enforcement. Non-empty candidates carrying SYN or FIN are not interpreted as
process-local MMDS HTTP requests. The process vmnet packet I/O path detours MMDS
ARP requests, pure empty-payload MMDS SYN packets, pure empty-payload MMDS
ACK-only packets that acknowledge bangbang's deterministic SYN-ACK, pure
empty-payload MMDS FIN close packets, guest packets carrying RST, unsupported
empty control packets, and non-empty MMDS candidate TX payloads on configured
MMDS interfaces when they acknowledge bangbang's deterministic SYN-ACK and do
not carry unsupported SYN or FIN payload control flags. MMDS data remains
shared between the API and packet paths, while every configured interface's
detour owns a separate split-request buffer collection and response queue. The
detour buffers split request headers only when each fragment starts at the next
expected TCP sequence number, rejects non-contiguous buffered fragments without
forwarding them to vmnet,
synthesizes deterministic Ethernet/ARP replies, Ethernet/IPv4/TCP SYN-ACK
frames, minimal Ethernet/IPv4/TCP FIN close frames, minimal Ethernet/IPv4/TCP
RST frames, and Ethernet/IPv4/TCP response frames carrying the generated HTTP
response bytes, retains those frames in bounded per-interface queues, exposes
queued frames through the matching
virtio-net RX source before vmnet reads, prioritizes ARP replies before queued
TCP responses, and schedules one bounded post-TX RX retry when that source
reports a queued response. The same path records Firecracker-shaped top-level
`mmds` metrics for implemented guest packet acceptance, queueing failures, V2
token rejection, response delivery, and connection lifecycle events. When every
configured network interface is selected
by MMDS config, process startup can instead build process-local MMDS-only packet
I/O that reuses the same detour and response-queue logic, drops non-MMDS TX
frames, and serves queued MMDS responses without opening vmnet. A focused
two-entry test sends one TCP tuple's request fragments through different
interfaces and proves neither buffered state nor queued responses cross the
provider boundary. A signed guest case then completes the same shared metadata
fetch through two MAC-selected MMDS-only interfaces with distinct fixed
markers. Full ARP cache
management, gratuitous ARP, ARP
timeout/retry policy, broader ACK-number validation beyond the narrow ACK-only
and non-empty payload SYN-ACK acknowledgement paths, full TCP stream tracking,
out-of-order reassembly, retransmission policy, stateful RST policy, session
timeout policy, and broader per-interface TCP session state beyond the current
split-request buffers remain deferred.
Process-local guest response-byte serialization preserves accepted `HTTP/1.0`
or `HTTP/1.1` request versions in response status lines. Malformed request
lines and unsupported versions use the existing safe parse-error response path
without echoing arbitrary version tokens.
Invalid path and method errors use the Firecracker `fault_message` body shape
but intentionally avoid echoing path-like request values.
The initial blocking API server also uses a short per-connection timeout so an
incomplete request cannot hold the single server loop indefinitely.

API request bodies, path identifiers, and host resource paths are untrusted
input. Future implementations must validate them before mutating VMM state and
redact sensitive host path details from error messages. API parsing and response
serialization must stay outside the VM and vCPU fast path; expensive startup,
memory, or device work belongs in explicit VMM actions where it can be measured
and tested.

## Aggregate Storage Closure

The checked
[Firecracker v1.16.0 storage contract](../compat/firecracker/v1.16.0/storage-contract.md)
owns exactly 40 block, vhost-user, and pmem identities. Thirty-eight are
`implemented-and-verified`; only `corpus:pmem` and
`semantic.storage:pmem-root-mapping-flush-and-state` remain
`audit-required`, both assigned to Wave 6 optional-device serialization and
restore. Parser recognition, config echo, or a family-level test alone is not
enough to change those dispositions; the ledger records implementation and
validation per identity.

Two signed product-PCI profiles compose the live contract in one VM. Both boot
a read-only Sync root plus writable Sync, portable Async, vhost-user, pmem, and
virtio-mem devices; discover devices from on-media markers rather than fixed
startup BDFs; prove initial and continuing I/O; apply disjoint concurrent
block, pmem, and vhost updates; replace Async storage while paused; complete
memory `0 -> 128 MiB -> 0`; and serialize dynamic block and pmem
attach/remove/reuse so Linux observes exact slot or pmem-resource reuse. The
direct profile additionally proves terminal backend death and process cleanup.
The production profile uses only exact initial grants and authorized vhost
children, resists pathname replacement, leaves entitlements unchanged, and
proves orderly frontend/session/helper cleanup.

This is a cooperative live-storage contract. PATCH candidate preparation,
Async-generation quiescence, owner publication, grant commit, and public
configuration are failure-atomic, but already admitted I/O may cross an
operator-controlled pause/update boundary. Runtime pmem insertion performs
root/duplicate, shared endpoint, pmem inventory, PCI function, BAR, MSI-X,
dispatcher, and metrics capacity checks before a contained grant is claimed or
a direct backing is opened and mapped. Vhost-user backends remain trusted for
the complete immutable boot-RAM/virtio-mem aperture and own their caching,
limiting, health, and resource policy. Pmem DAX selection, page-cache/RSS
behavior, page faults, huge-page realization, eviction, same-backing sharing,
side channels, and throughput remain deployment measurements rather than
portable Firecracker-Linux promises.

## Non-Initial Firecracker Features

The following Firecracker features are outside the first compatibility tier.
Their eventual support level should follow the endpoint matrix:

- packet networking beyond the implemented supported virtio-MMIO/MMDS-only
  subset, including direct-vmnet start-parameter reconciliation, asynchronous
  RX readiness, entitled guest connectivity, host firewall/resource policy,
  limiter-specific metrics, network snapshot state, and PCI attach/remove
- virtio-vsock behavior beyond the **implemented supported live
  virtio-MMIO/Unix-socket subset**. The live subset includes repeatable pre-boot
  PUT with stable post-start rejection, guest/host connection setup, dynamic
  64-KiB wrapping-counter credit windows, bounded directional buffers, 256
  connections per direction, partial/full shutdown, two-second request/shutdown
  cleanup, `EVENT_IDX`, no-op event notifications, process-local listener
  ownership/redaction, ≥1-MiB signed bidirectional streams, and multistream
  isolation; indirect descriptors are a supported bangbang extension. Outside
  the tier are PATCH, DELETE, runtime hotplug, broader CID routing, full event
  payload dispatch, general performance/Firecracker artifact parity, and
  PCI/vhost/KVM transports. Native-v1 snapshot UDS override, event-queue
  `TRANSPORT_RESET`, and post-restore RX gating remain the stable #543
  exclusions rather than live snapshot-compatibility claims
- snapshot behavior beyond the implemented narrow native-v1 profile, including
  optional-device state, mutable VMClock restore/signaling, Diff artifacts,
  overrides, Firecracker artifact compatibility, and cross-host portability
- full MMDS TCP routing, stream reassembly, and retransmission policy
- balloon producers outside the implemented queue/discard/reporting activity
  and serialized/restored balloon state; live paired PFN accounting and
  capture-ready ownership are implemented, while absent guest statistics are
  not emitted as synthetic zero fields
- pmem dirty-range tracking and optional-device snapshot serialization/restore
  for the two exact Wave 6 composite records, beyond the aggregate-certified
  live state, direct file-backed HVF mapping, deterministic
  root boot, targeted exact-prefix flush, per-event bandwidth/ops limiting,
  retained retry, runtime limiter replacement, guest-visible MMIO/FDT or PCI
  attachment, transactional non-root PCI PUT/DELETE, direct mapping teardown,
  and signed guest root/read/write/flush/reuse proof
- full Firecracker active timerfd/eventfd rate-limiter wakeup parity beyond the
  current HVF block, PMEM, network, and entropy retry schedulers, including shared
  event-source behavior
- serial input/stdin, default stdout, public streaming, and read/flush
  producers beyond the implemented TX output path; native-v1 captures default
  UART registers but not its output buffer, path, limiter state, or counters
- process-global panic/fatal observability durability and production rotation,
  syslog, journald, tracing, or remote telemetry; the implemented logger and
  sparse interval metrics schema do not fabricate absent records or devices
- memory hotplug beyond the implemented block-granular virtio-MMIO lifecycle,
  including runtime device deletion, broader public guest-memory accounting,
  serialized/restorable optional-device snapshot state, and Firecracker's KVM
  slot mechanism
- complete HVF vCPU state capture/restore beyond the current one-vCPU native-v1
  aggregate, and generic snapshot-ready ownership for optional devices or
  multi-vCPU artifacts beyond the topology-wide pause barrier, four-scheduler
  transaction, and external-buffer exclusion
- runtime device attach/remove behavior beyond implemented in-place updates and
  stable unsupported paths

Non-initial features should be introduced through narrower capability work that
covers behavior, validation, documentation, security, and performance together.

## macOS and HVF Differences

Firecracker targets Linux/KVM. bangbang targets macOS with Apple's
Hypervisor.framework. Some Firecracker host mechanisms therefore need explicit
macOS design work instead of direct implementation:

- KVM-specific VM and vCPU operations need HVF equivalents rather than direct
  KVM ioctl usage.
- HVF guest RAM is mapped with a backend-owned owner that holds the selected
  anonymous-private or descriptor-backed-shared host allocation until unmap or
  VM destruction. Startup can load payloads into
  that memory and run the internal boot worker across bounded step windows; full
  run-loop control beyond pause/resume remains deferred.
- HVF vCPU handles are thread-affine: creation, register access, run, and
  destroy operations must happen on the owning thread. The current vCPU wrapper
  covers current-thread lifecycle, typed exit surface, narrow register access,
  single resolved MMIO exit dispatch/completion, and the single primary arm64
  Linux boot-register setup. The current runner skeleton creates a vCPU on a
  dedicated thread, applies that boot-register setup on the owning thread before
  the first run, can capture a detached X0-X30, PC, and CPSR subset through one
  owner-thread command, can reapply that typed value in architectural order
  through a nontransactional owner-thread restore operation, and can capture a
  separate raw SP_EL0, SP_EL1, ELR_EL1,
  and SPSR_EL1 subset through another command in the same core-register
  admission domain. It can also reapply that complete typed system-register
  value in capture order through another nontransactional owner-thread
  operation. A third command captures baseline Q0-Q31, FPCR, and FPSR
  state under that admission, retaining every 128-bit Q value, and can reapply
  the complete typed value in capture order through a nontransactional
  owner-thread operation whose SIMD setters cross one target-gated C ABI shim;
  it defines no SVE/SME alias ordering or destination validation. A fourth
  captures raw TPIDR_EL0, TPIDRRO_EL0, and TPIDR_EL1 values while keeping
  TPIDR2_EL0 in the separate SME system-register subset, and can reapply the
  complete typed value in capture order without validating guest pointers or
  composing wider software-context state; and a fifth captures
  raw SCTLR_EL1, TTBR0_EL1, TTBR1_EL1,
  TCR_EL1, MAIR_EL1, AMAIR_EL1, and CONTEXTIDR_EL1 translation state and can
  reapply the complete typed value in capture order without providing table
  memory, validation, barriers, maintenance, or a safe MMU transition sequence.
  A sixth captures raw AFSR0_EL1, AFSR1_EL1, ESR_EL1, FAR_EL1, PAR_EL1, and
  VBAR_EL1 exception state and can reapply the same complete typed value in
  capture order through a nontransactional owner-thread operation. A seventh
  captures raw ACTLR_EL1 and CPACR_EL1
  execution controls, requiring macOS 15 for ACTLR.EnTSO, and can reapply the
  complete typed value in capture order without defining feature validation or
  guest ISB transition policy. An eighth captures
  five 128-bit pointer-authentication keys from all ten APIA/APIB/APDA/APDB/APGA
  halves, redacts them from `Debug`, and can reapply the complete typed value in
  the same low/high capture order without feature/destination validation,
  protected persistence, zeroization, or SCTLR enable ordering. A ninth captures guest-visible MIDR,
  MPIDR, PFR0/1, DFR0/1, ISAR0/1, and MMFR0/1/2 as raw virtual-CPU/HVF
  compatibility inputs. A tenth captures raw MDCCINT_EL1 and MDSCR_EL1 and can
  reapply the complete typed pair in capture order without defining writable-bit,
  destination, or wider debug-policy validation. An eleventh captures raw
  CSSELR_EL1 as cache-size selection state, not cache topology. A twelfth reads
  DFR0 first and captures only the implemented hardware-breakpoint value/control
  pairs as sensitive observation-only state. A thirteenth reads DFR0 first and
  captures only the implemented hardware-watchpoint value/control pairs under
  the same constraints. A fourteenth captures Hypervisor.framework's two raw
  host debug-trap policy booleans and can reapply the complete pair, exception
  policy first, without defining wider guest-debug ordering or destination
  policy. A fifteenth
  captures optional macOS 15.2 ZFR0/SMFR0 compatibility metadata separately
  from the stable baseline. A sixteenth captures macOS 15.2+ `PSTATE.SM` and
  `PSTATE.ZA` through one runtime-resolved getter without calling its setter.
  A seventeenth captures raw macOS 15.2+ SMCR_EL1, SMPRI_EL1, and TPIDR2_EL0 in
  a separate value whose `Debug` output redacts every register.
  An eighteenth captures raw macOS 15.2+ SCXTNUM_EL0 and SCXTNUM_EL1 in a
  separate value whose `Debug` output redacts both software context numbers.
  A nineteenth conditionally captures all macOS 15.2+ streaming Z0-Z31 bytes at
  the configuration-wide maximum allocation width, after an owner-thread
  `PSTATE.SM` preflight, and redacts the complete buffer from `Debug`.
  A twentieth conditionally captures all macOS 15.2+ streaming P0-P15 bytes at
  one eighth of that maximum per predicate, after the same owner-thread
  preflight, and redacts the complete buffer from `Debug`.
  A twenty-first conditionally captures the complete macOS 15.2+ ZA matrix at
  the checked square of that maximum, after an owner-thread `PSTATE.ZA`
  preflight that does not require streaming mode, and redacts the complete
  buffer from `Debug`.
  A twenty-second conditionally captures the fixed 64-byte macOS 15.2+ SME2 ZT0
  register after the same owner-thread `PSTATE.ZA` preflight, without requiring
  streaming mode or maximum SVL, and redacts all bytes from `Debug`.
  A separate pre-VM query captures raw default-configuration CTR_EL0,
  CLIDR_EL1, and DCZID_EL0 feature metadata without changing vCPU creation.
  Another independent pre-VM query captures all eight raw data/unified and all
  eight instruction CCSIDR_EL1 values from a fresh default configuration.
  Newer beta-only IDs, broader configuration-time feature manifests, feature
  masking, destination policy, effective SME streaming vector length,
  ZT0 lane/feature policy and ZA layout interpretation,
  table and vector memory, optional CPACR and pointer-authentication feature
  validation, cache feature/geometry interpretation and masks, selector
  validation and maintenance, breakpoint and watchpoint control
  validation, debug-control writable/status-bit and destination policy,
  debug-trap destination policy and guest/host ordering, protected
  key persistence, and remaining wider restore ordering remain outside these
  subsets. General-register,
  core-system-register, exception-register, execution-control, debug-control,
  thread-context, and pointer-authentication restore report their typed failed
  register and completed-write count. Debug-trap restore instead reports the exact failed
  host-policy operation and completed prefix without either Boolean; callers
  must retry the complete captured value or discard the vCPU before execution.
  The runner can capture raw CNTKCTL_EL1, CNTP_CTL_EL0, CNTP_CVAL_EL0, and
  CNTP_TVAL_EL0 on the owning thread when macOS 15 physical-timer prerequisites
  are met. The absolute and relative views are read sequentially and do not
  form a simultaneous observation. It also gets and sets the HVF virtual-timer
  mask, raw offset, raw control, and raw CVAL on that owning thread and can
  capture those fields through one serialized command. It can
  also capture Hypervisor.framework's stable, versioned opaque GIC device blob
  except CPU system registers while the native-v1 size-one runner is stopped,
  and reapply that complete value through a separate never-run owner command.
  A companion owner-thread command captures all ten EL1 ICC CPU-interface
  registers exposed by the current SDK as a separate per-vCPU value.
  None of these subsets is a complete or portable restore model. The
  runner explicitly dispatches one resolved MMIO access through a shared runtime
  dispatcher on its owning thread and supports one cancellable `hv_vcpu_run`
  step at a time. The internal boot session owns the ordered runners through an
  aggregate coordinator, dispatches online members concurrently, joins them on
  shutdown, and composes indexed results into a bounded run-loop pump with boot
  block and virtio-net notifications plus per-vCPU EL1 virtual-timer PPIs.
- HVF exit snapshots preserve Hypervisor.framework reasons such as canceled,
  exception, virtual timer activation, and unknown after a run wrapper marks
  exit data available. Candidate arm64 MMIO data-abort exceptions can be decoded
  into checked access metadata and resolved against the internal MMIO registry.
  Checked runtime MMIO operations can be dispatched to registered internal
  handlers. A single resolved HVF exit can be converted into a runtime MMIO
  operation, dispatched through those handlers on the current thread or through
  an explicit runner-thread command, and completed back into guest GPRs for
  successful reads. Each runner performs that path for one identified step, and
  the boot session coordinates online members through a bounded internal loop
  that terminates on explicit aggregate outcomes. Full public Firecracker
  run-loop control beyond pause/resume remains deferred.
- The implemented `PATCH /vm` contract drains every online vCPU through the
  aggregate active-run barrier before a real pause and resumes only after the
  process-owned boot worker accepts the command. Same-state requests are
  successful controller no-ops that retain session ownership and avoid another
  backend generation. The one-vCPU native-v1 baseline adds complete
  snapshot-ready capture/publication orchestration with four acknowledged retry
  schedulers above this barrier. Complete optional HVF state, optional-device
  ownership, and multi-vCPU snapshot artifacts remain deferred.
- Device-facing interrupt triggers are backend-neutral runtime state today, and
  HVF interrupt-line support can allocate deterministic SPI lines from GIC
  metadata and set validated SPI levels through `hv_gic_set_spi`. Internal boot
  sessions can now use that path for block queue interrupts and virtio-net
  network queue interrupts, while device interrupt masking, timer EOI/deactivation-driven
  unmasking, runner-loop interrupt delivery beyond the current internal
  block/network/timer paths, and public device wiring still need
  macOS-specific backend work.
- Linux seccomp, jailer, cgroup, and namespace mechanisms do not directly
  apply; the fixed-code/current-user/rlimit/daemon outcomes above are macOS
  equivalents, not those Linux identities.
- Linux TAP-based networking needs a macOS-specific design.
- Snapshot and device behavior may differ when backed by HVF.

The initial compatibility scope should document these differences without
pretending they are solved. See [macOS Host Security Model](security.md) for the
host isolation boundary. The lower-level `app_sandbox` target proves that the
HVF lifecycle and process can execute inside App Sandbox. The separate
`production_bundle` target proves the fixed launcher/nested-worker package,
signature and entitlement split, tamper gate, container denial/redaction,
signal/exit forwarding, mandatory lifecycle-v5 policy/grant acknowledgment,
closed environment/descriptors, exact resource limits and private cwd, signed
daemon readiness/ownership and parent-loss cancellation, typed
SCM_RIGHTS file authority, one-session directory bookmark scope, atomic
rollback, grant-bearing crash/concurrency behavior, socket cleanup, and real HVF
guest lifecycle. The ordinary CLI remains uncontained. Contained startup config,
metadata, kernel, and initrd consumers now adopt exact, one-time read-only
grants. Block and pmem consumers adopt repeatable exact-ID grants with
read-only/read-write enforcement, same-ID rollback, move-only startup ownership,
limiter-only retention, and preauthorized live block replacement. Logger,
metrics, and serial consumers adopt singleton exact-ID `WriteOnly` regular-file
grants with validation-before-claim, append/nonblocking normalization, retained
logger/metrics sinks, and move-only serial startup ownership. The signed normal
bundle proves startup CLI, config-file, and delayed API use, retained opened
identity after pathname replacement, redacted failure-atomic mismatch handling,
guest block I/O, read-only rejection, pmem read/flush, logger records, initial
and terminal metrics, guest serial bytes, concurrent output isolation, and live
block swap. API and vsock consumers now adopt exact singleton directory grants
plus a bounded safe child, use short-lived signed binders for same-filesystem
anchored exclusive publication, retain supplied listeners and identity-aware
cleanup, and keep guest-initiated vsock connects on one fixed session-bound
launcher port facet without guest payloads or an outgoing-network entitlement.
Signed normal-bundle proof covers outside-container API clients, real guest- and
host-initiated vsock traffic, two contained vhost-user children from one
connect-only directory grant, active CONFIG refresh on the retained stream, no
surviving helper, and unchanged entitlements.
Snapshot path consumers use exact state/memory/root and repeatable output grants.
General dynamic brokerage, cross-filesystem socket publication, and hard
revocation remain incomplete. Lifecycle v5 carries a canonical
bounded host/shared/exact-bridge allowlist plus a separate 1-through-4 active
limit, retains it immutably in contained workers, and applies it to the complete
non-MMDS-only interface set before any startup resource or vmnet backend is
acquired. Direct mode is unchanged and all-MMDS requires no vmnet authority.
The default App Sandbox plus Hypervisor worker is an exact profile-absent
networkless profile and rejects every positive authority before spawn/resume.
An explicit caller-approved vmnet package profile requires a named identity,
bounded captured profile, exact five-key signature, profile-listed signing
leaf, and successful disposable current-host authorization probe before
publication; runtime static/live checks then require that profile and a
nonempty lifecycle authority. This establishes the production packaging and
policy gate but does not claim a repository-owned restricted credential,
`vmnet_start_interface` success, or real contained connectivity. Arbitrary
credential/chroot authority, remaining Linux
jailer controls, seccomp outcome classification, and deployment signing policy
remain later #1351 work.

## Validation Expectations

Every future compatibility change should choose validation appropriate to its
surface:

- unit tests for parsing, configuration, and state transitions
- golden tests for Firecracker-shaped API responses once the API exists
- real HVF-backed integration tests on macOS Apple Silicon through
  `scripts/run-integration-tests.sh`, which signs the selected HVF test
  binaries, executable e2e artifacts, or fixed production bundle before running
  them; the production worker receives App Sandbox plus Hypervisor entitlements
  while its launcher receives neither. The script prepares the pinned
  Firecracker kernel plus generated tiny initrd for guest boot, executable HVF
  e2e, and production bundle tests, and fails when the host cannot run HVF tests
  unless CI explicitly uses `--allow-unsupported` after build/sign validation

Changes that alter support status or validation coverage should also update
[Firecracker Validation Matrix](firecracker-validation-matrix.md).

## Security and Performance Scope

Security review should cover host paths, Unix sockets, FFI boundaries, guest
memory, device I/O, and untrusted API or guest input as those surfaces are
introduced. Performance review should cover startup path, memory mapping, vCPU
run loops, and device I/O when those areas change.

Detailed security and performance analysis belongs with the capability work that
introduces or changes the relevant surface.
