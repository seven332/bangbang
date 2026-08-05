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
  is the human-owned incremental audit for the exact 69 fields assigned to the
  process producer profile. It records one closed producer boundary, delivery
  child, disposition, rationale, and evidence set per field without duplicating
  the schema or shared field policy.
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
| [Metrics schema and process producers](metrics-contract.md) | Terminal twelve-row #1787 API/schema certification, exact 24-root/243-static-field arm64 line shape, 24/29/5 configured dynamic families, source fingerprints, closed units/reset/aggregation policy, and a terminal 69-field #1788 process audit with 64 implemented, one source-neutral, and four platform-zero records |

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
`missing-platform-feasible` record remains.

## Scoped Metrics Schema Certification

The terminal metrics API/schema slice has a separate checked gate:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --metrics-schema-final
```

This command validates the complete capability inventory and metrics authority
in delivery mode, requires the exact twelve #1787 capability and contract rows
as `implemented-and-verified`, pins the one implemented schema-runtime profile,
two implemented process-lifecycle profiles, and exact downstream #1789 device
partition, validates the terminal 69-field process audit in delivery mode, and
requires both #1790 aggregate rows to remain `audit-required`. Focused parser,
direct-process privacy, signed ordinary-production/App Sandbox privacy, and
real Paused/Running 60-second lifecycle evidence support the terminal claim.
It does not promote later device, corpus, or cross-producer work;
repository-global `validate --final` continues to fail until those unrelated
records are terminal.

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
source-neutral, and four platform-zero records. Ten planned and four
platform-zero device profiles remain #1789-owned, while both aggregate rows
remain `audit-required` for #1790; neither downstream scope is promoted by this
gate.

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
human policy projections in `metrics-schema.json`.
