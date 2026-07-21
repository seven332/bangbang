# bangbang

bangbang is a Rust VMM project for macOS hosts. It aims to keep the public
control plane compatible with the Firecracker HTTP API over a Unix domain
socket, while the VM backend is built on Apple's Hypervisor.framework.

The repository is still a scaffold. Use the documentation below as the source of
truth for detailed capability status, compatibility limits, security boundaries,
and test rules:

- [Firecracker Compatibility Scope](docs/firecracker-compatibility.md)
- [Firecracker Validation Matrix](docs/firecracker-validation-matrix.md)
- [Firecracker v1.16.0 Capability Inventory](compat/firecracker/v1.16.0/README.md)
- [Snapshot Feasibility](docs/snapshot-feasibility.md)
- [macOS Host Security Model](docs/security.md)
- [Testing Guide](docs/testing.md)
- [Pull Request Review Guidelines](docs/review-guidelines.md)

The reconciled Firecracker v1.16.0 remaining-device subset covers
virtio-balloon reporting and zero-safe best-effort Darwin discard, bounded
virtio-rng, targeted and rate-limited virtio-pmem flush, a block-granular
virtio-mem plug/unplug lifecycle, the no-interrupt aarch64 PL031 RTC,
DeviceTree VMGenID including native-v1 replacement notification, and startup
VMClock discovery. Pmem now registers its retained file-backed mapping directly
with HVF, supports deterministic read-only or writable root boot, and retains
exact dynamic PCI flush, teardown, and range reuse; PCI network attach/delete
owns per-interface packet I/O, metrics, teardown, and slot reuse. ARM PVTime,
serialized/restorable pmem snapshot state, and mutable VMClock restore remain
explicit limits. Host discard never promises synchronous RSS or footprint
reduction. See the
[pinned remaining-device audit](docs/firecracker-compatibility.md#firecracker-v1160-remaining-device-audit)
for exact upstream sources and classifications.

On macOS arm64 hosts with the required macOS 15+ HVF GIC/MSI symbols,
`--enable-pci` selects Firecracker's exclusive all-virtio startup transport.
Balloon, block, network, pmem, vsock, entropy, and virtio-mem functions are
published in that order on segment 0/bus 0; serial, RTC, boot timer, GIC,
VMGenID, and VMClock remain platform MMIO devices. Startup preflights the target,
symbols, endpoint slots, one 512-KiB BAR per function, dispatcher regions, and
the exact maximum queue-plus-configuration MSI-X demand before guest-visible
publication. Linux may allocate fewer vectors than a device's maximum table, so
each independently revocable endpoint registry resolves the complete
generation-bound VM GICv2m pool rather than predicting per-driver subranges.

PCI mode omits the VMM-supplied `pci=off`, publishes the 1-MiB generic ECAM host
and `arm,gic-v2m-frame`, and suppresses every virtio legacy SPI/MMIO/FDT node.
Default startup remains all-virtio-MMIO and retains `pci=off`. Signed Apple
Silicon coverage boots pinned Firecracker Linux with all seven device classes,
checks stable identities, and performs real block, MMDS/network, pmem, vsock,
balloon, entropy, and virtio-mem interrupt/I/O operations. Runtime block,
network, and pmem in-place PATCH keeps working in either startup transport;
PCI-mode block and non-root pmem PUT/DELETE use failure-atomic owner-thread
transactions in Running or Paused state, with explicit guest rescan/removal and
exact capacity reuse. Pmem additionally owns a shared direct-mapping lease,
exact-prefix synchronous flush/unmap, and reusable aligned guest range.
PCI-mode network PUT/DELETE uses the same owner-thread boundary with independent
MMDS-only or vmnet packet I/O,
generation-safe metrics, and exact cleanup. Automatic guest hotplug
notification, PCI snapshot persistence, external vmnet connectivity
certification, and Firecracker's KVM ITS identity remain explicit limits.
Native-v1 create runs the complete paused live-storage preflight and then
rejects PCI before native-state capture or artifact work. Load keeps its
pre-file/grant/controller/VM-mutation PCI rejection, while the default MMIO
snapshot profile is unchanged.

File-backed drives accept an existing regular file or, on macOS, one exact
block-special descriptor over MMIO and PCI with omitted/default `Sync` or
explicit `Async`. Direct block media obtains checked capacity and persistence
through public `DKIOCGETBLOCKSIZE`, `DKIOCGETBLOCKCOUNT`, and
`DKIOCSYNCHRONIZECACHE`; contained media uses the launcher's retained exact
grant descriptor for those App-Sandbox-denied operations through a fixed
session/sequence/grant-bound control facet. The worker receives no path lookup,
device enumeration, generic ioctl, or added entitlement. `Async` uses one lazy
bounded portable worker pool per VM session and one completion wakeup watched
by the owner thread; completion, not submission, publishes descriptor status,
used entries, dirty ranges, interrupts, and metrics. Multiple drives use
generation-bound routing so live path PATCH, same-ID backing/engine/limiter PUT,
PCI hotplug/DELETE/reuse, reset, and shutdown quiesce only the intended work.
Regular-to-block, block-to-regular, and block-to-block replacements use the same
failure-atomic transaction. This is the Firecracker-shaped public engine
behavior, not a claim that macOS supplies Linux io_uring. A paused snapshot-create
preflight now asks the live boot owner to traverse every startup or runtime
block and pmem endpoint across MMIO and PCI. It closes all Async admissions,
drains and publishes all entered work, captures exact continuation and
transport state, and reopens the same generations before returning. Native-v1
serialization remains Sync-only, so broader profiles still reject before
artifact creation after this non-persisting preflight.

Pre-boot drives may instead select Firecracker's vhost-user block `socket`
shape. Direct mode accepts an operator path; production contained mode accepts
only `bangbang-grant:<GrantId>/<SocketChild>` backed by a repeatable
connect-only directory grant. The launcher connects the exact current-user,
single-link socket relative to its retained directory descriptor and returns
only the nonblocking stream over a dedicated authenticated broker facet.
Startup switches guest RAM to descriptor-backed shared mappings, negotiates the
reviewed CONFIG and virtio feature set, and transfers one queue plus the
complete bounded memory table to the selected backend. When virtio-mem is
configured, startup also creates one sparse unlinked shared reservation for its
complete deterministic aperture before device preparation, even if no vhost
device is initially present. The reservation is not current guest RAM: offline
bytes remain outside CPU/HVF mappings, FDT RAM, dirty metadata, byte access,
`total_size`, and the public plugged size. Online blocks are exact views into
that retained mapping. A vhost backend receives boot RAM plus that one aperture
in an immutable table before queue activation (at most three regions on arm64),
so it can access currently unplugged bytes but no unrelated mapping. The same
device works over default MMIO or all-virtio PCI, including
root/partuuid/read-only/writeback behavior, backend-call interrupts, metrics,
and redacted terminal disconnects. Every external backend is therefore a
trusted confidentiality, integrity, and availability capability. Darwin offers
no Linux `memfd` seal parity, and bangbang ships no production vhost backend;
backend implementation, policy, and isolation remain operator-owned.
An ID-only PATCH refreshes an active MMIO or PCI frontend without reconnecting.
Both pre-boot configuration orders are accepted. In an eligible all-PCI
dynamic-memory or otherwise shared VM, Running or Paused requests may also
attach a new non-root direct or contained socket drive after a no-side-effect
owner preflight and remove it after manual guest-side PCI removal. Every
initial or runtime frontend receives the same stable export topology;
grow/shrink never sends a second memory table. A contained directory is adopted
once per session and may authorize multiple exact children, retries, and
reinsertion after DELETE; each drive retains only its child lease. Duplicate
IDs, anonymous RAM, root insertion, and exhausted capacity reject before any
broker request or direct socket connection. DELETE or backend death releases
the frontend's descriptor clones and device resources without dropping the
VM-owned aperture; final shutdown drops it after online views are gone.
Automatic guest PCI notification, same-ID vhost replacement without DELETE,
and vhost snapshot state remain explicit limits. Snapshot preflight scans the
complete live vhost inventory first and returns one typed, path-redacted
unsupported result before Async mutation, contained grant claims, or artifact
staging.

The checked
[storage closure contract](compat/firecracker/v1.16.0/storage-contract.md)
certifies these block and pmem leaves together. A purpose-built signed guest
profile runs a read-only Sync root, writable Sync control, portable-Async
drive, vhost-user drive, pmem, and virtio-mem in one product-PCI lifecycle.
Direct and production-contained cases prove disjoint concurrent PATCH,
pause/resume, grow/shrink, failure-atomic Async backing replacement,
block/pmem attach-remove-reuse, exact persistence and capacity reuse, and
terminal or orderly cleanup. The contained case uses only existing exact
grants and a connect-only vhost directory, with no helper or entitlement
change. Runtime pmem capacity preflight now precedes direct open/map and
contained grant claim. The live pmem schema and API leaves are terminal;
`corpus:pmem` and its state aggregate remain Wave 6 work solely for
optional-device snapshot serialization/restore and portability outcomes.

## Layout

```text
crates/api        Firecracker-compatible API request and response surface
crates/runtime    Backend-neutral VM model, memory, MMIO, boot, and device helpers
crates/hvf        Hypervisor.framework backend and signed integration tests
crates/bangbang   VMM process entrypoint and startup CLI
crates/launcher   Production app bundle, nested-worker validation, and supervision
crates/session    Private launcher-worker protocol and runtime namespace ownership
crates/vhost-user Strict vhost-user frontend protocol, SCM_RIGHTS framing,
                  and portable pipe queue notifiers used by direct block startup
tools/firecracker-capability-audit
                  Checked Firecracker source/capability inventory validator
tools/seccompiler Firecracker v1.16-compatible offline seccompiler CLI and
                  reusable Linux-target compiler core; neither installs nor
                  enforces filters on macOS
```

Build or run the offline tool with the public Firecracker argument names:

```sh
cargo run -p bangbang-seccompiler --bin seccompiler-bin --locked -- \
  --target-arch aarch64 \
  --input-file policy.json \
  --output-file seccomp_binary_filter.out
```

`--split-output` writes `vmm.bpf`, `api.bpf`, and `vcpu.bpf` in the selected
output parent; `--basic` retains Firecracker v1.16's deprecated
condition-dropping mode. Combined output uses the pinned v1.16 bitcode format
and Firecracker's 100,000-byte consumer limit. The tool reads one bounded
regular UTF-8 policy, rejects symlink and special-file endpoints, stages
owner-only complete outputs, and publishes through checked atomic rename
operations. It is an artifact compiler only: current public macOS cannot install
or enforce Firecracker's per-thread Linux seccomp filters. Runtime installation
is therefore a certified platform exclusion, and the executable rejects both
runtime seccomp inputs before opening a filter path or constructing the VMM.

On supported macOS Apple Silicon hosts, the public machine configuration accepts
`vcpu_count` from 1 through 32 and HVF startup admits the host-limited subset
`1..=min(32, host_max)`. Counts above the runtime host maximum fail before a
session is retained or the instance becomes `Running`. Machine memory accepts
`1..=1,046,528` MiB (1022 GiB); unlike Firecracker's stored-request/later-clamp
behavior, Bangbang rejects a larger value before storage so GET, balloon, FDT,
guest memory, and native snapshots use one exact size. Dynamic host-free-memory
preflight is not promised. `huge_pages = "None"` is supported; exact Firecracker
`"2M"` Linux hugetlbfs backing is unavailable through public arm64 macOS/HVF
and returns a stable platform fault rather than substituting alignment or a
16-KiB IPA granule. See the checked
[machine-memory contract](compat/firecracker/v1.16.0/machine-memory-contract.md).
Before VM creation, arm64 startup also reads one same-default-configuration HVF
cache identity and requires exactly one public macOS performance-level
description to confirm its cache sizes and sharing factors. Invalid,
incomplete, mismatched, or ambiguous facts fail startup without retaining VM
state. The resulting FDT publishes exact split or unified L1 geometry and
shared unified L2/L3 nodes, with deterministic links for every guest vCPU and
partial final sharing groups. Host performance-level CPU counts prove sharing
only; they do not reduce the existing guest-vCPU limit. Signed Linux coverage
compares the retained model against the guest's cache sysfs view.
Public pause/resume uses
a topology-wide active-run barrier for every online vCPU. Guest PSCI `CPU_OFF`
and later `CPU_ON` re-entry reuse the fixed owner topology. PSCI
`CPU_SUSPEND32/64` provides KVM-style retained standby for an enabled,
guest-unmasked EL1 virtual timer: affinity remains `ON`, all three call
arguments are ignored, the timer PPI is made pending before `SUCCESS`, and
lifecycle cancellation rearms the same transaction without fabricating a
wake. Runtime discovery reports PSCI 1.0 and a minimal safe SMCCC 1.1 surface:
`PSCI_FEATURES` advertises only delivered calls, `SMCCC_ARCH_FEATURES` reports
only its mandatory VERSION/self results, and optional firmware services remain
unsupported. The FDT deliberately keeps Firecracker v1.15.1's
`arm,psci-0.2`/HVC binding. FDT idle-state discovery and SGI/SPI/direct IRQ/FIQ
wake are not exposed. Dynamic CPU topology, SMT, static CPU-template execution,
and cross-host CPU portability remain unsupported. The native-v1 snapshot
profile below remains restricted to exactly one vCPU and no effective custom
template.

Firecracker-shaped `PUT /cpu-config` retains bounded ordered values and applies
exact expert-controlled masks for eleven U64 arm64 identification registers,
ACTLR.EnTSO, and the reviewed X/core/SIMD/FP profile on every owner-thread vCPU
before boot overrides. ZFR0/SMFR0 have a public macOS 15.2 pre-VM availability
gate; ACTLR accepts only filter bit 1. The profile also admits U64 X0 and
X4-X30 plus the reviewed SP/PC/PSTATE fields, U128 Q0-Q31 with explicit
little-endian conversion, and U32 FPCR/FPSR with fail-closed HVF transport
conversion. Every other KVM class and named public-HVF system family receives a
stable value-free topology, lifecycle, security, time, ownership, dependency,
disabled-feature, or platform reason; there is no raw system-register escape
hatch. All requested baselines are read and compared before the first write;
every write is immediately reread, and any failure destroys the unpublished
VM. Empty custom input clears the selection. Machine `V1N1` remains GET-visible
pending configuration and can be replaced by custom or `None`, but if still
effective it fails before VM construction because Apple Silicon cannot
truthfully provide Firecracker's documented Neoverse V1 source model. Custom
contents remain omitted from GET and native-v1 snapshots, and exact readback is
not a cross-host portability or feature-coherence guarantee. See the checked
[CPU-template contract](compat/firecracker/v1.16.0/cpu-template-contract.md).

The HVF runner currently exposes owner-thread capture building blocks for
general registers, plus ordered nontransactional restore of the same typed
X0-X30/PC/CPSR value, raw core system registers plus ordered nontransactional
restore of their typed SP_EL0/SP_EL1/ELR_EL1/SPSR_EL1 value, raw EL1 exception
registers plus ordered nontransactional restore of their typed
AFSR0/AFSR1/ESR/FAR/PAR/VBAR value, raw EL1 execution controls plus ordered
nontransactional restore of their typed ACTLR/CPACR value, raw thread-context
registers plus ordered nontransactional restore of their typed
TPIDR_EL0/TPIDRRO_EL0/TPIDR_EL1 value, raw EL1 translation registers plus
ordered nontransactional restore of their typed
SCTLR/TTBR0/TTBR1/TCR/MAIR/AMAIR/CONTEXTIDR value, baseline SIMD/FP registers
plus ordered nontransactional restore of their typed Q0-Q31/FPCR/FPSR value,
baseline and optional SVE/SME guest-visible processor identification metadata,
mutable SME PSTATE flags, raw SME system registers with redacted `Debug`,
conditional maximum-width streaming Z0-Z31 contents with
redacted `Debug`, conditional maximum-derived streaming P0-P15 predicates with
redacted `Debug`, conditional maximum-SVL-square ZA contents with redacted
`Debug`, conditional fixed-size SME2 ZT0 contents with redacted `Debug`, raw
system-context registers with redacted `Debug` plus ordered nontransactional
restore of their typed SCXTNUM_EL0/SCXTNUM_EL1 value, raw cache-selection plus
ordered nontransactional restore of its typed CSSELR_EL1 value,
hardware-breakpoint,
hardware-watchpoint, debug-control plus ordered nontransactional restore of its
typed MDCCINT_EL1/MDSCR_EL1 value, raw Hypervisor.framework debug-trap policy
plus ordered nontransactional restore of its complete two-Boolean value,
pointer-authentication key state with redacted `Debug` plus ordered
nontransactional restore of the complete APIA/APIB/APDA/APDB/APGA value, raw
physical and virtual timer state plus a separate debug-redacted normalized
timer value with ordered never-run restore, CPU-level IRQ/FIQ pending injection
levels plus ordered nontransactional restore of their complete typed value,
opaque GIC device state plus runner-owned pre-first-run reapply, and raw EL1
GIC ICC CPU-interface registers plus ordered pre-first-run restore of their nine
mutable values with derived RPR validation.
A native-v1 optional-state classifier fails closed for active SVE/SME and
enabled hardware breakpoint/watchpoint state. Prepared boot sessions can also
replace the 16-byte VMGenID buffer and retained metadata before first run, then
inject its edge-rising SPI after replacement. A separate no-handle query
exposes the maximum SME streaming vector length used for the Z-, P-, and
ZA-register allocations.

These primitives back a deliberately narrow public native-v1 snapshot path on
macOS Apple Silicon. `PUT /snapshot/create` supports only `Full` snapshots from
a paused VM with one vCPU, exactly one regular read-only root drive, default
serial, and no optional devices or MMDS. It writes a bounded kind-2
`BANGCMT\0` pair whose state file binds the complete memory image to an exact
five-component `BANGHVF\0` payload and nested `BANGDEV\0` device profile.

Create reserves one FIFO boot-worker transaction, then failure-atomically
quiesces the block, PMEM, network, and entropy retry publishers. The same lease
preflights both final namespaces, streams the paused aggregate capture into an
owner-only staging inode, verifies and synchronizes it, publishes memory first
and state last as the commit marker without replacing existing entries, and
runs the session's post-publication transition before reopening ordinary command
admission. For tracked sessions this failure-atomically re-protects guest-written
pages and advances the shared epoch. The synchronous process borrow also serializes API/MMDS/controller
mutation and periodic callbacks until this transaction returns.

SIGINT/SIGTERM cancellation wins only before the atomic commit seal. Once
sealed, publication finishes and preserves its exact durable,
durability-uncertain, memory-orphan, or other typed visibility result before
orderly shutdown continues. A successful request returns `204 No Content` and
leaves the source paused and usable. Earlier failures clean only private staging
where safe and release every scheduler without losing deferred wakeups. A
recoverable tracked reset failure preserves the old conservative epoch; an
incomplete rollback keeps the committed artifact result but latches terminal
failure and prevents resume.

`PUT /snapshot/load` accepts the matching committed pair only in a pristine
fresh process, except that logger and metrics configuration are allowed. It
supports a `File` memory backend (or the deprecated sole `mem_file_path` alias),
constructs a fresh HVF VM/GIC/vCPU, restores the exact local native state,
replaces and signals VMGenID, and first commits the session as `Paused`.
`track_dirty_pages: true` or deprecated `enable_diff_snapshots: true` installs
tracking after the loaded memory baseline and before mapping, vCPU ownership,
and VMGenID replacement; the destination request controls the restored setting
independently of the source snapshot.
`resume_vm: true` then uses the ordinary resume path; otherwise resume later
with `PATCH /vm`. The external root backing must still match the captured
regular-file identity. Snapshot files and guest state are untrusted and
confidential, so keep artifacts and the API socket in operator-owned private
directories.

In the production bundle, contained describe/load inputs use exact read-only
file grants and create outputs use retained `SnapshotOutputDirectory` anchors
plus bounded UTF-8 child names. Load atomically adopts state, memory, and any
grant-tagged persisted root backing after bounded state preinspection; no tag is
reopened as a pathname. Create preserves the same anchor-relative no-clobber
transaction and repeated output-directory authority. Direct mode keeps ordinary
path behavior.

The transaction stops bangbang-owned packet and stream access because the
accepted profile excludes network and vsock devices and the transient vsock
poller is joined before pause acknowledgement. It does not freeze or persist
vmnet peers, vsock peers, or their host/kernel buffers. Native-v1 remains a
one-vCPU baseline; optional devices and multi-vCPU snapshot artifacts are still
outside this format.

This is not Firecracker snapshot-file compatibility or a portable migration
format. Machine `track_dirty_pages` now enables one shared guest-RAM epoch
before boot population. Boot-loader, VMM, current virtio-device, balloon
discard, dynamic-memory, and guest-CPU writes all enter the same bitmap; HVF
keeps a separate write-protection overlay. A visibly committed Full snapshot
re-protects guest-written pages before clearing and advancing the epoch while
the source is still paused. Complete rollback keeps the old conservative epoch;
incomplete rollback prevents resume and tears the VM down safely. `Diff`
artifacts and merging, UFFD, clock adjustment, restore overrides, writable or
additional drives, serialized/restorable optional-device snapshot state,
active SVE/SME/debug state, EL2 GIC CPU-interface state, and cross-host
portability remain unsupported.

## Process CLI

Run the VMM process skeleton and API server:

```sh
cargo run -p bangbang -- --api-sock /tmp/bangbang.socket --id demo-1
```

Supported value-taking options accept either `--name value` or `--name=value`.
Value-less flags, such as `--no-api`, do not accept an attached value.

- `--api-sock <PATH>` sets the Unix socket path. The default is
  `/tmp/bangbang.socket`.
- `--boot-timer` enables Firecracker-compatible guest boot-time logging. During
  startup, bangbang registers a pseudo-MMIO boot timer at Firecracker's aarch64
  boot timer address; a guest write of byte value `123` at offset `0` logs the
  elapsed wall and process CPU time when logger output is configured.
- `--config-file <PATH>` reads a Firecracker-shaped JSON configuration for the
  supported startup subset from a readable regular file up to 1 MiB, starts the
  VM, then serves the API socket unless `--no-api` is set.
- `--http-api-max-payload-size <BYTES>` sets the maximum accepted HTTP API
  request body size declared by `Content-Length`. The default is `51200` bytes;
  request-head bytes are bounded separately by the parser.
- `--id <ID>` records the microVM identifier. The default is
  `anonymous-instance`.
- `--start-time-us <MICROS>`, `--start-time-cpu-us <MICROS>`, and
  `--parent-cpu-time-us <MICROS>` accept Firecracker launcher timing values for
  session-initial, explicit `FlushMetrics`, 60-second Running/Paused periodic,
  and normal-terminal metrics output.
- `--metrics-path <PATH>` configures the same per-process metrics sink as
  `PUT /metrics` before the API socket is served.
- `--mmds-size-limit <BYTES>` sets the maximum serialized MMDS data-store size.
  When omitted, it inherits the HTTP API request-size limit, which defaults to
  `51200` bytes.
- `--log-path <PATH>`, `--level <LEVEL>`, `--module <MODULE>`,
  `--show-level`, and `--show-log-origin` configure the same per-process
  logger state as `PUT /logger` before the API socket is served. Implemented
  logger events use module paths `bangbang_runtime::api_server`,
  `bangbang_runtime::vmm_action`, and `bangbang_runtime::boot_timer`.
- `--no-api` requires `--config-file <PATH>`, starts from that configuration
  without publishing an API socket, and exits cleanly on `SIGINT` or `SIGTERM`.
- `--snapshot-version` prints the supported bangbang-native snapshot envelope
  version (`v1.0.0`) and exits before startup.
- `--describe-snapshot <PATH>` reads a bounded regular native state file,
  validates its complete envelope and CRC, prints its embedded version, and
  exits before startup. In contained mode an exact read-only
  `SnapshotDescribeInput` grant is inspected without reopening its tag. It does
  not accept Firecracker state files.
- `--help`, `-h`, `--version`, and `-V` are supported.

The API socket is an unauthenticated local control interface. bangbang restricts
the published socket inode to owner-only permissions; the parent directory is
still part of the access-control boundary, so use a private directory on
multi-user hosts.

Start with metrics and logger output configured:

```sh
cargo run -p bangbang -- \
  --api-sock /tmp/bangbang.socket \
  --id demo-1 \
  --metrics-path /tmp/bangbang.metrics \
  --log-path /tmp/bangbang.log \
  --level Info \
  --show-level
```

Start from a configuration file while keeping the API socket enabled:

```sh
cargo run -p bangbang -- \
  --api-sock /tmp/bangbang.socket \
  --config-file /tmp/bangbang-vm.json
```

Start from a configuration file without publishing an API socket:

```sh
cargo run -p bangbang -- \
  --config-file /tmp/bangbang-vm.json \
  --no-api
```

## Production macOS Bundle

The direct `cargo run -p bangbang` path above is intentionally uncontained: it
runs the VMM as the invoking user and relies on host filesystem permissions and
per-resource validation. The production entry point instead has a fixed
two-process topology:

```text
Bangbang.app                          dev.bangbang
├── Contents/MacOS/bangbang           unsandboxed launcher
└── Contents/Helpers/BangbangWorker.app
    └── Contents/MacOS/bangbang-worker  App Sandbox + Hypervisor worker
```

Build and exclusively publish it to an absent destination named
`Bangbang.app`:

```sh
scripts/build-production-bundle.sh --output /private/operator/Bangbang.app
```

Ad-hoc signing (`-`) is the local-validation default. A distribution build can
supply one identity for both separately signed code objects:

```sh
scripts/build-production-bundle.sh \
  --output /private/operator/Bangbang.app \
  --signing-identity "Developer ID Application: Example (TEAMID)"
```

`networkless` is the default worker profile. An operator with an Apple-approved
vmnet provisioning profile can test the exact restricted authorization without
publishing an app:

```sh
scripts/preflight-production-vmnet.sh \
  --output /private/operator/Bangbang.app \
  --signing-identity "Developer ID Application: Example (TEAMID)" \
  --provisioning-profile /private/operator/vmnet.provisionprofile
```

The command prints exactly `bangbang vmnet preflight: ready` on success. Any
profile, identity, signing, inspection, or current-host authorization failure
prints exactly `bangbang vmnet preflight: blocked` and exits 3. A successful
preflight permits the same inputs to publish:

```sh
scripts/build-production-bundle.sh \
  --output /private/operator/Bangbang.app \
  --worker-profile vmnet \
  --signing-identity "Developer ID Application: Example (TEAMID)" \
  --provisioning-profile /private/operator/vmnet.provisionprofile
```

The networkless worker is signed first with exactly App Sandbox and Hypervisor.
The vmnet worker instead has exactly those two Boolean claims, Boolean
`com.apple.vm.networking`, and the profile-derived application and team
identifiers; its captured profile is embedded before signing. Both profiles
use Hardened Runtime, and the outer launcher is signed last without
entitlements. Packaging checks the bounded profile relationships and validity,
requires the actual signing leaf to be listed by the profile, and inspects the
final two-or-five-key signature. Before vmnet publication it signs and runs a
disposable copy of the already-running package tool with the same identity,
profile, App ID, and entitlements. That private command exits immediately, so
the caller-supplied worker is never executed during packaging. Before every
launch, the outer executable validates
the fixed bundle layout, nested signatures, identifiers, and required worker
entitlements. It then starts the fixed worker suspended with a default-close
descriptor policy: only open standard streams, one private lifecycle endpoint,
one private startup-grant endpoint, one dormant private vsock-broker endpoint,
one dedicated private vhost-user-broker endpoint, and one dedicated private
retained-descriptor block-control endpoint survive. The exec
environment contains only the private lifecycle
marker; ambient launcher variables, including loader/debug controls, are not
forwarded. The launcher validates the live worker code before resuming it and
again after the worker has used the endpoint and sent the bounded pre-session
greeting.

Each launch uses unnamed lifecycle stream, grant datagram, vsock-broker,
vhost-user-broker, and block-control socketpairs plus a random 256-bit session
identity. Lifecycle
protocol v5 has a
4-KiB frame limit, exact per-direction sequence numbers, closed message
variants, a fixed 96-byte reserved-zero `Start(WorkerPolicy)` payload, and monotonic
`prepared -> grants-accepted -> starting -> ready -> terminal` state. Even an
empty grant batch must be atomically acknowledged before `Proceed`. The launcher
authenticates the live worker PID, effective credentials, signature, identity,
and exact entitlements. The sandboxed worker verifies that the peer PID is its
direct parent, that real/effective credentials match the authenticated policy,
and that both processes share the intended session. It then applies and reads
back exact soft/hard `RLIMIT_FSIZE` and `RLIMIT_NOFILE` values without raising an
inherited hard limit. App Sandbox prevents the worker from independently
querying the launcher's code signature, so authentication is deliberately
asymmetric.

`Start` also carries one immutable, redacted `VmnetAuthority`. Its canonical
default denies system vmnet; a nonempty value can independently allow host and
shared modes, up to four exact 15-byte ASCII bridge names, and a separate active
interface maximum from 1 through 4. The authority is bound to the same random
session, sender role, sequence, fixed worker identity, and daemon handoff as the
credential/limit policy. A two-entitlement/profile-absent worker is classified
as `Networkless` and rejects every nonempty vmnet policy. An exact
five-entitlement/profile-present worker is classified as `Vmnet` and requires a
nonempty authority before spawn or resume. Packaging authorization proves only
that the current host accepted that restricted signature/profile combination;
real `vmnet_start_interface`, packet connectivity, teardown, crash, and
concurrency evidence remain separate and are not claimed here.

Before public argument or VM processing, the worker creates and locks a unique
mode-0700 empty namespace in its App Sandbox container and enters it through the
retained directory descriptor. The launcher derives that path independently and
checks its exact name, owner, mode, device, inode, emptiness, and live lock
before authorizing startup. Graceful signals become one
session cancellation, readiness is reported only at the existing committed API
or no-API seams, and structured terminal status must match the reaped public
exit. Initial `Hello`, `Start`, and `Proceed` reads use absolute five-second
deadlines; cancellation and post-`Terminal`/EOF process-exit waits use a
five-second grace before owned-worker escalation. A surviving worker cleans
after launcher EOF; a surviving launcher cleans after worker exit; a later
worker performs bounded identity-checked recovery when both were killed.
Concurrent sessions retain independent identities, processes, namespaces,
policies, grant registries, and API sockets.

The launcher recognizes this optional versioned launch-policy envelope only in
argv position one:

```text
--bangbang-jailer-v1 \
  --id ID \
  --exec-file /exact/BangbangWorker.app/Contents/MacOS/bangbang-worker \
  --uid CURRENT_UID --gid CURRENT_GID \
  [--resource-limit fsize=U64] \
  [--resource-limit no-file=U64] \
  [--vmnet-allow host|shared|bridged:INTERFACE]... \
  [--vmnet-max-interfaces 1..=4] \
  [--daemonize] -- \
  [--bangbang-grant-manifest MANIFEST --] FIRECRACKER_ARGS...
```

Singletons, unknown options, malformed values, missing delimiters, a different
executable or credential, and conflicting forwarded ID/timing arguments are
rejected before spawn without echoing values. Repeated `fsize` and `no-file`
entries use the last value; `no-file` defaults to 2048 for every production
worker, including launches without this envelope. The launcher injects the
validated ID and sampled timing once. `--bangbang-jailer-v1 --help` and
`--version` are exact early commands.

### Linux-only runtime isolation arguments

These names are not part of either successful help grammar. They are recognized
only to return a fixed, value-redacted macOS platform error:

| Process | Linux-only input | macOS outcome |
| --- | --- | --- |
| `bangbang` | `--no-seccomp` | rejected before configuration-file access, VMM/backend construction, readiness, or API socket publication |
| `bangbang` | `--seccomp-filter[=PATH]` | rejected before consuming or opening `PATH`, with the same fixed category for missing, separated, attached, duplicate, and conflicting forms |
| `bangbang-launcher` | `--cgroup[=VALUE]` | rejected before grant parsing/preparation, profile selection, staging, spawn, publication, or worker execution |
| `bangbang-launcher` | `--cgroup-version[=VALUE]` | same fixed pre-mutation rejection; Darwin rlimits are not a cgroup version |
| `bangbang-launcher` | `--parent-cgroup[=VALUE]` | same fixed pre-mutation rejection; no parent hierarchy or PID placement is claimed |
| `bangbang-launcher` | `--netns[=PATH]` | same fixed pre-mutation rejection without opening `PATH`; vmnet is guest networking, not host-process `setns` |
| `bangbang-launcher` | `--new-pid-ns[=VALUE]` | same fixed pre-mutation rejection; sessions and supervision do not remap PID 1 or process visibility |

Launcher matching is exact and applies only before the launch-policy `--`.
Attached values are inspected only for the fixed name, separated values are not
consumed, and post-delimiter worker arguments remain opaque. App Sandbox,
rlimits, Endpoint Security, Network Extension, vmnet, process sessions, and the
supervisor retain their narrower documented roles; none is presented as a
seccomp, cgroup, network-namespace, or PID-namespace alias.

`--vmnet-allow` is repeatable but exact host/shared or bridge duplicates are
invalid. A nonempty allowlist requires exactly one
`--vmnet-max-interfaces`; the maximum without an allowlist is also invalid.
Bridge names use `[A-Za-z0-9._-]`, are 1 through 15 bytes, and match guest
configuration exactly. Ordinary post-`--` worker argv, files, environment, and
descriptors cannot create or mutate this authority. Configuration remains
order-neutral: all-MMDS final configurations require no vmnet authority, while
any partial-MMDS configuration authorizes the complete configured interface set
at final InstanceStart before resources or a backend are acquired.

With `--daemonize`, the validated outer launcher performs a default-close,
empty-environment re-exec of the same signed code as a new session leader with
standard streams attached to `/dev/null`. That process remains the sole worker
supervisor. The original command returns only after API/no-API readiness and a
bounded PID acknowledgment, printing exactly `bangbang daemon pid: PID`.
SIGINT or SIGTERM sent to that PID uses the normal graceful worker cancellation
and cleanup path. Original-launcher loss before acknowledgment cancels the
unpublished session; after acknowledgment the handoff endpoint is closed.

This is the unprivileged macOS outcome for fixed executable/current-user
identity, private working root, environment/descriptors, resource limits, and
daemon ownership. It does not claim arbitrary uid/gid switching, configurable
chroot ownership, or Linux runtime-isolation identity. The exact seccomp,
cgroup, network-namespace, and PID-namespace inputs above are certified platform
exclusions rather than accepted no-ops; broader credential/chroot work remains
separate.

After the launch-policy delimiter, the existing optional grant envelope remains
position one in the worker argument sequence:

```text
--bangbang-grant-manifest MANIFEST -- FIRECRACKER_ARGS...
```

Manifest v1 is bounded strict JSON with `version: 1` and a `grants` array. Each
grant has a 64-byte ASCII `id`, one closed `role`, exact `access`, and an
absolute UTF-8 `source` path. The launcher walks resource paths component by
component without following symlinks or accepting `.`/`..`, opens every
existing resource before spawn, rejects aliases and type/access conflicts, and
prepares the complete batch atomically. Regular-file roles transfer only an
identity-checked descriptor. The three create-children directory roles combine
an anchor descriptor with a bounded one-session implicit bookmark whose
resolved inode and active scope are revalidated in the worker.

The initial roles are startup config/metadata, kernel/initrd, repeatable
drive/pmem backing, logger/metrics/serial sinks, snapshot describe/state/memory
inputs, and API/vsock/snapshot-output directories. The exact access matrix and
hard limits are part of the closed protocol; unknown roles and operator-supplied
bookmark bytes are rejected. Grant delivery uses 1024-byte datagrams, bounded
bookmark fragmentation, SCM_RIGHTS, one five-second absolute deadline, and a
session-owned one-time typed registry. Closing the launcher's duplicate does
not revoke an already delivered descriptor; cleanup is cooperative ownership.

Production consumers now adopt read-only startup config, startup metadata,
kernel, initrd, snapshot describe/state/memory, and persisted snapshot-root
grants plus repeatable read-only/read-write block and pmem backing grants,
singleton write-only logger/metrics/serial sink grants, and repeatable snapshot
output-directory grants.
In authenticated contained mode the exact
case-sensitive private reference `bangbang-grant:<GrantId>` claims one matching
ID/role/access entry; malformed, missing, mismatched, or consumed claims fail
without pathname or singleton fallback. Direct mode treats the same bytes as an
ordinary pathname. Config and metadata read the transferred descriptor, while
explicit kernel/initrd references are claimed atomically when boot-source
configuration is applied, retained across API readiness, and consumed once by
boot loading without reopening the reference. Mixed boot sources claim only
their referenced members and leave ordinary members on deferred pathname
opening. Submitted boot references remain visible through the owner-authorized
VM configuration response but never appear in diagnostics. A
descriptor-consuming boot failure requires a fresh contained launch for
grant-backed retry because those roles are singleton.

Block and pmem `PUT` claims validate complete device state before consuming the
exact grant, retain the opened backing by device ID, and move it into startup
without reopening the tag. Access must match `is_read_only`/`read_only`.
Same-ID pre-boot `PUT` replaces the retained authority atomically; ordinary
paths preserve deferred opening. A path-changing live block `PATCH` may consume
one still-unused startup-batch drive grant and swaps the opened backing before
public configuration commits. Path-free block limiter and pmem limiter updates
retain the active backing. A grant consumed by startup or a live block swap is
one-time even if a later consumer step fails; retry requires a fresh same-ID
configuration with unused authority. Authorized configuration responses may
return submitted tags, while logs, faults, errors, and derived debug output stay
value-redacted.

A `DriveBacking` grant is either a regular file or one exact macOS
block-special node. The BBG2 grant record binds kind, device/inode/rdev, exact
access and normalized status flags, logical block size, block count, checked
capacity, and the transferred descriptor. The worker independently rechecks
fstat/fcntl identity and never reopens the tag. Because App Sandbox rejects the
disk geometry and cache-sync ioctls, descriptor 7 carries only fixed 256-byte
`BBC1` `Inspect` and `SynchronizeCache` exchanges for the launcher's retained
copy. Each exchange is lifecycle-session, monotonic-sequence, grant, role,
access, identity, and geometry bound, has a two-second worker deadline, contains
no descriptor rights, and poisons the facet on ambiguous protocol state.

Logger and metrics validate before claiming, normalize the transferred regular
file to append/nonblocking behavior without upgrading its kernel-enforced
write-only access, and retain the opened sink. A logger update without
`log_path` retains that sink and consumes no grant; metrics remains one-time
initialized. Serial retains a prepared output until startup, moves it into the
VM without reopening the reference, and requires successful reconfiguration
after a startup attempt consumes it. Clearing or replacing serial before start
drops the prepared output. Direct paths retain their existing create, FIFO-like,
and open-timing behavior.

Snapshot file inputs use the same exact `bangbang-grant:<GrantId>` grammar with
distinct read-only roles. Describe inspects a duplicate of its exact descriptor.
Load preinspects state without consuming it, discovers any persisted root grant,
then atomically takes all tagged state, memory, and read-only root backings and
finishes from those opened identities. Input authority is one-time after that
take. The persisted root identity includes file metadata such as `ctime`, so a
later rename or metadata-changing replacement is correctly rejected even when
it refers to the same inode.

Create outputs instead use
`bangbang-grant:<GrantId>/<SnapshotOutputChild>`. The child is one 1–255 byte
UTF-8 component, contains no NUL or `/`, and is neither `.` nor `..`. One
retained output grant can serve distinct state/memory children and later create
requests; distinct or mixed ordinary/granted directories are also supported.
Staging and exclusive final publication stay relative to the exact retained
anchors. App Sandbox authorization still requires the granted directory to
remain reachable at its authorized pathname; moving it after scope activation
can make descriptor-relative writes fail.

Each active granted staging inode gets one strict private identity record.
Normal publication or conclusive cleanup clears it; after worker death the
launcher removes only an exact current-user regular `0600`, single-link match
through its retained directory anchor and preserves a replacement. A hard death
between staging creation and record persistence, or simultaneous uncatchable
launcher/worker death, can still leave residue because Darwin has no
identity-conditional unlink primitive.

API and vsock directory consumers instead require the exact case-sensitive
reference `bangbang-grant:<GrantId>/<SocketChild>`. `SocketChild` is one 1–64
byte ASCII `[A-Za-z0-9._-]` component other than `.` or `..`; direct mode still
treats identical bytes as an ordinary path. The owner thread claims the exact
singleton directory role, retains its scope and anchor, and runs a short-lived
default-close instance of the signed worker that binds one fixed private
staging name. The worker receives the listener descriptor, records only its
role, safe child, and socket identity in its private namespace, and publishes
the socket exclusively to the requested child with fd-relative
`renameatx_np(RENAME_EXCL)`. Publication requires the namespace and granted
directory to share a filesystem. The binder is reaped before API readiness or
VM-start success; shutdown removes only an identity-matching socket. A
simultaneous uncatchable launcher and worker death can leave a stale external
socket name plus its private ownership record; automatic later recovery remains
limited to empty session namespaces.

The granted API listener is served directly and becomes ready only after
publication. `--no-api` claims no API directory. A granted vsock keeps the
published main listener plus directory authority through its VM lifetime.
Host-initiated traffic uses that supplied listener. Guest-initiated connections
activate the otherwise dormant per-session launcher broker once, then send only
monotonic `u32` host ports. The launcher is fixed to the retained vsock anchor
and safe child, connects only to relative `<SocketChild>_<port>` targets after
identity checks, and returns one validated connected stream descriptor. It
receives no guest payload, grant ID, path, bookmark, or general resource
selector. API-only, no-API, and direct-path sessions leave the broker dormant;
the worker still has exactly App Sandbox and Hypervisor entitlements and steady
state remains one launcher plus one worker.

General dynamic post-Ready brokerage, hard revocation, cross-filesystem socket
publication, real contained vmnet connectivity and lifecycle evidence, broader
snapshot profiles, automatic restart policy, repository-owned Developer ID or
profile possession, launch-constraint policy, and
notarization workflow remain. The session namespace must be empty at the
`Prepared` gate. Authorized construction may transiently add one fixed
role-specific staging socket or one strict record per active snapshot artifact;
steady state retains no snapshot staging record and at most the two fixed socket
ownership records. Records never expose a path, descriptor, bookmark, grant ID,
payload, or session byte.
Same-identifier workers share one App Sandbox
container, so namespace locks and identity checks protect cooperative sessions
and replacements but do not isolate a malicious same-bundle sibling. See
[macOS Host Security Model](docs/security.md) for the precise trust boundary.

## API Examples

Query the instance info endpoint:

```sh
curl --unix-socket /tmp/bangbang.socket http://localhost/
```

Example response:

```json
{"app_name":"bangbang","id":"demo-1","state":"Not started","vmm_version":"0.1.0"}
```

Query the accumulated VM configuration:

```sh
curl --unix-socket /tmp/bangbang.socket http://localhost/vm/config
```

Record a pre-boot boot source:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/boot-source \
  -H 'Content-Type: application/json' \
  -d '{"kernel_image_path":"/tmp/vmlinux","boot_args":"console=ttyS0 reboot=k panic=1"}'
```

Record a pre-boot drive:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/drives/rootfs \
  -H 'Content-Type: application/json' \
  -d '{"drive_id":"rootfs","path_on_host":"/tmp/rootfs.ext4","is_root_device":true,"is_read_only":true}'
```

Alternatively, configure exactly one pmem root before startup. Pmem order
determines the Linux device name (`/dev/pmem0`, `/dev/pmem1`, and so on), and a
block root and pmem root cannot coexist:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/pmem/rootfs \
  -H 'Content-Type: application/json' \
  -d '{"id":"rootfs","path_on_host":"/tmp/rootfs.ext4","root_device":true,"read_only":true}'
```

With the process started using `--enable-pci`, attach and remove a non-root
drive after startup:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/drives/data \
  -H 'Content-Type: application/json' \
  -d '{"drive_id":"data","path_on_host":"/tmp/data.img","is_root_device":false,"is_read_only":false}'

# Rescan PCI inside Linux, use and flush the disk, then remove its PCI function
# through guest sysfs before issuing the host-side DELETE.
curl --unix-socket /tmp/bangbang.socket \
  -X DELETE http://localhost/drives/data
```

The same public PCI profile supports a non-root pmem lifecycle:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/pmem/pmem0 \
  -H 'Content-Type: application/json' \
  -d '{"id":"pmem0","path_on_host":"/tmp/pmem.img","read_only":false}'

# Rescan PCI inside Linux, flush /dev/pmem*, and remove its PCI function before
# releasing the exact host mapping and endpoint.
curl --unix-socket /tmp/bangbang.socket \
  -X DELETE http://localhost/pmem/pmem0
```

Runtime block, pmem, and network PUT plus bodyless DELETE are accepted in
`Running` and `Paused` when public PCI is enabled. They commit `/vm/config` only after the live owner-thread operation
succeeds; root, duplicate, missing, capacity, backing, mapping, or publication
failures leave the prior configuration intact. Default MMIO sessions reject
the operations before opening the proposed path. In production-contained mode,
runtime PUT can consume only an exact still-unused `drive-backing` or
`pmem-backing` grant from the initial manifest; an aborted insertion restores
that authority without ambient path fallback. Pmem removal synchronizes the
exact file prefix and unmaps only its direct lease before releasing the guest
range. Incomplete cleanup is terminal rather than leaving a damaged worker
live. See the
[runtime device hotplug contract](compat/firecracker/v1.16.0/device-hotplug-contract.md).
Aggregate certification also pins one shared 31-endpoint budget across fixed,
block, pmem, and network functions; equal ID strings remain type-scoped,
duplicate network MACs remain global, and concurrent mutations serialize on
the VM owner while live configuration stays success-authoritative. PCI state
is still rejected by the native-v1 snapshot profile rather than persisted.

Create a supported full native-v1 snapshot after the VM is paused:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PATCH http://localhost/vm \
  -H 'Content-Type: application/json' \
  -d '{"state":"Paused"}'

curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/snapshot/create \
  -H 'Content-Type: application/json' \
  -d '{"snapshot_type":"Full","snapshot_path":"/private/snapshot.state","mem_file_path":"/private/snapshot.memory"}'
```

Load that pair into a fresh `bangbang` process and leave it paused:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/snapshot/load \
  -H 'Content-Type: application/json' \
  -d '{"snapshot_path":"/private/snapshot.state","mem_backend":{"backend_path":"/private/snapshot.memory","backend_type":"File"},"resume_vm":false}'

curl --unix-socket /tmp/bangbang.socket \
  -X PATCH http://localhost/vm \
  -H 'Content-Type: application/json' \
  -d '{"state":"Resumed"}'
```

The destination must be pristine apart from optional logger/metrics setup, and
the captured read-only root backing must still satisfy the recorded identity.

Record a pre-boot network interface:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/network-interfaces/eth0 \
  -H 'Content-Type: application/json' \
  -d '{"iface_id":"eth0","host_dev_name":"vmnet:shared","guest_mac":"12:34:56:78:9a:bc","mtu":1500}'
```

After the VM starts, update individual RX/TX limiter buckets without resetting
omitted buckets:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PATCH http://localhost/network-interfaces/eth0 \
  -H 'Content-Type: application/json' \
  -d '{"iface_id":"eth0","rx_rate_limiter":{"bandwidth":{"size":1048576,"refill_time":100}}}'
```

Set a bucket's `size` or `refill_time` to `0` to disable only that bucket.

With `--enable-pci`, the same PUT endpoint can add a new interface after start.
Rescan PCI inside Linux before using it, remove its PCI function through guest
sysfs before the host-side DELETE, and then release it with:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X DELETE http://localhost/network-interfaces/eth0
```

The immutable pre-boot MMDS interface list selects whether a later interface ID
can use process-local MMDS-only packet I/O. Existing packet-I/O entries retain
their startup class; an initially mixed vmnet session keeps later entries on
vmnet, while an initially empty or all-MMDS session can use MMDS-only packet I/O
for a selected ID. Removal releases the exact queues, retry deadline, metrics
generation, packet-I/O owner, and PCI resources before the ID/MAC/slot can be
reused. Default MMIO rejects runtime PUT/DELETE without mutation.

The configured `mtu` is advertised to the guest virtio-net device. Current
signed Network/MMDS scenarios select every configured interface in MMDS config,
so startup uses process-local MMDS-only packet I/O without opening vmnet; they
do not prove direct vmnet or external packet movement. Non-MMDS-only startup
conditionally uses the internal direct-vmnet foundation, which requires
Apple's restricted networking authorization plus operator-owned firewall,
routing/NAT, resource, and distribution policy. See the
[compatibility scope](docs/firecracker-compatibility.md#internal-network-interface-configuration),
[vmnet security boundary](docs/security.md#vmnet-host-policy-boundary), and
[testing guide](docs/testing.md) for the exact supported subset and exclusions.
Contained startup and runtime insertion additionally enforce the authenticated
lifecycle-v5 mode, bridge name, and actual live-vmnet count before backend
construction. The current networkless production profile rejects every positive
vmnet authority before worker spawn, but supports all-MMDS startup and hotplug
without that authority. This is not a production-connectivity claim.

Record a pre-boot vsock configuration:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/vsock \
  -H 'Content-Type: application/json' \
  -d '{"guest_cid":3,"uds_path":"./v.sock"}'
```

Virtio-vsock is an **implemented supported live MMIO-or-PCI startup/Unix-socket subset**.
Repeated valid pre-boot `PUT /vsock` requests replace the stored
configuration; post-start PUT is rejected without mutation, and there is no
PATCH, DELETE, runtime hotplug, or broader CID-routing contract. The live path
uses dynamic 64-KiB credit windows with wrapping counters, two-second
request/shutdown cleanup, up to 256 connections per direction, `EVENT_IDX`, and
process-local listener ownership with path/payload-redacted transport
diagnostics. Signed Apple Silicon tests verify at least 1 MiB in each direction
for both initiation paths plus two-stream isolation. Indirect descriptors are a
supported bangbang extension. Native-v1 snapshot UDS override, event-queue
`TRANSPORT_RESET`, and post-restore RX gating remain explicit exclusions; this
does not claim general performance, Firecracker artifact, or snapshot parity.

Configure metrics output before boot:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/metrics \
  -H 'Content-Type: application/json' \
  -d '{"metrics_path":"/tmp/bangbang.metrics"}'
```

Configuring the sink does not write before a VM session exists. The first
retained session causes one best-effort initial JSON line, regardless of
whether CLI, config-file, or API configuration supplied the sink. The same
process writes every 60 seconds in both `Running` and `Paused`, supports the
explicit runtime `FlushMetrics` action, and makes one best-effort
normal-terminal attempt while it still owns live diagnostics. Initial,
periodic, and terminal sink failures never replace the action, loop, or process
result; explicit `FlushMetrics` remains runtime-only and returns a configured
sink failure to its caller. Lines can include a `boot_run_loop_status` store
such as `running`, `paused`, `exited`, or `failed`. When startup timing CLI values are provided,
the same metrics output includes Firecracker-style
`api_server.process_startup_time_us` and
`api_server.process_startup_time_cpu_us` elapsed values. `--start-time-us` is
subtracted from the sampled monotonic clock, `--start-time-cpu-us` is
subtracted from the sampled process CPU clock, and `--parent-cpu-time-us`
contributes to the CPU value without being serialized as a separate field. If a
provided start timestamp is later than the sampled clock value, the elapsed
component saturates at zero. The current
Firecracker-shaped API request metrics subset also reports selected GET counters
under `get_api_requests`; parsed core
configuration, MMDS, observability, memory hotplug, pmem, and `/actions`
counters under `put_api_requests`; parser failures, including malformed bodies
and path/body ID mismatches, for those PUT endpoints with matching
Firecracker-style fields in the matching
`put_api_requests` count/fail counters; and selected PATCH counters including
memory hotplug and pmem under `patch_api_requests`, including parser failures
for those PATCH endpoints. bangbang also records
bangbang-specific `balloon_count` API request counters for parsed balloon GET,
PUT, and PATCH routes, plus `balloon_fails` counters for parsed balloon PUT and
PATCH failures and identifiable malformed balloon PUT/PATCH parser failures,
because Firecracker does not expose matching balloon API request metric fields.
Runtime metrics flushes can also include a top-level aggregate `block` object
and non-empty per-drive `block_{drive_id}` objects for implemented virtio-block
queue activity, read/write latency aggregates, backing update counters, and
failures; a top-level aggregate `pmem` object and non-empty per-device
`pmem_{id}` objects for implemented virtio-pmem queue activity and failures;
top-level aggregate `net` and non-empty per-interface
`net_{iface_id}` objects for implemented virtio-net RX/TX queue activity,
packet counts, byte counts, and failures; a top-level `mmds` object for
implemented guest MMDS packet detour and response queue activity; a top-level
`vsock` object for implemented virtio-vsock RX/TX queue activity, packet
counts, byte counts, connection cleanup counters, and classifiable queue/event
failures; a top-level `entropy` object with Firecracker-shaped counters for
implemented virtio-rng request, byte, host-randomness failure, and event-failure
activity; a
top-level `uart` object with Firecracker-shaped serial counters for implemented
TX writes, missed writes, output errors, and rate-limiter drops; a top-level
`signals` object with `sigpipe` counts for handled non-terminating `SIGPIPE`;
plus a top-level `balloon` object for implemented virtio-balloon activity and
failures. Balloon metrics distinguish inflate, free-page-hint, and free-page-
report discard attempts, bytes whose Darwin host-page interiors completed
zero/free advice, partial-edge bytes skipped to protect neighboring guest data,
and failed attempts. Reporting also exposes its requested byte total separately
from advised bytes, so accepted guest descriptors never imply that the host
reclaimed the complete range. Darwin discard is best effort and does not promise
a synchronous process-footprint reduction.

All implemented API, logger, signal, UART, and device counts, byte totals,
failures, errors, limiter activity, and block-latency `sum_us` are interval
increments. Startup timing, boot status, the latest lifecycle/snapshot action
latencies, and block-latency `min_us`, `max_us`, and `sample_count` are stores.
The typed baseline advances only after a complete successful write. A new or
lower producer generation emits its full current value; new, disappeared, and
reappearing keyed devices follow the same rule. Empty device families stay
sparse rather than appearing as fake all-zero Firecracker objects. An ambiguous
write error retains the old baseline, so a later success replays the interval
at least once. Every successfully completed line includes bangbang's extension
`vmm.metrics_flush_count: 1`.

Parsed deprecated HTTP API
usage is counted under `deprecated_api.deprecated_http_api_calls` for supported
deprecated machine `cpu_template`, MMDS V1 config, `vsock_id`, and snapshot-load
field forms.
After a metrics write failure, later successful output includes
`logger.missed_metrics_count`; failed API request/action/boot-timer logger
delivery appears in `logger.missed_log_count`; and denied boot-timer records
appear in `logger.rate_limited_log_count`. These are interval counters under the
same successful-baseline rule.

Configure logger output before boot:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/logger \
  -H 'Content-Type: application/json' \
  -d '{"log_path":"/tmp/bangbang.log","level":"Info","module":"bangbang_runtime","show_level":true,"show_log_origin":true}'
```

No logger sink is configured by default. A configured nonblocking file/FIFO
sink records successfully parsed API request method/path lines without request
bodies, plus successful `InstanceStart` and explicit `FlushMetrics` action
events. These host records are unrestricted by the guest limiter. `show_level` adds `level=Info`, and
`show_log_origin` adds the callsite as `origin=<file>:<line>`.
`module` filters these logger events by prefix against
`bangbang_runtime::api_server`, `bangbang_runtime::vmm_action`, or
`bangbang_runtime::boot_timer`.

When `--boot-timer` is enabled, its guest-triggered callsite admits an initial
burst of ten records, refills at one record per 500 ms across a five-second
budget, counts every denied record, and emits one unrestricted warning before
the next admitted boot-time record. Filtered or unconfigured records consume no
budget. Sink contention, poisoning, write, or flush failure is best effort:
`missed_log_count` changes, but the API, action, startup, or guest MMIO result
does not. Bangbang does not claim process-global panic/fatal durability,
rotation, syslog, journald, tracing, or remote telemetry.

Serial output is independently configured before boot with `PUT /serial`.
Omitting or clearing `serial_out_path` keeps TX in a bounded 64-KiB internal
buffer instead of stdout; a configured file/FIFO is opened nonblocking with
path-redacted errors. An optional token bucket drops exhausted bytes without
sleeping or failing the guest write and reports the drop count in `uart`
metrics. There is no public serial RX, stdin route, or streaming API. The
bangbang-native v1 profile captures default serial MMIO metadata/registers but
restores a fresh output buffer and does not capture a public path, buffered or
in-flight bytes, limiter state, or UART counters.

The exact field classes, failure semantics, and native-v1 boundary are in
[Firecracker Compatibility Scope](docs/firecracker-compatibility.md#firecracker-v1160-observability-contract).

Submit an `InstanceStart` action:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/actions \
  -H 'Content-Type: application/json' \
  -d '{"action_type":"InstanceStart"}'
```

See [Firecracker Compatibility Scope](docs/firecracker-compatibility.md) for
the full endpoint matrix, implemented behavior, and deferred Firecracker
features. See [Firecracker Validation Matrix](docs/firecracker-validation-matrix.md)
for the support status and validation layer summary. The
[v1.16.0 capability inventory](compat/firecracker/v1.16.0/README.md) is the
mechanically checked scope authority for exhaustive compatibility work. Its 381
generated source identities and 37 local semantic identities form a 418-record
delivery overlay with 164 implemented-and-verified, 234 audit-required, three
missing-platform-feasible, and 17 proven-platform-impossible outcomes. The
[machine and lifecycle closure ledger](compat/firecracker/v1.16.0/machine-lifecycle-audit.md)
records the completed Wave 2 subset and the explicit Wave 6 snapshot, Wave 7
tooling/specification, and Wave 8 final-certification handoffs. Nonterminal
entries do not make new runtime claims. The
[storage closure ledger](compat/firecracker/v1.16.0/storage-contract.md)
records its exact 38-terminal/two-Wave-6 split, and the
[balloon closure ledger](compat/firecracker/v1.16.0/balloon-contract.md)
records its exact 50-terminal/two-Wave-6 split.

## Build And Test

Requires the latest stable Rust toolchain.

```sh
cargo fmt --all -- --check
cargo run -p bangbang-firecracker-capability-audit --locked -- validate
cargo check --workspace --all-targets --all-features --locked
cargo check -p bangbang-launcher --all-targets --all-features --locked --target aarch64-unknown-linux-musl
cargo test --workspace --all-targets --all-features --locked --exclude bangbang-hvf
cargo test -p bangbang-hvf --lib --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo clippy -p bangbang-launcher --test production_bundle_e2e --all-features --locked --target aarch64-apple-darwin -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked
```

Run signed HVF integration tests on macOS Apple Silicon:

```sh
scripts/run-integration-tests.sh
```

Run the integration-only App Sandbox boundary on its own:

```sh
scripts/run-integration-tests.sh --test app_sandbox
```

This target packages real test binaries as minimal app bundles, runs the full
HVF lifecycle suite with App Sandbox plus Hypervisor entitlements, and checks
that the real executable accepts an app-container API socket while rejecting
the default `/tmp` socket and outside configuration paths. It validates an
Apple containment building block, not a production sandboxed distribution.

Build and run the separately signed production launcher/worker boundary on its
own:

```sh
scripts/run-integration-tests.sh --test production_bundle
```

This target verifies exact identifiers, entitlements, Hardened Runtime, strict
static and live-worker validation, tamper rejection, the descriptor allowlist,
closed worker environment, lifecycle-v5 launch-policy authentication, canonical
default-denied vmnet policy and networkless-profile rejection, exact and
kernel-enforced resource limits, private-root entry, jailer help/version/parser
rejection, fixed redacted pre-mutation rejection of every exact/attached Linux
cgroup/network/PID-namespace name, foreground compatibility, daemon
readiness/PID/stdio/session
ownership, pre-ack parent-loss cancellation, concurrent daemon isolation,
malformed-bootstrap rejection, container-only path denial and redaction,
structured API/no-API readiness and cancellation, worker-first/launcher-first
namespace cleanup, empty both-killed namespace recovery, concurrent-session
isolation, owned-socket
cleanup, mandatory empty-grant startup, typed read-only/write-only/directory
grants, mismatch rollback, grant-phase cancellation/deadline behavior,
grant-bearing crash/concurrency isolation, absence of the test exerciser from
the normal production build, exact external config/metadata/kernel/initrd
adoption by the normal worker, config-file and delayed API block/pmem adoption,
startup-CLI/config-file and delayed-API logger/metrics/serial adoption,
pathname-replacement identity, exact role/access and one-time failures,
read-only guest-write rejection, writable block persistence, direct read-only
pmem root boot from the exact granted descriptor after pathname replacement,
pmem read/flush,
guest console output through the transferred serial descriptor, terminal
metrics, concurrent output-session isolation, preauthorized live block
replacement, limiter-only backing retention, redacted failure atomicity, and
granted native-v1 create/describe/state-memory-root restore, strict snapshot
staging cleanup after worker death, and real sandboxed HVF guests through
`SYSTEM_OFF`. It also proves an
outside-container client can use a granted API socket, and that a real guest
can complete deterministic bidirectional and half-close/EOF vsock traffic in
both initiation directions through the supplied granted listener and fixed
launcher broker, without changing the exact entitlements or leaving a helper
in steady state. Signed contained-vhost cases boot a vhost root plus scratch
child alongside vsock from one connect-only directory grant, prove scratch
read/write/flush and guest-observed ID-only capacity refresh on the existing
stream, and exercise all-PCI runtime target rejection, negotiation rollback,
new-ID attach, manual guest removal, DELETE, Paused same-ID reuse through a
second exact child, and exact stream closure. They likewise leave no helper or
entitlement change. Abrupt launcher-first and worker-first cases replace the
granted API pathname before death and prove both surviving cleanup owners
preserve the replacement while clearing the matching private namespace record.
Signed file-backed Async cases separately cover direct MMIO live path PATCH,
config-file startup, two concurrent Async root/data drives, first-use PCI
hotplug, DELETE/reuse, and paused same-ID Sync-to-Async replacement. Normal
production cases repeat contained Async root/control startup, preauthorized
same-ID backing/engine replacement, limiter PATCH, and runtime
hotplug/DELETE/reuse without reopening grant tags.

Prepare the pinned Firecracker arm64 Linux kernel artifact used by guest boot
validation work:

```sh
scripts/fetch-firecracker-kernel.sh
```

Run only the minimal guest boot integration test on macOS Apple Silicon:

```sh
scripts/run-integration-tests.sh --test guest_boot
```

Hosted macOS CI may build and sign integration tests without executing HVF:

```sh
scripts/run-integration-tests.sh --allow-unsupported
```

See [Testing Guide](docs/testing.md) for test layering, signed integration-test
rules, guest boot artifact caching, and local verification expectations.

## Exit Status

- `0`: help or version completed successfully, the API server exited without
  error, or no-api mode handled `SIGINT`/`SIGTERM`.
- `152`: startup configuration failed before the process entered runtime,
  including config-file, metadata, logger-sink, and metrics-sink configuration
  failures. This matches Firecracker's bad-configuration exit
  code.
- `153`: startup argument parsing failed before process configuration began.
  This matches Firecracker's argument-parsing exit code.
- `148`, `149`, `150`, `151`, `154`, `156`, `157`: Firecracker-compatible
  fatal or restricted host signal exits for `SIGSYS`, `SIGBUS`, `SIGSEGV`,
  `SIGXFSZ`, `SIGXCPU`, `SIGHUP`, and `SIGILL`.
- `1`: process failure, including API socket bind, shutdown signal handling, API
  accept failures, or process-owned runtime failures.
