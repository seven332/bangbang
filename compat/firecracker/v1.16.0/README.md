# Firecracker v1.16.0 capability inventory

This directory is the structural scope authority for bangbang's Firecracker
v1.16.0 compatibility work. The baseline is commit
`d83d72b710361a10294480131377b1b00b163af8`.

The inventory complements, rather than replaces, the detailed behavior in
[`docs/firecracker-compatibility.md`](../../../docs/firecracker-compatibility.md)
and the test-layer summary in
[`docs/firecracker-validation-matrix.md`](../../../docs/firecracker-validation-matrix.md).
Those documents explain behavior; this directory makes omissions and terminal
claims mechanically visible.

## File ownership

- [`source-manifest.json`](source-manifest.json) is machine-owned. It records
  the pinned upstream inputs and exact identities for 26 Swagger paths, 38
  operations, 44 definitions, 152 properties, 23 configured Firecracker
  arguments, three non-Swagger DELETE routes, 14 public-tool operations, 41
  public-tool arguments, and 40 explicit non-Swagger source-corpus items.
- [`capabilities.json`](capabilities.json) is human-owned. Every generated
  identity has exactly one overlay, and additional `semantic.*` records cover
  cross-leaf guest, lifecycle, snapshot, observability, isolation, and
  specification behavior.
- [`process-contract.md`](process-contract.md) is the human-owned semantic
  audit for the 23 configured Firecracker arguments and the composite process
  records. It traces arity, defaults, relationships, observable behavior,
  cross-family ownership, implementation, and executable validation without
  expanding the machine-owned identity extractor.
- [`isolation-contract.md`](isolation-contract.md) records the production
  macOS bundle/worker boundary, its executable evidence, and the remaining
  #1351 isolation/resource/seccomp outcomes without treating them as direct
  Linux jailer parity.
- [`cpu-template-contract.md`](cpu-template-contract.md) records the bounded
  reviewed ID/ACTLR/core/SIMD/FP custom profile, transactional static/custom
  selection, strict KVM/static execution exclusions, public OS availability,
  startup/readback/boot-precedence/cleanup order, snapshot boundary, and Wave 7
  helper/portability handoffs.
- [`machine-lifecycle-audit.md`](machine-lifecycle-audit.md) is the #1388
  closure ledger. It accounts for the original 28 Wave 2 records, directly
  related API aggregates, exact evidence, count arithmetic, and explicit Wave
  6/7/8 ownership without changing the generated source manifest.
- [`device-hotplug-contract.md`](device-hotplug-contract.md) is the
  #1420/#1421/#1422/#1423 runtime block, pmem, network, and aggregate ledger. It binds the
  promoted PUT/path/DELETE identities to owner-thread transactions, contained
  grant or vmnet-authority rollback, dynamic pmem mapping, per-entry network
  packet I/O, guest lifecycle, signed evidence, shared capacity/identity, and
  the completed live aggregate boundary.
- [`storage-contract.md`](storage-contract.md) is the #1471 aggregate storage
  ledger. It pins the exact 40-record family, 38 terminal outcomes, the two
  Wave 6 pmem snapshot handoffs, field-specific implementation evidence, and
  signed direct/production coexistence and cleanup proof.
- [`balloon-contract.md`](balloon-contract.md) is the #1473 balloon ledger. It
  pins all 52 records, promotes the 50 complete API/path/property/schema leaves,
  records publication-safe paired PFN accounting and coherent MMIO/PCI
  capture-ready ownership, and hands exactly two serialization/restore
  aggregates to Wave 6.
- [`memory-hotplug-contract.md`](memory-hotplug-contract.md) is the #1474
  virtio-mem ledger. It pins all 19 records, promotes the 17 complete
  API/path/property/schema leaves, records singleton metrics and exact
  shared-aperture/HVF/dirty capture ownership, and hands exactly two
  serialization/restore aggregates to Wave 6.
- [`entropy-contract.md`](entropy-contract.md) is the #1475 virtio-rng ledger.
  It pins all seven records, promotes the five complete
  API/path/property/schema leaves, records dual-bucket retry and exact
  MMIO/PCI capture-ready ownership, and hands exactly two
  serialization/restore aggregates to Wave 6.
