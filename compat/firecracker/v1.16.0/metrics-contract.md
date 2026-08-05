# Firecracker v1.16.0 Metrics Schema Contract

This contract defines the checked public metrics-line authority used by
Bangbang for the pinned Firecracker v1.16.0 arm64 target. It separates schema
presence, shared producer policy, and incremental producer completion. The
machine-readable artifacts are [`metrics-schema.json`](metrics-schema.json)
and
[`metrics-process-producer-audit.json`](metrics-process-producer-audit.json);
this document explains how to interpret and update them.

## Authority Boundary

The checked contract has three layers:

- `metrics-schema.json.source` is machine-derived from the pinned Rust serializers and
  `tests/host_tools/fcmetrics.py`. It owns inputs and Git blobs, exact roots,
  scalar paths, JSON type, reset primitive, architecture, configured-root
  grammar, source anchors, and normalized SHA-256 fingerprints.
- `metrics-schema.json.policy_profiles` and `field_policies` are reviewed
  shared Bangbang policy. Together
  they attach one closed unit, aggregation rule, producer owner/disposition,
  delivery issue, rationale, and eventual implementation/validation evidence
  to every source field.
- `metrics-process-producer-audit.json` is the human-owned incremental audit of
  the exact fields selected by the process producer profile. It records each
  field's closed production boundary, delivery child, current disposition,
  rationale, and evidence without copying source shape, unit, reset, or
  aggregation policy.

The policy mapping is an exact bijection over the source fields. Policy cannot
create or remove schema identities, and source regeneration cannot manufacture
or overwrite policy. A field being required in every line does not mean its
producer is implemented; a required numeric neutral value remains distinct
from an evidence-backed producer.

The runtime compiles this authority into the canonical serializer and an exact
descriptor-equality test. #1823 certifies the bounded #1787 API/schema slice,
including the schema-runtime timestamp producer. That result does not promote
another producer policy: #1788 still owns API, process, logger, signal, boot,
and lifecycle producers; #1789 owns device, MMDS, vCPU, time/device,
configured-key, and retained-neutral producers. `corpus:metrics` and the
cross-producer aggregate semantic remain #1790-owned. A required zero therefore
proves wire-shape completeness only, never producer completion.

## Terminal API and Schema Certification

The terminal #1787 slice contains exactly twelve capability records:

- `api-operation:PUT /metrics`, `api-path:/metrics`,
  `api-property:FullVmConfiguration.metrics`, and
  `api-property:Metrics.metrics_path`;
- `api-property:RateLimiter.bandwidth`,
  `api-property:RateLimiter.ops`,
  `api-property:TokenBucket.one_time_burst`,
  `api-property:TokenBucket.refill_time`, and
  `api-property:TokenBucket.size`;
- `api-schema:Metrics`, `api-schema:RateLimiter`, and
  `api-schema:TokenBucket`.

The scoped gate requires those exact records and their #1787 contract rows to
be `implemented-and-verified`, while both #1790 aggregate records remain
`audit-required`. It also pins the policy partition to one implemented
schema-runtime timestamp profile, two planned #1788 process-lifecycle
profiles, ten planned #1789 device profiles, and four #1789 platform-zero
device profiles. Missing, extra, promoted, stale, or differently handed-off
members fail closed.

Focused API evidence rejects duplicate `metrics_path`, rate-limiter, and token
bucket fields. Every token-bucket number accepts the complete JSON `u64`
domain from zero through `18446744073709551615` and rejects negative,
fractional, and overflowing values. Direct and contained tests prove metrics
configuration leaves `GET /vm/config` byte-for-byte unchanged and cannot
expose paths, grant identifiers/references, descriptors, or private errors.
One signed ordinary-production/App Sandbox HVF guest supplies product evidence:
the initial line is followed by real 60-second lines while Paused and Running,
then one explicit line and one normal-terminal line. Each observed record is a
newline-terminated numeric JSON tree with the canonical timestamp and without
Bangbang-only fields.

This certification is deliberately narrower than producer or corpus closure.
It does not complete #1788, #1789, either #1790 aggregate, tracing, durable
delivery, remote telemetry, or a versioned extension format.

## Incremental Process Producer Audit

