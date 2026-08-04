# Firecracker v1.16.0 Metrics Schema Contract

This contract defines the checked public metrics-line authority used by
Bangbang for the pinned Firecracker v1.16.0 arm64 target. It separates schema
presence from producer completion. The machine-readable authority is
[`metrics-schema.json`](metrics-schema.json); this document explains how to
interpret and update it.

## Authority Boundary

`metrics-schema.json` is one strict envelope with two ownership domains:

- `source` is machine-derived from the pinned Rust serializers and
  `tests/host_tools/fcmetrics.py`. It owns inputs and Git blobs, exact roots,
  scalar paths, JSON type, reset primitive, architecture, configured-root
  grammar, source anchors, and normalized SHA-256 fingerprints.
- `policy_profiles` and `field_policies` are reviewed Bangbang policy. Together
  they attach one closed unit, aggregation rule, producer owner/disposition,
  delivery issue, rationale, and eventual implementation/validation evidence
  to every source field.

The policy mapping is an exact bijection over the source fields. Policy cannot
create or remove schema identities, and source regeneration cannot manufacture
or overwrite policy. A field being required in every line does not mean its
producer is implemented; a required numeric neutral value remains distinct
from an evidence-backed producer.

This authority-only delivery leaves all producer profiles nonterminal. #1822
owns canonical line construction and timestamp publication, #1788 owns API,
process, logger, signal, boot, and lifecycle producers, and #1789 owns device,
MMDS, vCPU, time/device, configured-key, and retained-neutral producers. #1823
owns the final #1787 API/schema certification. `corpus:metrics` and the
cross-producer aggregate semantic remain #1790-owned.

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
responsibility. Other process and device fields remain `planned` until their
owner slices provide exact production and validation evidence.

## Bangbang Publication Rules for the Next Slice

The checked shape is strict. Canonical Firecracker lines cannot retain public
Bangbang-only fields such as `vmm.metrics_flush_count` or string-valued status
extensions. Any extension needs a separately versioned and tested boundary;
silently appending it to the pinned line is incompatible with
`additionalProperties: false`.

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

## Validation and Regeneration

Ordinary delivery validation is self-contained:

```sh
cargo test -p bangbang-firecracker-capability-audit --test metrics_schema --locked
cargo run -p bangbang-firecracker-capability-audit --locked -- validate
```

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
manually reconcile them with human policy. Never copy old policy onto a changed
identity without reviewing its unit, reset, aggregation, architecture,
cardinality, producer owner, disposition, rationale, and evidence.