- [`serial-contract.md`](serial-contract.md) is the #1479 serial stdio ledger.
  It pins all six records, promotes the five complete API/path/property/schema
  leaves, records default stdout, configured-output stdin exclusion, bounded
  terminal/FIFO RX and exact MMIO capture-ready ownership, and hands the one
  serialization/endpoint-reconstruction aggregate to Wave 6.
- [`time-identity-contract.md`](time-identity-contract.md) is the #1477/#1478/#1480
  PL031, VMGenID, VMClock, and PVTime ledger. It records complete
  typed VMClock capture, backward-compatible native-v1 state, ordered
  destination notifications, postcommit terminality, fresh destination PL031
  time, public per-vCPU PVTime measurement/accounting/publication/discovery,
  capture-ready cumulative values, and signed Linux proof while handing the one
  aggregate record to Wave 6 #1490 for clone/portability certification.
- [`remaining-device-contract.md`](remaining-device-contract.md) is the #1481
  checked aggregate ledger. It joins the exact 52 balloon, 19 memory-hotplug,
  seven entropy, six serial, and one time/identity records; pins the 77 terminal
  and eight Wave 6 outcomes; and binds focused, signed MMIO/PCI, and production
  serial-isolation evidence without changing family semantics.

Regeneration may produce a candidate `source-manifest.json`; it must never
create or rewrite a capability disposition, owner, evidence reference,
delivery issue, or Challenge result. A changed generated identity instead
causes a missing or stale overlay validation failure for a reviewer to resolve.

Stable source IDs use `<kind>:<upstream-key>`. Semantic IDs use the lowercase
`semantic.<namespace>:<slug>` form. IDs are scoped to this immutable v1.16.0
baseline. A later Firecracker baseline gets a separate directory and an
explicitly reviewed delta.

## Dispositions

Each capability has exactly one disposition:

- `audit-required` means the exact contract still needs review under the
  strict parent rule. It is allowed while delivery is in progress and is never
  a completion state.
- `missing-platform-feasible` requires a concrete delivery issue. It is never
  a completion state.
- `implemented-and-verified` requires implementation and validation
  references appropriate to the claim. Parser recognition or a stable
  unsupported response is not implementation.
- `proven-platform-impossible` requires the upstream contract, authoritative
  platform evidence, alternatives with rejection reasons, stable behavior,
  focused tests, compatibility and security documentation, and a current
  Challenge result linked as its GitHub issue comment.

The initial inventory is deliberately conservative. Existing prose or issue
closure does not automatically promote a record from `audit-required`.

The #1352 process audit, #1368 snapshot-description delivery, and #1419 PCI
startup delivery promote exactly 22 of the 29 process-family records: 20
complete argument leaves plus the complete CLI/readiness and
signal/exit/fd/cleanup semantics. #1384 additionally classifies the two seccomp
argument leaves as `proven-platform-impossible`. One argument leaf, the
snapshot-containing identity/output semantic, the aggregate run operation, and
both broad source corpora remain `audit-required`. The checked
[`process-contract.md`](process-contract.md) records those five handoffs; a
partially implemented composite is not a terminal claim.

The #1354 production-boundary audit moves exactly three composite isolation
records to `missing-platform-feasible` with #1351 as their delivery owner. It
does not terminally promote them: external resource authority, authenticated
brokerage, vmnet policy, crash coupling, deployment identity, and complete
jailer/seccomp outcome classification remained incomplete at that checkpoint. The broad source
corpus records remain `audit-required`. The checked
[`isolation-contract.md`](isolation-contract.md) separates the delivered
package/sandbox/supervisor subset from those handoffs.

The #1365 socket-directory slice adopts the API and vsock directory roles with
an exact safe-child grammar, same-filesystem anchored exclusive publication,
strict ownership records, supplied listeners, and one fixed session-bound
launcher facet for guest-initiated vsock port connections. It adds no worker
entitlement or steady-state helper and does not terminally promote the three
composites; at that point snapshot authority, general dynamic brokerage and hard revocation,
vmnet policy, Linux outcome classification, and deployment identity still
remain under #1351.