The process audit is an exact, sorted bijection over the 69 static fields whose
schema policy owner is `process`. Missing, duplicate, extra, stale, reordered,
or differently owned records fail closed. Its delivery partition is also
fixed: #1827 owns 44 API request and deprecation fields, #1828 owns 12 startup,
logger, panic, signal, and seccomp fields, #1829 owns five successful inner-VMM
operation fields, and #1830 owns eight successful outer API-operation fields.
Only a completed child may use the `implemented` disposition and carry
resolvable production and validation evidence. #1827 is the sole completed
child in this slice; the remaining 25 records stay `planned` until their owning
pull requests land. `source-neutral` and `platform-zero` are terminal policy
choices, not aliases for an undelivered feasible producer.

For #1827, request metrics are created as a typed, value-free effect only after
the HTTP request passes admission. The effect is applied exactly once before
dispatch, so parser results—not later lifecycle, configuration, or backend
action results—control request failure counters. Exact GET routes increment
their count only. Exact PUT and PATCH routes increment once and add a failure
only when their endpoint parser rejects the request, with these pinned
exceptions:

- arm64 `SendCtrlAltDel` increments `actions_count` without incrementing
  `actions_fails`;
- an empty machine-config PATCH object increments `machine_cfg_count` without
  incrementing `machine_cfg_fails`;
- a missing or mismatched network PATCH identifier contributes the pinned
  count delta of two and no failure, while malformed JSON contributes one count
  and one failure;
- an invalid nonempty resource identifier contributes one count and no
  failure, while a missing identifier contributes one count and one failure
  except for the network PATCH rule above;
- one unrecognized nonempty `/mmds/<token>` route contributes one MMDS count
  and one failure; strict extra segments and unrelated routes have no effect.

Bodies rejected during HTTP admission are never parsed for metrics. Deprecated
API accounting is emitted only for an accepted typed deprecated value and
contains no request value. Saturating counters, canonical JSON shape, omission
of newer balloon request fields, redaction, and the initial successful
`PUT /metrics` self-count remain unchanged.

## Exact Arm64 Shape

Every scalar is JSON `number`. Every static property is required and every
object rejects additional properties. The 24 static roots occur in pinned Rust
serialization order:

1. `utc_timestamp_ms`
2. `api_server`
3. `balloon`
4. `block`
5. `deprecated_api`
6. `get_api_requests`
7. `i8042`
8. `rtc`
9. `uart`
10. `latencies_us`
11. `logger`
12. `mmds`
13. `net`
14. `patch_api_requests`
15. `put_api_requests`
16. `seccomp`
17. `vcpu`
18. `vmm`
19. `signals`
20. `vsock`
21. `entropy`
22. `pmem`
23. `interrupts`
24. `memory_hotplug`

Those roots contain exactly 243 scalar paths. `i8042` is retained by the common
legacy serializer on arm64 and must not be removed merely because its useful
PIO device mechanism is x86-oriented. Arm64 additionally serializes `rtc`;
this is an addition, not an `i8042` replacement.

The upstream fixture's dictionary insertion order is not the wire order: it
adds `rtc` after constructing the base dictionary. The authority therefore
uses the parsed Rust root/flatten order while requiring exact set equality with
the strict fixture.

## Configured Dynamic Roots

The canonical schema has exactly three configured dynamic families:

| Fixture rule | Concrete pinned producer template | Scalar fields | Cardinality |
| --- | --- | ---: | --- |
| `block_*` | `block_{drive_id}` | 24 | one object per configured ordinary block device |
| `net_*` | `net_{iface_id}` | 29 | one object per configured network interface |
| `vhost_user_*` | `vhost_user_block_{drive_id}` | 5 | one object per configured vhost-user block device |

Block and network also retain their required aggregate `block` and `net` roots
when no device is configured. Their configured entries precede the aggregate
in the pinned serializer and use ordered maps. Vhost-user has no aggregate
root. The generic serializer prepends `vhost_user_` to the module name; the
pinned block constructor supplies `block_{drive_id}`. A vhost-user block is not
also entered into the ordinary block metrics map and must not produce a second
`block_{drive_id}` object.

Dynamic suffixes come only from validated configuration identifiers. Runtime
serialization may iterate only the bounded device registries owned by the
configuration model; it must not discover untrusted host paths, guest data, or
unbounded keys while writing a line.

