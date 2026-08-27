# Firecracker v1.16.0 Capability Inventory

This directory is bangbang's checked structural scope for Firecracker v1.16.0,
pinned to upstream commit
`d83d72b710361a10294480131377b1b00b163af8`. It answers which upstream
identities were inventoried, how each identity is classified, and which
repository evidence supports a terminal claim.

For reader-facing behavior, start with
[Firecracker Compatibility Scope](../../../docs/firecracker-compatibility.md).
For a compact status index, see the
[Firecracker Validation Matrix](../../../docs/firecracker-validation-matrix.md).
For commands and test-layer selection, see the
[Testing Guide](../../../docs/testing.md#firecracker-capability-inventory).

## File Ownership

- [`source-manifest.json`](source-manifest.json) is machine-owned. It records
  identities extracted from the pinned upstream source.
- [`capabilities.json`](capabilities.json) is human-owned. It assigns exactly
  one disposition to every generated identity and adds reviewed semantic
  records for cross-leaf behavior.
- [`logger-producer-manifest.json`](logger-producer-manifest.json) is
  machine-owned. It records every pinned public logger invocation identity,
  syntax/source context, input Git blob, and value-redacted fingerprint.
- [`logger-producer-audit.json`](logger-producer-audit.json) is human-owned. It
  maps every logger invocation explicitly to one closed semantic class and its
  reviewed delivery, limiter, disposition, owner, and evidence policy.
- [`metrics-schema.json`](metrics-schema.json) is one strict authority envelope.
  Its `source` projection is machine-derived from the pinned Rust serializers
  and Python fixture; its policy profiles and exact field mappings are
  human-owned and never regenerated.
- [`metrics-process-producer-audit.json`](metrics-process-producer-audit.json)
  is the human-owned terminal audit for the exact 69 fields assigned to the
  process producer profile. It records one closed producer boundary, delivery
  child, disposition, rationale, and evidence set per field without duplicating
  the schema or shared field policy.
- [`metrics-device-producer-audit.json`](metrics-device-producer-audit.json) is
  the human-owned terminal audit for the exact 231 fields assigned to device
  producer profiles. It records a closed operation boundary and one of the nine
  #1789 delivery children per field. After #1846 it contains 212 implemented,
  two source-neutral, 17 terminal platform-zero, and no planned or provisional
  records. #1847 composes those facts into ten implemented shared profiles and
  the dedicated device-final gate.
- [`metrics-lifecycle-audit.json`](metrics-lifecycle-audit.json) is the
  human-owned terminal #1790 matrix for ten aggregate publication, lifecycle,
  cardinality, snapshot-destination, hotplug, and isolation scenarios. Its
  distinguished transaction record closes complete-line commit atomicity,
  previous-success retry, concurrent-cut ownership, lost-output accounting,
  and one-shot final behavior without claiming all-or-none sink visibility.
- [`tracing-audit.json`](tracing-audit.json) is the human-owned terminal #1791
  authority for the opt-in feature, fixed limits and fields, exact eight
  literal production scopes, delivery classes, privacy rules, evidence, and
  nonclaims. Its validator scans production Rust syntax instead of trusting
  the declared call-site list.
- [`cpu-template-helper-audit.json`](cpu-template-helper-audit.json) is the
  human-owned terminal #1795 producer ledger. It owns the exact five operations
  and thirteen arguments once, the implemented and platform-impossible CPU
  foundations, fourteen closed composition/runtime/snapshot/fleet scenarios,
  seven explicit nonclaims, and aggregate evidence.
- [`guest-workflow-audit.json`](guest-workflow-audit.json) is the human-owned
  terminal #1796 guest workflow authority. It owns exact download pins,
  generated and recipe cache policy, output ownership classes, the implemented
  API/no-API profiles, guest identity and timeouts, evidence, and nonclaims for
  the exact two promoted corpus rows.
- [`specification-benchmark-audit.json`](specification-benchmark-audit.json) is
  the human-owned terminal #1798 authority. It pins the upstream specification
  and network-performance blobs, exact reference environments/statuses,
  Bangbang measurement methods/units, report and fixture policy, evidence,
  nonclaims, and the exact three promoted rows.
- [`wave7-aggregate-audit.json`](wave7-aggregate-audit.json) is the human-owned
  terminal #1799 and #1491 authority. It partitions all design semantics,
  derives all device-API cells and identities, enumerates all release entries
  and public-tool leaves, certifies the complete virtio-MMIO composition, and
  retains the exact nine audit plus three feasible handoffs.
- Contract Markdown files are human-owned evidence ledgers. They define a
  selected capability family, its exact supported or excluded boundary, and
  the implementation and validation evidence for its dispositions.

Regeneration may create a candidate source manifest or machine-owned source
projection. It must never create or rewrite capability dispositions, owners,
evidence references, delivery issues, policy mappings, or Challenge results. A
generated identity change must instead surface as missing or stale human policy
for review.

Stable source IDs use `<kind>:<upstream-key>`. Semantic IDs use
`semantic.<namespace>:<slug>`. IDs are scoped to this immutable baseline; a
later Firecracker release requires a separate directory and an explicitly
reviewed delta.

## Contract Index

| Contract | Evidence owned here |
| --- | --- |
| [Process](process-contract.md) | Startup arguments, readiness, process lifecycle, signals, file descriptors, cleanup, and composite run behavior |
| [Isolation](isolation-contract.md) | Production launcher/worker containment, resource authority, exact Linux platform exclusions, and remaining isolation aggregates |
| [Machine memory](machine-memory-contract.md) | Machine-memory configuration, backing, dirty tracking, and exact platform boundaries |
| [Machine lifecycle](machine-lifecycle-audit.md) | VM lifecycle, vCPU ownership, pause/resume, power sessions, and snapshot-ready coordination |
| [CPU templates](cpu-template-contract.md) | Reviewed arm64 profile, selection, application, snapshot boundary, and KVM/static exclusions |
| [CPU-template dump and verify helper](cpu-template-helper-contract.md) | Strict public CLI, real all-vCPU HVF capture, portable format, config projection, bounded input, redaction, and absent-only publication |
| [CPU-template strip](cpu-template-strip-contract.md) | Portable normalized common-bit transformation, path and suffix admission, canonical output, multi-path publication, rollback, and uncertainty |
| [CPU-template fingerprint dump](cpu-template-fingerprint-contract.md) | Versioned closed macOS/Linux document, reviewed public macOS facts, real signed effective-state capture, strict reparse, privacy, and absent-only publication |
| [CPU-template fingerprint compare](cpu-template-fingerprint-compare-contract.md) | Portable strict persisted inputs, platform-honest filters, deterministic selected-value diagnostic, exact guest stripping, redaction, and zero-provider execution |
| [Device hotplug](device-hotplug-contract.md) | Runtime block, pmem, and network transactions and their aggregate ownership |
| [Storage](storage-contract.md) | Block/pmem live and snapshot composition |
| [Balloon](balloon-contract.md) | Balloon API, queue/accounting behavior, and native-v2 state |
| [Memory hotplug](memory-hotplug-contract.md) | Virtio-mem lifecycle, mixed-memory ownership, and native-v2 state |
| [Entropy](entropy-contract.md) | Virtio-rng queues, limiting, ownership, and native-v2 state |
| [Serial](serial-contract.md) | Stdio policy, input/output ownership, and native-v2 state |
| [Time and identity](time-identity-contract.md) | PL031, VMGenID, VMClock, PVTime, clone, and restore semantics |
| [Remaining devices](remaining-device-contract.md) | Checked union of balloon, virtio-mem, entropy, serial, and time/identity families |
| [Network and MMDS](network-mmds-contract.md) | Interface/MMDS lifecycle, authority, native-v2 state, and explicit external/performance handoffs |
| [Vsock](vsock-contract.md) | Live Unix-socket subset, native-v2 state, clone-local authority, and exclusions |
| [Snapshot paging](snapshot-paging-contract.md) | Pinned page-fault contract, macOS feasibility, pager protocol/consumer boundary, native-v1 reader, and terminal evidence |
| [Snapshot Diff and rebase](snapshot-diff-rebase-contract.md) | Native-v2 2.13 differential create/load, public rebase commands, stronger no-clobber transaction, and terminal Wave 6 composition |
| [Snapshot editor state](snapshot-editor-contract.md) | Native-state version/vCPU/VM inspection, finite reviewed-register editing, no-clobber publication, signed Full/Diff product restore, and the exact twelve-row closure |
| [Snapshot Wave 6](snapshot-wave6-contract.md) | Exact 70-row load, artifact, version, device, tool, time/identity, portability, and downstream-owner certification |
| [Observability, tools, and specification](observability-tools-specification-contract.md) | Exact Wave 7 ownership, core API certification, x86 CPUID/MSR platform exclusions, and retained downstream handoffs |
| [Logger producers](logger-contract.md) | Certified 11-row logger aggregate with exact 468-invocation source closure, 24 implemented classes, 7 exact platform/developer exclusions, no planned producer classes, safe fields, and bounded default-stdout admission/forwarding policy |
| [Metrics schema and producer audits](metrics-contract.md) | Terminal twelve-row #1787 API/schema certification, exact 24-root/243-static-field arm64 line shape, 24/29/5 configured dynamic families, source fingerprints, closed units/reset/aggregation policy, a terminal 69-field #1788 process audit, and the device-final ten-profile/231-field #1789 certification with #1838–#1846 terminal |
| [Developer tracing](tracing-contract.md) | Terminal opt-in feature contract, exact eight-call AST closure, fixed nesting/record/tool-delivery bounds, privacy, loss/result preservation, runtime tool admission, release diagnostics, and explicit mechanism/timing nonclaims |
| [macOS guest workflow](guest-workflow-contract.md) | Terminal two-corpus mapping, exact pinned guest identity, public API/no-API lifecycle, process/cache ownership, signed execution and platform/nonclaim boundary |
| [Targeted formal verification](formal-verification-contract.md) | Terminal one-corpus mapping, exact Kani/toolchain pins, five source/compiled-bijective bounded proofs, retained tests, and explicit whole-system/mechanism nonclaims |
| [Specification benchmark](specification-benchmark-contract.md) | Terminal three-row reference interpretation, strict signed collector/report/comparison, real FIFO loss/replay evidence, optional fixture boundary, and exact performance nonclaims |
| [Wave 7 aggregate](observability-tools-specification-contract.md#wave-7-aggregate-certification) | Terminal 93-row parent distribution, exact design/device API/release/tool/MMIO ledgers, and explicit #1351/#1373/#1378/Wave 8 handoffs |
| [Wave 8 platform-feasible certification](wave8-certification-contract.md) | Final seven-domain/21-pair interaction authority, four historical platform-mechanism reviews, the exact Wave 8, uid/gid, and configurable-chroot successor transitions, and retained external outcomes |
| [Elevated macOS bootstrap evidence](elevated-bootstrap-evidence.md) | Same-host signed evidence for the chroot boundary, three-class permanent credential/session/grant continuation, real guest/HVF completion, the listener and retired-session results, and the product uid/gid and configurable-chroot platform gates |

## Dispositions

Every capability has exactly one disposition:

- `audit-required`: the exact contract still needs review. This is a delivery
  state, never a completion state.
- `missing-platform-feasible`: the capability is feasible on the supported
  platform but remains undelivered and must name a concrete delivery issue.
- `implemented-and-verified`: the exact observable contract has appropriate,
  resolvable implementation and validation evidence. Recognition or a stable
  unsupported response is not implementation.
- `proven-platform-impossible`: the exact upstream contract has authoritative
  platform evidence, rejected alternatives, stable product behavior, focused
  tests, compatibility/security documentation, and a current Challenge result.

Only [`capabilities.json`](capabilities.json) and the validator are
authoritative for current global totals. Contract-local selected-record counts
may be retained where they are part of a mechanically checked closure.
Historical delivery arithmetic belongs in Git and GitHub.

Wave 7 retains its historical handoff snapshot. The phase-aware gates accept
only the exact Wave 8 one-row successor, the later exact uid/gid-only
377/6/3/32 successor, the exact configurable-chroot-only 377/5/3/33 successor,
the aggregate-jailer 379/3/3/33 successor, the multiprocess-isolation
380/3/2/33 successor, the host-resource-authority 381/3/1/33 successor, and
the jailer/seccomp-containment 382/3/0/33 successor, and the current
production-host 383/2/0/33 successor, followed by the current exact
network/vmnet-feasibility 383/0/2/33 successor. The two #1378 outcomes remain
undelivered but are now `missing-platform-feasible` rather than unaudited. See the
[Wave 8 contract](wave8-certification-contract.md) for the checked transitions,
the [aggregate jailer contract](jailer-aggregate-contract.md) for its two-row
transition, the [multiprocess isolation contract](multiprocess-isolation-contract.md)
for its one-row transition, the
[host-resource authority contract](host-resource-authority-contract.md) for
its one-row transition, the
[jailer/seccomp containment contract](jailer-seccomp-containment-contract.md)
for its one-row transition, the
[production-host contract](production-host-contract.md) for the current
one-row transition, and
the [vmnet feasibility contract](vmnet-feasibility-contract.md) for the
two-row evidence transition, and
[`docs/testing.md`](../../../docs/testing.md#entitlement-free-vmnet-feasibility)
for the canonical commands.

## Guest workflow artifact authority

The guest-workflow audit deliberately separates the pinned Firecracker v1.16.0
source/API baseline from the v1.15 Firecracker CI artifact namespace used by
the signed arm64 tests. Downloaded kernel and squashfs bytes are never vendored
or redistributed by Bangbang. Their manifest-owned caches require exact size
and SHA-256 validation and use announced repair under a nonblocking advisory
lock.

The generated initrd has byte-identical cache semantics. Prepared ext4 images
are only recipe-deterministic: a sidecar binds each local result to the source,
requested size, variant, tracked recipe, bounded tool identities, output
digest/size, and a successful `e2fsck -fn`. The sidecar is committed last as a
validity marker; the image and sidecar are not claimed to be one crash-atomic
transaction.

The default direct recipe identity remains `rootfs-ext4-direct-boot-v110`. It
adds the exact mode-`0555` Apple-authorized production-vmnet Python oracle as a
tracked input alongside the two static Rust helpers and generated init. The
separate `rootfs-ext4-direct-boot-v111` identity replaces that oracle with the
static no-std entitlement-free guest oracle used only by #1930. Neither recipe
aliases historical sidecars; v111 is feasibility evidence and does not itself
implement either #1378 capability.

Runtime sidecars stay under the ignored cache root and never count as checked
inventory or terminal workflow evidence.

Both public workflow profiles are `implemented-and-verified`. The public
composer executes the exact API and no-API lifecycle against the same pinned
guest identity, and the signed integration selection runs both literal public
commands. The terminal contract and audit jointly close
`corpus:getting-started` and `corpus:rootfs-and-kernel`; the
[operator guide](../../../docs/macos-guest-workflow.md) owns usage and
troubleshooting.

The inventory is not evidence by itself. A terminal claim depends on the
referenced production behavior and validation, and a broad corpus reference
records audit ownership rather than proving every statement in that corpus.

## Evidence Rules

Implementation and validation references must resolve to tracked regular files
inside this repository. Ignored, untracked, symlinked, duplicate, unsorted, or
escaping paths fail validation. Evidence must prove the exact capability named:
parser coverage cannot promote runtime behavior, and an aggregate claim needs
aggregate validation.

Platform-impossible records additionally keep the upstream requirement,
authoritative host-platform evidence, considered alternatives and rejection
reasons, stable public behavior, focused tests, documentation, and the trusted
Challenge decision together.

## Aggregate CPU-Template Certification

The terminal #1795 helper/template corpus and CPU semantic use one fail-closed
gate:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --cpu-template-final
```

This command composes the four earlier scoped CPU-template gates with the
canonical producer ledger, exact implemented and platform-impossible
foundations, signed five-operation artifact composition, transactional runtime
selection, all-vCPU application/readback and boot precedence, the native-v1
no-template snapshot boundary, and the applicable fleet workflow. It promotes
only the exact three #1795 aggregate rows and preserves the ledger's explicit
security, distinct-host, x86/KVM, snapshot, migration, and publication
nonclaims.

## Scoped Logger Certification

The terminal logger aggregate has its own checked gate because unrelated
Firecracker capabilities remain under delivery:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --logger-final
```

This command validates the complete capability inventory in delivery mode,
the complete logger producer audit in final mode, and the exact eleven #1786
capability records as `implemented-and-verified`. It does not ignore a logger
class or alter another capability's disposition. Repository-global
`validate --final` remains the stronger all-capabilities completion gate and
must continue to fail while any unrelated `audit-required` or
`missing-platform-feasible` record or device producer remains nonterminal.

## Scoped Developer-Tracing Certification

The terminal tracing corpus has a separate checked gate:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --tracing-final
```

This command composes terminal logger certification with the checked tracing
authority in final mode and consumes unrelated metrics authorities in delivery
mode. It requires the exact eight literal production macro calls, feature and
release-default policy, fixed stack/record/tool-delivery limits, closed fields,
anchored evidence, and the single #1791 contract/capability row. It does not
relax repository-global `validate --final` or promote any logger or metrics row.

## Scoped Metrics Schema Certification

The terminal metrics API/schema slice has a separate checked gate:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --metrics-schema-final
```

This command validates the complete capability inventory and metrics authority
in delivery mode, requires the exact twelve #1787 capability and contract rows
as `implemented-and-verified`, pins the one implemented schema-runtime profile,
two implemented process-lifecycle profiles, and either complete pre-promotion
or terminal #1789 device partition (never a hybrid), validates the terminal
69-field process audit in delivery mode, and
requires both #1790 aggregate rows to occupy the same historical or terminal
state. Focused parser,
direct-process privacy, signed ordinary-production/App Sandbox privacy, and
real Paused/Running 60-second lifecycle evidence support the terminal claim.
It observes but does not itself certify the aggregate lifecycle matrix;
repository-global `validate --final` remains broader than this scoped gate.

## Scoped Process Metrics Certification

The completed #1788 process producer scope has its own fail-closed gate:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --metrics-process-final
```

This command retains the metrics API/schema compatibility gate and logger
delivery validation, then applies final mode to the exact 69-field process
audit. It requires both process-lifecycle profiles to be implemented, all four
delivery children to be terminal, and every exact field boundary, disposition,
and evidence reference to validate. Its closed result is 64 implemented, one
source-neutral, and four platform-zero records. The checked authority now has
ten implemented device profiles, but this process-scoped gate does not certify
their 231 field records. It accepts only the exact coherent historical or
terminal aggregate pair, never a partial #1790 promotion.

## Scoped Device Metrics Certification

The completed #1789 device producer scope has its own fail-closed gate:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --metrics-device-final
```

This command composes the schema and process certifications, requires exactly
ten implemented device profiles with identical checked local evidence, resolves
every evidence anchor, and applies final mode to the exact 231-field device
audit. Its closed census is 212 implemented, two source-neutral, 17
platform-zero, and no nonterminal records. Whole-line runtime coverage proves
one configured ordinary block, network, and vhost-user root produce exactly
231 device leaves, exactly 25 intentional active zeros, at-least-once replay
after an ambiguous accepted write, and stable idle shape. Both #1790 aggregate
rows may be terminal, but this device-scoped command does not certify their
ten-scenario lifecycle evidence.

## Aggregate Metrics Lifecycle Certification

The completed #1790 metrics corpus and cross-producer scope has one final gate:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --metrics-final
```

This command composes device-final certification with final validation of the
exact ten-record lifecycle matrix. It requires both aggregate capability rows
to be `implemented-and-verified` with exact evidence and the Wave 7 owner rows
to agree. The matrix covers initial, real 60-second, explicit, terminal,
backpressure, partial/failure/retry, configured cardinality, fresh snapshot
destinations, hotplug/reuse, and concurrent process isolation. A scripted
cross-producer transaction proves coherent process/device commit and
success-only baselines after a visible prefix; existing process tests prove
the final attempt is best effort and consumed once.

## Targeted Formal-Verification Certification

The terminal `corpus:formal-verification` row has one fail-closed gate:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --formal-verification-final
```

This gate validates the exact five-record authority, independently derives all
tracked `cfg(kani)` proof symbols, requires the single owned corpus row to be
terminal, and retains all unrelated delivery rows. The Linux-only
`python3 scripts/run-kani.py` command adds Kani's per-package compiled-list
bijection and executes every exact proof; its setup and bounded interpretation
live in the [formal-verification guide](../../../docs/formal-verification.md).

## Contributor Update Rule

Every pull request that changes a Firecracker-facing capability must update all
affected overlay records and owner documents in the same change. Keep
unreviewed behavior `audit-required`; use `missing-platform-feasible` only
with a delivery issue; and use `proven-platform-impossible` only after the
full evidence and Challenge gate.

Validate the checked inventory, compare or regenerate candidates, and run the
normal repository checks using
[Testing Guide](../../../docs/testing.md#firecracker-capability-inventory).
Review candidate identity changes before updating a machine-owned projection.
Never use regeneration to alter `capabilities.json`,
`logger-producer-audit.json`, `metrics-process-producer-audit.json`, or the
human policy projections in `metrics-schema.json`. The human-owned
`metrics-device-producer-audit.json`, `metrics-lifecycle-audit.json`,
`tracing-audit.json`, and `formal-verification-audit.json` likewise have no
regeneration command.