The #1368 snapshot-resource slice adopts the read-only describe/state/memory
inputs, any grant-tagged persisted read-only root backing, and repeatable
snapshot-output directories with bounded UTF-8 children. State preinspection
does not consume authority; final state/memory/root adoption is atomic. Granted
publication stays anchor-relative and no-clobber, while strict per-artifact
ownership records let a surviving launcher clean an exact staging inode after
worker death without deleting a replacement. The unavoidable create-before-
record window, simultaneous uncatchable launcher/worker death, broader native
snapshot profiles, general brokerage/hard revocation, network policy, Linux
outcome classification, and deployment identity remain outside this slice.

The #1370 launch-control slice promotes exactly five jailer argument leaves:
`id`, fixed embedded `exec-file`, repeatable `resource-limit`, `daemonize`, and
the early `version` command. Lifecycle v3 authenticates a fixed redacted worker
policy; the worker receives no ambient parent environment, installs exact
`RLIMIT_FSIZE`/`RLIMIT_NOFILE`, and descriptor-enters its private namespace
before `Prepared`. A same-code signed launcher re-exec supplies bounded
Ready/PID acknowledgment and retained daemon supervision. Signed tests exercise
real descriptor/file-size exhaustion, pre-ack parent loss, post-ack signals,
and concurrent daemon isolation. The complete 417-record delivery inventory is
therefore, at that checkpoint, 26 `implemented-and-verified`, 388 `audit-required`, and three
`missing-platform-feasible`. Arbitrary uid/gid, configurable chroot, cgroups,
network/PID namespaces, seccomp, aggregate jailer operation/corpus, general
brokerage, vmnet, and deployment identity remained nonterminal under #1351.

The #1383 offline-seccompiler slice promotes exactly seven isolation records:
the complete pinned `seccompiler` corpus, its `compile` operation, and the
`basic`, `input-file`, `output-file`, `split-output`, and `target-arch`
arguments. The host-side tool preserves the v1.16 policy transform, bad-
architecture action, bitcode 0.6.9 combined format, raw split files, default
name, size cap, and public argument spellings while adding bounded redacted I/O
and transactional publication. It does not install a filter. The install-helper
language in pinned `docs/seccompiler.md` describes the current Linux VMM
consumer owned by `corpus:seccomp`; that runtime work passed to #1384. At the
#1383 checkpoint the 417-record delivery inventory contained 33
`implemented-and-verified`, 381 `audit-required`, and three
`missing-platform-feasible` records.

The #1384 runtime-isolation slice certifies exactly eight
`proven-platform-impossible` records: `corpus:seccomp`, both Firecracker runtime
seccomp arguments, and the five jailer cgroup/network/PID-namespace arguments.
Each record binds its pinned Linux kernel contract to current Apple SDK/XNU
evidence, rejected native aliases, fixed pre-mutation behavior, focused tests,
documentation, and the current Plan Challenge. The executable never opens a
rejected filter path; the launch-policy parser returns a closed fixed-name error
before grants, profile/staging, session creation, spawn, publication, or worker
execution. Broader jailer, design, getting-started, production-host, aggregate,
and composite records retain their independent handoffs. The 417-record
delivery inventory is now 33 `implemented-and-verified`, 373 `audit-required`,
three `missing-platform-feasible`, and eight
`proven-platform-impossible` records.