The arm64 transport envelope admits at most 985 dynamic identities, including
at most 16 networks. The 985 bound comes from 987 SPI INTIDs after reserving the
mandatory VMGenID and VMClock identities. Total copied suffix bytes are limited
to 51,429,376 bytes: one 1 MiB initial configuration plus 984 independently
bounded 51,200-byte API payloads. Identifiers keep their validated UTF-8 bytes;
JSON serialization does not use host paths or diagnostic registry keys as
suffix sources.

Pinned Rust also formats `pmem_{pmem_id}`, while the strict v1.16.0 fixture has
no `pmem_` prefix branch and would reject such an extra root. The source
authority retains this as the explicit
`producer-only:pmem_{pmem_id}` reconciliation. It is not a fourth canonical
dynamic family. A later compatibility-policy change must update the strict
extension boundary deliberately rather than silently admitting the producer
extra.

## Value, Unit, and Aggregation Semantics

`SharedIncMetric` fields are interval values. Pinned Firecracker computes the
delta from its previous atomic and advances that previous value when field
serialization succeeds. `SharedStoreMetric` fields are current persistent
values and do not reset per line. `utc_timestamp_ms` is a fresh publication
attempt timestamp in milliseconds since the Unix epoch; the upstream fixture
requires it to be plausible within one second.

The closed units are counts, bytes, microseconds, and Unix-epoch milliseconds.
Byte fields include names containing `bytes` and balloon's
`free_page_report_freed` and `free_page_hint_freed`, which add descriptor byte
lengths. Latency roots, `_us` fields, and `min_us`/`max_us`/`sum_us` leaves use
microseconds. All remaining fields use counts.

Nested latency aggregates identify minimum, maximum, and sum semantics. Static
block, network, and pmem roots are sums across configured devices for their
ordinary fields. Pinned block/network aggregate construction adds latency
`sum_us` but leaves aggregate `min_us` and `max_us` at zero; the authority
records that behavior explicitly instead of describing those zeros as valid
cross-device extrema. Dynamic per-device latency objects retain their own
minimum/maximum/sum meaning.

The arm64 schema keeps the i8042 fields and the x86-only vCPU PIO and KVM-clock
fields as required numeric neutral values. Their `platform-zero` producer
disposition is not an implementation claim; #1789 retains the terminal evidence
responsibility. Every other required field without an exact current producer is
also emitted as numeric zero while its policy remains nonterminal.

## Bangbang Publication Rules

The checked shape is strict. Canonical Firecracker lines do not retain public
Bangbang-only fields such as `vmm.metrics_flush_count`, string-valued boot
status, dynamic `pmem_*` roots, vmnet fields, newer balloon API counters, newer
balloon/UART fields, or ordinary-block configuration-change fields. Their
underlying internal state may remain for runtime decisions and focused tests.
Any future extension needs a separately versioned and tested boundary;
silently appending it to this pinned line is incompatible with
`additionalProperties: false`.

The migration decision census for every former Bangbang-only public group is:

| Former public key or group | Strict v1.16.0 decision |
| --- | --- |
| `vmm.metrics_flush_count` | Removed; it was a synthetic successful-line marker, not a pinned metric. |
| `vmm.boot_run_loop_status` | Retained only in internal lifecycle diagnostics; no string value enters the numeric line. |
| GET/PUT/PATCH `balloon_count` and PUT/PATCH `balloon_fails` | Retained as internal API accounting; v1.16.0 has no matching API-request fields. |
| `pmem_{device_id}` roots | Removed; per-device accounting remains internal and exact values continue to feed only the pinned aggregate `pmem` root. |
| ordinary `block_{drive_id}.config_change_time_us` | Retained internally; ordinary pinned block metrics have no matching field. |
| vhost-user values formerly exposed under `block_{drive_id}` | Reclassified exclusively to `vhost_user_block_{drive_id}`. Only the store-compatible `config_change_time_us` maps today; other vhost notification state remains internal or logger evidence. |
| `net{,_<id>}.vmnet_{read,write}_{count,fails,packets_count,partial_batches}` and `vmnet_{read,write}_latency_us` | Retained only in vmnet backend diagnostics; tap metrics are not a name-only semantic substitute. |
| `uart.input_count`, `uart.interrupt_count`, and `uart.overrun_count` | Retained only in serial internals; the remaining UART fields map exactly. |
| `balloon.inflate_discard_{attempts,advised_bytes,skipped_bytes,fails}` | Retained internally; there is no matching pinned inflate-discard family. |
| `balloon.hinting_discard_{attempts,advised_bytes,fails}` | Exact-mapped respectively to `free_page_hint_{count,freed,fails}`; `hinting_discard_skipped_bytes` remains internal. |
| `balloon.free_page_report_{count,advised_bytes,fails}` | Exact-mapped respectively to `free_page_report_{count,freed,fails}`; `requested_bytes` and `skipped_bytes` remain internal. |
| `memory_hotplug.interrupt_fails`, `rollback_{count,fails}`, `owner_cleanup_{count,fails}`, and `teardown_{count,fails}` | Retained only in device/owner diagnostics; all pinned memory-hotplug fields map independently. |