Issues #1389 and #1390 subsequently promote the topology-wide pause/resume and
complete snapshot-quiescence lifecycle records. #1391 promotes the individual
MachineConfiguration vCPU, target-bounded memory, and aarch64 SMT leaves and
certifies the exact `2M` property plus pinned hugepages corpus as public arm64
macOS/XNU/HVF platform exclusions. #1392 adds the verified arm64 cache
presentation record. #1393 implements the bounded four-ID-register custom
template subset and certifies exactly seven narrow KVM/static schema leaves as
platform-impossible: machine `cpu_template`, `CpuTemplate`, KVM capabilities,
KVM vCPU-init features, both `VcpuFeatures` properties, and its schema. #1402
adds the width-exact U64 core, U128 Q, and U32 FP transaction. #1403 completes
the finite arm64 system policy with eleven ID registers, ACTLR.EnTSO, a public
macOS 15.2 preflight for ZFR0/SMFR0, and terminal value-free classification for
every other KVM/public-HVF family. It promotes exactly six parent-owned ARM
records: both `ArmRegisterModifier` properties, `CpuConfig.reg_modifiers`,
`FullVmConfiguration.cpu-config`, and the `ArmRegisterModifier` and `CpuConfig`
schemas. #1395 and #1396 add the signed HVF first-write primitive and complete
shared dirty epochs, including public machine/load activation and Full commit
reset. #1408 then performs the final #1388 audit: it promotes the three
remaining bounded boot/lifecycle records and 18 single-purpose boot-source,
machine, CPU, and VM-state API identities. The generated manifest remains 381
identities; with 37 local semantic records, the #1408 418-record delivery
overlay was 73 `implemented-and-verified`, 325 `audit-required`, three
`missing-platform-feasible`, and 17 `proven-platform-impossible` records. Wave
6 retains generalized snapshots and portability, Wave 7 retains the broad
CPU/rootfs corpora, public `cpu-template-helper`, and applicable specification
outcomes, and Wave 8 retains final cross-capability/export certification. The
exact identities and boundaries are recorded in the
[`machine-lifecycle-audit.md`](machine-lifecycle-audit.md) ledger.

#1420 subsequently promotes exactly two storage API identities: the Swagger
`PUT /drives/{drive_id}` operation and pinned non-Swagger bodyless
`DELETE /drives/{drive_id}` route. Their post-start behavior is restricted to
the public all-virtio PCI profile and is verified through direct and contained
two-round guest attach/remove/reuse. The broad device-hotplug corpus and
aggregate semantic record remain nonterminal pending the pmem and network
slices. The exact boundary is recorded in
[`device-hotplug-contract.md`](device-hotplug-contract.md).

#1421 subsequently promotes exactly three pmem API identities: the Swagger
`PUT /pmem/{id}` operation, the aggregate `/pmem/{id}` path whose PUT/PATCH/
DELETE supported profile is now complete, and the pinned non-Swagger bodyless
`DELETE /pmem/{id}` route. Transactional direct and contained signed gates
prove dynamic HVF mapping, guest flush, teardown, and exact same-ID/PCI-slot/
guest-range reuse. At that checkpoint the overlay was 76
`implemented-and-verified`, 322 `audit-required`, three
`missing-platform-feasible`, and 17 `proven-platform-impossible` records. The
broad device-hotplug corpus and aggregate semantic record were nonterminal at
that checkpoint pending the independent network slice.

#1422 subsequently promotes exactly two network API identities: the Swagger
`PUT /network-interfaces/{iface_id}` operation and the pinned non-Swagger
bodyless `DELETE /network-interfaces/{iface_id}` route. Transactional direct
and networkless-production signed gates prove Running/Paused attach, guest PCI
rescan, real MMDS exchange, sysfs removal, teardown, contained non-MMDS denial,
and exact same-ID/MAC/PCI-slot reuse without vmnet authority. At that checkpoint
the overlay was 78 `implemented-and-verified`, 320 `audit-required`,
three `missing-platform-feasible`, and 17 `proven-platform-impossible`
records.

#1423 subsequently certifies the shared 31-slot resource budget, type-scoped
cross-device IDs, duplicate-MAC policy, mixed Running/Paused mutation order,
concurrent owner serialization, repeated reuse, and success-authoritative live
configuration. It terminalizes exactly `corpus:device-hotplug`,
`semantic.hotplug:runtime-device-manager`, and
`semantic.transport:pci-msi-and-coexistence`. At that checkpoint the overlay was
81 `implemented-and-verified`, 317 `audit-required`, three
`missing-platform-feasible`, and 17 `proven-platform-impossible` records.
Native-v1 PCI persistence and external vmnet evidence remain respectively
later-snapshot and #1351/#1378-owned.

#1444 subsequently promotes exactly three pmem API properties:
`Pmem.path_on_host`, `Pmem.read_only`, and `Pmem.root_device`. Direct MMIO/PCI
and normal contained signed gates prove one authoritative file/private-tail
mapping, exact descriptor identity, read-only guest protection, writable
coherence, deterministic root command lines, and exact-prefix persistence.
The overlay therefore contains 84 `implemented-and-verified`, 314
`audit-required`, three `missing-platform-feasible`, and 17
`proven-platform-impossible` records.

#1445 records direct pre-boot vhost-user block startup in the nonterminal
`Drive.socket`, `corpus:block-vhost-user`, and aggregate storage summaries.
Strict direct configuration, bounded discovery, shared-memory/vring transfer,
MMIO/PCI root and scratch I/O, flush, metrics, cleanup, backend death, and
pre-artifact snapshot rejection are implemented and signed. No disposition is
promoted yet.

#1447 extends those same nonterminal records with pinned runtime behavior:
ID-only PATCH performs repeated exact CONFIG acquisition and one MMIO/PCI guest
configuration notification; an already-shared all-PCI VM may attach a new
non-root direct backend in Running or Paused state after preconnection owner
preflight; and caller-coordinated DELETE releases the complete endpoint for
same-ID/slot reuse. Signed evidence covers Linux capacity refresh, guest I/O,
Paused mutation, invalid negotiation rollback, duplicate and anonymous-profile
zero-connect rejection, teardown, and reuse. Live same-ID PUT remains a
duplicate as in pinned v1.16. At that checkpoint contained authorized stream
delivery, vhost snapshot state, Async, and complete broad-corpus semantics
remained owned by later slices, so the inventory counts were unchanged.

#1449 extends those same nonterminal records plus the isolation summaries with
contained vhost-user block authority. Lifecycle v5 retains a dedicated broker
facet; a repeatable `VhostUserSocketDirectory + ConnectChildren` grant owns one
exact anchored directory while per-drive leases name bounded children. Startup
and eligible all-PCI runtime PUT obtain only authenticated connected streams,
normal broker failure is retryable, owner/startup preflight makes zero requests,
ID-only PATCH reuses the active stream, and DELETE releases a child lease while
retaining directory authority for later same-ID reinsertion. Signed production
evidence boots an exact vhost root and scratch child alongside vsock from one
grant, proves scratch read/write/flush and guest-observed active resize, and
uses an all-PCI shared-memory guest to cover invalid-target and negotiation
rollback, new-ID attach, duplicate zero-connect rejection, manual removal,
DELETE, Paused same-ID reuse through another child, resumed I/O, and exact
closure without a steady-state helper or entitlement change.
At that checkpoint snapshot state, Async/io_uring, dynamic-memory coexistence,
and the broad vhost/storage aggregates remained nonterminal, so the inventory
counts were unchanged.

#1446 promotes exactly `api-property:Drive.io_engine` and
`corpus:block-io-engine`. Regular-file and exact macOS block-special drives now
accept default `Sync` or explicit `Async` over MMIO/PCI with direct paths or
contained opened grants.
One lazy bounded portable executor per VM session supplies generation-safe
owner-thread completion, limiter/dirty/status/used/interrupt/metrics
publication, live path and same-ID backing/engine replacement, PCI
hotplug/DELETE/reuse, and orderly reset/shutdown. Four signed executable and two
signed production scenarios cover concurrent devices and the complete public
lifecycle. This is not a claim of Linux io_uring identity; native-v1 Async state
remains excluded before artifact creation. The overlay therefore contains 86
`implemented-and-verified`, 312 `audit-required`, three
`missing-platform-feasible`, and 17 `proven-platform-impossible` records.

#1448 records a complete, redacted capture-ready storage handoff without
changing any disposition. The paused HVF owner reconciles every configured
startup/runtime block and pmem device with one authoritative live MMIO/PCI
owner, captures exact regular-file backing, pmem mapping, limiter/retry, queue,
transport, PCI/MSI-X, and origin state, and performs one stop-all/drain-all/
publish-all/capture-all/resume-all transaction for Async generations. It scans
vhost-user owners first and returns a typed pre-artifact unsupported result.
Native-v1 bytes/load, PCI/dynamic persistence, migration, and vhost snapshot
support remain Wave 6 work, so the overlay remains 86
`implemented-and-verified`, 312 `audit-required`, three
`missing-platform-feasible`, and 17 `proven-platform-impossible` records.