`GET /vm/config` continues to omit active metrics output configuration, as does
the pinned Firecracker full-configuration projection. Output sink ownership and
path redaction remain governed by the existing direct/contained process
contract.

Bangbang uses a stronger publication transaction than pinned Firecracker. One
flush attempt must snapshot all producers before serialization. Interval values
are computed from the last completely successful publication, and that
baseline advances only after the entire newline-terminated line is accepted.
A failed write retains the baseline and replays the interval. If a writer
exposed a prefix before returning failure, observers may see an ambiguous
partial attempt followed by an at-least-once replay; the contract does not
claim durable or exactly-once output.

Initial, periodic, explicit, and terminal flush paths must share that immutable
attempt transaction. Producer events arriving after the snapshot belong to a
later attempt. No individual producer may reset independently during
collection.

One attempt captures the clock once, constructs one immutable typed value, and
serializes it twice: first into a no-allocation counting writer, then into an
exactly reserved fixed-capacity buffer. The complete JSON-plus-newline record
is limited to 64 MiB. Count overflow, allocation failure, serialization drift,
or a larger record fails before sink access. Publication writes all JSON bytes,
then exactly one newline, then flushes. Short and interrupted writes are
retried; zero progress, would-block, other write failures, newline failures,
and flush failures are closed redacted stages. Only success of all three output
stages advances the baseline.

Exact compact fixtures live at
`crates/runtime/src/metrics/fixtures/minimal.jsonl` and
`crates/runtime/src/metrics/fixtures/all-static-nonzero.jsonl`. A generated
maximum recipe exercises all 985 configured roots without checking a
tens-of-megabytes fixture into the repository. The compiled upper-bound proof
uses the identity budget, dynamic prefix/punctuation overhead, every family
shape, and 20-digit `u64::MAX` values and remains below the 64 MiB record cap.

## Validation and Regeneration

Ordinary delivery validation is self-contained:

```sh
cargo test -p bangbang-runtime metrics::firecracker::tests --all-features --locked
cargo test -p bangbang-runtime metrics::tests --all-features --locked
cargo test -p bangbang-firecracker-capability-audit --test metrics_schema --locked
cargo run -p bangbang-firecracker-capability-audit --locked -- validate
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --metrics-schema-final
```

`validate --metrics-schema-final` runs repository delivery validation, the
metrics authority delivery gate, the exact twelve-row #1787 certification, and
the logger producer delivery gate needed by metrics collection. It validates
the exact 69-field process audit and accepts only its recorded completed-child
set. It permits only the explicit #1788/#1789 producer handoffs and retained
#1790 aggregate rows; repository-global `validate --final` remains the stronger
all-capability gate.

With an explicit clean sibling at the pinned commit, compare every source
identity, Git blob, root/path/template, type/reset fact, architecture rule, and
fingerprint:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- compare \
  --firecracker /path/to/firecracker
```

Regeneration can emit only a source candidate at a new explicit path:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- \
  regenerate-metrics-schema-source \
  --firecracker /path/to/firecracker \
  --output codex-work/tmp/metrics-schema-source.candidate.json
```

The destination must not exist and cannot directly, lexically, or through a
symlink alias any checked inventory file. Review exact source changes, then
manually reconcile them with human policy and, when process-profile membership
changes, the process audit. Regeneration never writes either human-owned layer.
Never copy old policy onto a changed identity without reviewing its unit,
reset, aggregation, architecture, cardinality, producer owner, disposition,
rationale, boundary, delivery child, and evidence.