#1461 extends the already implemented drive operation to an existing regular
file or exact macOS block-special descriptor. Direct control uses public disk
geometry/cache ioctls; contained BBG2 grants bind exact identity/access/status/
geometry and descriptor 7 exposes only fixed, session-bound fresh inspect and
cache-sync operations on the launcher's retained descriptor because App
Sandbox denies those ioctls in the worker. Four signed direct/contained MMIO/PCI
scenarios certify complementary Sync/Async, Unsafe/Writeback,
read-only/read-write, limiter retry, 4/6/8-MiB configuration refresh, GET_ID,
regular/block replacement, guest persistence, capture rejection, DELETE/reuse,
unchanged entitlements, and exact fixture cleanup. Native-v1 remains
regular-only. At that checkpoint the broad
`semantic.storage:block-sync-async-vhost-and-limits` record remained
`audit-required` for #1450, so disposition counts were unchanged.

#1462 extends the nonterminal `Drive.socket`, `FullVmConfiguration.memory-
hotplug`, `corpus:block-vhost-user`, `corpus:memory-hotplug`, and aggregate
memory/storage summaries with their combined lifecycle. Virtio-mem startup now
owns one sparse shared reservation for the complete deterministic aperture;
only plugged offset views enter guest CPU/HVF mappings, dirty state, and current
accounting. Initial and eligible runtime direct or contained vhost frontends
receive one immutable table containing boot RAM plus that aperture, so the
trusted external backend can access currently unplugged bytes without exposing
unrelated mappings. Signed MMIO/PCI direct and production-bundle scenarios prove
both configuration orders, storage I/O across grow/shrink, exact stable region
geometry, CONFIG refresh, backend death, Running/Paused attach/delete/reuse,
unchanged entitlements, no helper, and unchanged pre-artifact snapshot
rejection. Darwin memfd sealing, a bundled production backend, backend policy,
optional-device persistence, and broad storage promotion were not claimed by
that slice. At that checkpoint the six broad records remained
`audit-required`: #1450 still owned aggregate storage certification and Wave 6
owned optional-device persistence, so inventory counts were unchanged.

#1471 completes the #1450 aggregate storage certification. One direct signed
executable and one signed production App Sandbox bundle run Sync, portable
Async, vhost-user, and pmem together with virtio-mem through concurrent
disjoint PATCH, pause/resume, memory grow/shrink, Async backing replacement,
serialized block/pmem attach/remove/reuse, persistence, exact owner-capacity
reuse, and terminal or orderly cleanup. The contained case uses only existing
exact file grants plus a connect-only vhost directory, proves pathname
replacement resistance, redaction, child/frontend/session cleanup, unchanged
entitlements, and no helper. Owner capacity preflight now rejects before a
vhost request, pmem grant claim, direct open/map, or public configuration
change. The checked [`storage-contract.md`](storage-contract.md) terminalizes
38 of the exact 40 records: the previous ten remain terminal and 24 API
records, three block corpora, and the block semantic aggregate are promoted.
Exactly `corpus:pmem` and
`semantic.storage:pmem-root-mapping-flush-and-state` remain
`audit-required` for Wave 6 optional-device serialization/restore. At that
storage-closure checkpoint the overlay was 114 `implemented-and-verified`, 284 `audit-required`,
three `missing-platform-feasible`, and 17 `proven-platform-impossible` records.

#1473 completes the live and capture-ready balloon slice under #1440. It
prepares fallible compact PFN accounting before used-ring publication, commits
by infallible move, captures bounded validated device/queue/statistics/hinting/
accounting state through the exact paused MMIO or PCI owner, and adds signed
Linux evidence for polling, optional fields, hint acknowledgement/reporting,
pause/capture/resume, target-to-zero convergence, metrics, failure diagnostics,
and cleanup. The checked [`balloon-contract.md`](balloon-contract.md) promotes
all 50 API operation/path/property/schema leaves. Exactly
`corpus:ballooning` and
`semantic.memory-device:balloon-oom-stats-hinting-and-reporting` remain
`audit-required`; Wave 6 owns encoding, artifact integration, restore,
migration/clone behavior, portability, and signed restored-guest outcomes. The
overlay at that checkpoint was therefore 164 `implemented-and-verified`, 234
`audit-required`, three `missing-platform-feasible`, and 17
`proven-platform-impossible` records.

#1474 completes the live, observable, and capture-ready virtio-mem slice under
#1440. One per-device producer exposes Firecracker's exact 18 memory-hotplug
metrics plus separate Bangbang interrupt, rollback, owner-cleanup, and teardown
counters. Supported requests record one count and latency sample at the final
publication boundary; committed bytes, partial or late rollback, post-commit
discard, and owner cleanup remain distinct. Detached MMIO/PCI state validates
feature negotiation, queue geometry and cursors, pending notification and
interrupt state, compact plugged ranges, one exact shared reservation identity,
guest owners, actual HVF maps, dirty tracking, epoch, and byte accounting. The
native-v1 create path performs this paused preflight before publication without
claiming a serialization format. The checked
[`memory-hotplug-contract.md`](memory-hotplug-contract.md) promotes all 17 API
operation/path/property/schema leaves. Exactly `corpus:memory-hotplug` and
`semantic.memory-device:virtio-mem-lifecycle-accounting-and-state` remain
`audit-required`; Wave 6 owns encoding, artifact integration, restore,
migration/clone behavior, portability, and signed restored-guest outcomes. The
current overlay is therefore 181 `implemented-and-verified`, 217
`audit-required`, three `missing-platform-feasible`, and 17
`proven-platform-impossible` records.

#1475 completes the live, observable, and capture-ready entropy slice under
#1440. Request processing caps each writable chain at 64 KiB, obtains bytes
from the host operating system, and exposes seven Firecracker-shaped entropy
metrics. Optional operations and bandwidth buckets retain exactly one
throttled descriptor, wake it at the earliest deadline, and roll back exact
budget/burst/refill-origin state when a post-consumption host-memory or
used-ring failure prevents publication. Detached MMIO/PCI state validates
feature negotiation, activation, exact queue geometry/ranges/cursors, external
limiter configuration, redacted bucket state, the pending descriptor, and
host-time-free retry agreement with the quiesced HVF scheduler. The public
native-v1 create path traverses this state before optional-profile rejection
without claiming an encoding. Signed Linux guests prove marker-gated first and
repeated `/dev/hwrng` reads, real throttling, pause/capture/resume, retry
metrics, and cleanup over both selected transports. The checked
[`entropy-contract.md`](entropy-contract.md) promotes all five API
operation/path/property/schema leaves. Exactly `corpus:entropy` and
`semantic.device:entropy-queues-limits-metrics-and-state` remain
`audit-required`; Wave 6 owns encoding, artifact integration, restore,
migration/clone behavior, portability, and signed restored-guest outcomes. At
that checkpoint the overlay was 186 `implemented-and-verified`, 212
`audit-required`, three `missing-platform-feasible`, and 17
`proven-platform-impossible` records.

#1479 completes the live, observable, and capture-ready serial stdio slice
under #1440. With no configured output path the production process now owns
nonblocking stdout and supported terminal/FIFO stdin; a configured direct or
contained output disables stdin. The owner run loop reads only current UART
capacity, disarms a full 64-byte FIFO, rearms after guest drain, detaches on
EOF/error, retries interrupt delivery, and consumes no input while Paused.
Shared descriptor ownership restores original flags and terminal attributes
only after the final endpoint drops. Detached state pairs exact external
configuration with complete UART registers, RX bytes, status, and pending
intents while excluding host descriptors, pipe buffers, and terminal state.
The public native-v1 create path traverses this state before publication
without claiming a new encoding. Signed direct processes prove default stdout,
greater-than-FIFO stdin, configured-output exclusion, rate limiting,
pause/capture/resume, EOF, concurrency isolation, metrics, and cleanup; the
signed production bundle repeats the default stdio flow across the launcher
and App Sandbox worker boundary. The checked
[`serial-contract.md`](serial-contract.md) promotes all five API
operation/path/property/schema leaves. Exactly
`semantic.device:serial-stdin-stdout-rx-and-restore` remains
`audit-required`; Wave 6 owns optional-device encoding, endpoint
reconstruction, restore, migration/clone behavior, portability, and signed
restored-guest outcomes. The current overlay is therefore 191
`implemented-and-verified`, 207 `audit-required`, three
`missing-platform-feasible`, and 17 `proven-platform-impossible` records.

#1477 completes the native-v1 PL031, VMGenID, and VMClock restore slice under
#1440. New captures append the exact validated 112-byte VMClock ABI in nested
`BANGDEV\0` 1.1.0 state; loaders continue to accept 1.0.0 by deriving the same
typed value from the independently checked memory image. A 1.1.0 encoded value
must exactly equal that image. Restore completes aggregate device, vCPU, GIC,
ICC, timer, and pending-interrupt state first, then writes and notifies a fresh
VMGenID, publishes the odd/fenced/counter/fenced/even VMClock update, and
notifies it before any resume. A completely cleaned precommit failure remains
retryable; any guest-memory or notification commit is terminal. Destination
PL031 reconstruction uses destination wall clock with every unsupported alarm
register clear. Signed cross-process guest code observes both VMGenID halves,
stable VMClock sequence/counters, and non-regressing RTC time. The checked
[`time-identity-contract.md`](time-identity-contract.md) retains
`semantic.device:rtc-vmclock-vmgenid-and-pvtime` as `audit-required`.

#1478 adds the exact 64-byte-per-vCPU PVTime ABI placement, checked startup
initialization and rollback, owner-thread `hv_vcpu_get_exec_time` measurement
with Mach conversion, and policy-gated 64-bit SMCCC dispatch. #1480 adds bounded
runnable wall/execution accounting, dirty-aware atomic little-endian
publication, fail-closed public discovery, topology-ordered paused capture, and
signed Linux contention/idle/pause certification. #1481 completes the aggregate
live/capture-ready reconciliation with one shared MMIO/PCI signed profile,
ordered failure/retry proof, bounded owner-budget reuse, and production
default-stdio session isolation. The checked
[`remaining-device-contract.md`](remaining-device-contract.md) selects exactly
85 records: 77 are terminal and the same eight family aggregates remain
`audit-required`, all under [Wave 6 #1490](https://github.com/seven332/bangbang/issues/1490).
Wave 7 #1491 owns no selected row. No disposition changes, so the overlay
remains 191/207/3/17.

## Commands

Validate checked-in delivery state without an upstream checkout:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- validate
```

The final parent gate rejects `audit-required` and
`missing-platform-feasible`:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --final
```

Compare the generated manifest with an explicit clean Firecracker checkout at
the exact pinned commit:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- compare \
  --firecracker /path/to/firecracker
```

Generate a candidate without overwriting either checked-in inventory file:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- regenerate \
  --firecracker /path/to/firecracker \
  --output codex-work/tmp/firecracker-v1.16-source-manifest.candidate.json
```

The comparison command requires a clean Git worktree whose `HEAD` is the exact
pinned commit. It reads only declared regular files below that canonical root.
The local checkout path is not stored in tracked data. Ordinary CI does not
need a sibling checkout.

## Contributor update rule

Every pull request that changes a Firecracker-facing capability must update
all owned overlay records in the same change. Add implementation and validation
evidence only for the exact observable contract proved by that PR. Keep
unreviewed behavior `audit-required`; use `missing-platform-feasible` only with
a delivery issue; and use `proven-platform-impossible` only after the complete
strict evidence and Challenge gate. Keep capability IDs, source references,
evidence references, and exclusion alternatives in canonical sorted order and
free of duplicates. Local evidence must resolve to a tracked regular file
inside the repository; ignored, untracked, symlinked, and escaping paths fail
validation.

Run the focused validator and the repository's normal checks before submission.
The checked-in integration test also validates this inventory through the
ordinary workspace test command. A corpus reference records audit ownership;
it does not by itself prove that every semantic statement is implemented.

The inventory is not evidence by itself. Every terminal compatibility claim
depends on its referenced production behavior and validation.
