# Host-Resource Authority Contract

This contract certifies the exact Firecracker v1.16.0 fixed host-resource
outcome owned by #1916. It closes only
`semantic.isolation:host-resource-authority-and-brokerage`. It does not convert
operator host administration, Linux mechanisms, positive vmnet connectivity,
or a broader product design into bangbang claims.

## Pinned source identity

| Source | Manifest identity | Pinned path | Git blob | Reviewed scope |
| --- | --- | --- | --- | --- |
| Firecracker design | `corpus:design` | `docs/design.md` | `143fef76410e4f7e45b32d3986e0d78eedf5175a` | TAP and block backing, threat barriers, device rate limiting, privileged third-party grants, and host fairness |
| Jailer | `corpus:jailer` | `docs/jailer.md` | `fa5e8b4ee769f64ee83a317dce5902ffd0029a1b` | Validation, limits, trusted inputs, resource placement and permissions, partitioning, and cleanup |
| Network setup | `corpus:network-setup` | `docs/network-setup.md` | `c161b6661d4362a49d1978e0cafc5e7a6e5cebf6` | TAP, routing, host-device selection, multi-guest allocation, bridge setup, and cleanup |
| Production host setup | `corpus:production-host` | `docs/prod-host-setup.md` | `8939b56a965963d8df1c44c583dcd38361197347` | Output bounds, path/identity policy, workload controls, network filtering, storage contention, and overwatching |

The machine authority records 30 ordered source clauses. Missing, duplicated,
reordered, or invented clauses fail validation. The checked source manifest
pins every blob, so the result cannot float to another upstream revision.

## Ordered obligation mapping

| Order | Upstream obligation | Checked outcome |
| --- | --- | --- |
| 1 | Network devices use a host TAP | Canonical vmnet policy and admission are implemented; actual service execution stays under #1378 |
| 2 | Block devices use host files | Exact pre-opened regular-file or block-device grants back startup and runtime storage |
| 3 | The network barrier applies I/O rate limiting | Independent RX/TX device limiters execute before host packet delivery |
| 4 | Guest egress must be filtered on the host | Firewall policy remains operator-owned |
| 5 | Per-volume and per-interface limiters provide fair sharing | Block and network token buckets, updates, retry, persistence, and metrics are implemented |
| 6 | A supplied vhost-user backend owns its limiter | The fixed connect-only facet is implemented; backend limiting stays backend-owned |
| 7 | After jail setup, only privileged third-party grants are accessible | Strict launcher preflight and bounded typed transfer provide the macOS outcome |
| 8 | Cgroup affinity and CPU quotas can provide host fairness | Device/process bounds compose with the terminal cgroup platform result and operator policy |
| 9 | Jailer validates all paths and VM identity before mutation | Launch, complete batch, and contained startup preflight precede spawn/device mutation |
| 10 | Jailer supports `fsize` and `no-file` | Exact authenticated worker-local limits are implemented and kernel-tested |
| 11 | Jailer inputs and their parents are trusted | No-follow identity/type/access checks protect accepted objects; caller parent permissions remain operator-owned |
| 12 | Resources are copied/linked into the jail with exact permissions | Descriptor and anchored-directory authority supplies the equivalent fixed result |
| 13 | Users tune resource partitioning through cgroups | Linux cgroup tuning remains a terminal platform/operator boundary |
| 14 | Users clean up and account for crash-before-subscription races | Owned publication, both death orders, bounded recovery, and replacement safety are implemented; host state stays operator-owned |
| 15 | Serial output must use bounded storage or rate limiting | Serial token buckets, exact sinks, and file-size limits are implemented; storage selection stays operator-owned |
| 16 | Guest-influenced logs require bounded storage | Exact logger sinks and process bounds are implemented; collection capacity stays operator-owned |
| 17 | An external overwatcher may SIGKILL an unresponsive VMM | Detection/restart stays operator-owned; cancellation, reap, cleanup, and next-launch recovery are implemented |
| 18 | Production paths must not be writable by unprivileged users | Fixed code and strict pre-open identity validation are implemented; caller path ownership stays operator-owned |
| 19 | Resource files use least-privilege ownership | Exact access and peer identity checks compose with the terminal fixed-topology uid/gid result |
| 20 | Resource policy is workload-specific and has no aggressive universal default | Operator admission and capacity policy are not product defaults |
| 21 | Disk consumption uses blkio controls and `fsize`/`no-file` | Storage token buckets and exact rlimits are positive; Linux blkio is terminal/operator-owned |
| 22 | Memory and CPU use workload cgroup controls | These remain terminal Linux mechanisms and host policy |
| 23 | Network flooding can use device limiters or host tools | Device limiters are implemented; tc/netns/firewall remain host policy |
| 24 | Production guest egress requires a host firewall | Operator-owned, with no VMM filtering claim |
| 25 | Storage noisy-neighbour contention uses jailer and rate limits | Typed storage authority and token buckets are implemented; host page-cache policy is external |
| 26 | The network guide is a quick start to adapt for production | Host topology remains an operator choice |
| 27 | Each VM receives a host interface and TAP | Authority policy is implemented; positive native execution remains #1378 |
| 28 | Firecracker consumes `host_dev_name` | Bangbang consumes a bounded immutable vmnet mode/bridge policy, not a raw ambient host path |
| 29 | Multiple guests require distinct subnets, endpoints, and rules | Global allocation and shared-table lifetime remain operator-owned |
| 30 | Bridge setup and deletion are host operations | Operator-owned and outside the fixed VMM authority claim |

## Closed resource surface

The authority pins exactly 17 resource roles and five access modes:
read-only, write-only, read-write, create-children, and connect-children.

| Role | Object/access | Lifetime and consumer |
| --- | --- | --- |
| `startup-config` | regular file, read-only | One-time startup parser claim |
| `startup-metadata` | regular file, read-only | One-time metadata parser claim |
| `kernel-image` | regular file, read-only | One-time guest kernel load |
| `initrd-image` | regular file, read-only | One-time guest initrd load |
| `drive-backing` | regular file or block device, read-only/read-write | Transactional startup, hotplug, replacement, and restore |
| `pmem-backing` | regular file, read-only/read-write | Transactional startup, hotplug, replacement, and restore |
| `api-socket-directory` | directory, create-children | Worker claim, launcher staged bind/publication, record-backed cleanup |
| `vsock-socket-directory` | directory, create-children | Session-retained listener/publication/connect |
| `logger-sink` | regular file, write-only | One-time output adoption |
| `metrics-sink` | regular file, write-only | One-time output adoption |
| `serial-sink` | regular file, write-only | Transactional output and restore |
| `snapshot-describe-input` | regular file, read-only | One-time describe claim |
| `snapshot-state-input` | regular file, read-only | One-time restore claim |
| `snapshot-memory-input` | regular file, read-only | One-time restore claim |
| `snapshot-output-directory` | directory, create-children | Transactional state/memory/staging publication |
| `vhost-user-socket-directory` | directory, connect-children | Session-retained exact-child connections |
| `snapshot-pager-stream` | connected Unix stream, read-write | One-time pager claim |

The strict v1 manifest is bounded to 256 KiB and 64 grants. IDs and singleton
roles are unique; paths are absolute and bounded; opens are no-follow; type,
status, identity, access, and aliases are checked before spawn. A complete
prepared batch owns rollback. The receiver stages the complete authenticated
file and directory population before one commit exposes authority.

## Fixed authority behavior

Contained consumers use opened identities or retained anchored scopes; no
tagged path string becomes ambient authority. Startup and restore preflight
complete before mutation. Storage and snapshot transactions return unused
authority on abort and consume it exactly on commit. Ordinary API publication
uses one authenticated worker request, launcher-owned private staging, a
durable record before exclusive rename, and one exact listener reply; later
vsock activation carries the same broker sequence. API/vsock publication,
vhost-user exact-child connects, retained block control, and pager streams each
use separate bounded session/sequence protocols rather than a general dynamic
resource broker.

Output grants, `RLIMIT_FSIZE`, `RLIMIT_NOFILE`, and device token buckets bound
the product-owned surfaces. Replacement-safe cleanup removes only recorded
objects. Cancellation, absolute deadlines, both process death orders, and
same-ID concurrent noninterchangeability are signed production outcomes.

## Network and operator boundary

`VmnetAuthority` is canonical, immutable, redacted, profile-bound, and paired
with an active-interface maximum. Networkless production rejects every positive
mode before session creation. That is the complete authority result here.

Positive vmnet connectivity, Apple-approved credentials, real packet movement,
service failure taxonomy, teardown, cancellation, SIGKILL reclamation, repeat,
and concurrency remain exclusively owned by #1378. `corpus:network-setup` and
`semantic.network:virtio-net-vmnet-policy-and-connectivity` remain
audit-required. The contract does not borrow their unexecuted evidence.

Host TAP/bridge/address/routing/NAT/firewall construction and shared-state
cleanup remain operator responsibilities. CPU, memory, and blkio policy,
workload admission, capacity, monitoring, output storage, and deployment are
also operator or independently owned outcomes. Literal cgroup, namespace,
configurable-chroot, and arbitrary uid/gid mechanisms retain their existing
terminal platform conclusions.

## Residual classification and nonclaims

| Residual phrase | Checked classification |
| --- | --- |
| General dynamic brokerage | Generic nonrequirement; every accepted fixed resource has an exact facet |
| Hard revocation | Generic nonrequirement and explicit nonclaim |
| Cross-filesystem socket publication | Implementation-specific nonclaim; same-filesystem owned publication is asserted |
| Global cross-launcher allocation | Operator-owned nonrequirement |
| Real vmnet and repository-owned credentials | Independent #1378 external dependency |
| Host networking administration | Operator-owned outcome |
| CPU/memory/blkio cgroups and arbitrary uid/gid | Existing terminal platform limits |
| Automatic restart/reconnect | Operator-owned nonrequirement |
| Vhost-user backend rate limiting | Explicit upstream backend responsibility |
| Aggressive universal quotas | Generic nonrequirement; upstream makes them workload-specific |
| Developer ID, notarization, and deployment | Independently owned outcome |
| Remaining resource mutation/cleanup | Already implemented across fixed storage, snapshot, output, socket, and failure profiles |

No general dynamic resource broker, hard revocation, cross-filesystem atomic
socket publication, global allocator, positive production vmnet, host firewall,
Linux mechanism parity, positive arbitrary per-instance uid/gid, restart daemon,
vhost-user backend limiter, aggressive universal quota, or distribution claim
is made.

## Evidence profiles

Eleven closed profiles bind the source and resource records to tracked anchors:
manifest preflight; atomic grant transport; boot/input authority; storage
runtime authority; output/rate bounds; socket/vsock/vhost authority;
snapshot/pager authority; network policy; limits/fairness; failure/cleanup/
concurrency; and terminal/operator boundaries. Every reference is local,
anchored, unique, sorted, and checked for presence.

## Inventory transition

| Capability | Delivery | Disposition |
| --- | --- | --- |
| `semantic.isolation:host-resource-authority-and-brokerage` | #1916 | `implemented-and-verified` |

The predecessor is exactly `380/3/2/33`; the successor is exactly
`381/3/1/33`. A digest pins every unrelated inventory record. The owned row
also repairs its source set by adding `corpus:design`, whose checked Wave 7
threat-containment mapping points to this composite.

At the original #1916 checkpoint, `corpus:network-setup`,
`corpus:production-host`, and
`semantic.network:virtio-net-vmnet-policy-and-connectivity` remained
audit-required. The sole remaining #1351 feasible row was
`semantic.isolation:jailer-seccomp-and-macos-containment-outcomes`.

At the later exact `382/3/0/33` successor, #1918 certifies that containment row
without changing this #1916 transition or its terminal host-resource claim.
At `383/2/0/33`, #1920 separately certifies the production-host corpus; only
the two #1378 records remain audit-required.

## Terminal host-resource authority outcome

The complete applicable result is a validation-before-mutation, bounded,
typed, identity-checked fixed resource surface; atomic adoption and rollback;
no ambient reopen or fallback; exact runtime transactions; bounded outputs and
device fairness; fixed session brokers; replacement-safe cleanup; bounded
crash behavior; and concurrent noninterchangeability. Terminal platform limits,
external vmnet evidence, and operator policy remain at their exact boundaries.

No new entitlement, root helper, service, runtime protocol, sudo-dependent
product path, or host mutation is introduced by this certification.

## Certification

The scoped terminal gate is:

```console
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --host-resource-authority-final
```

It composes delivery validation, canonical authority bytes, exact source blobs
and clauses, the resource surface, dependencies, current-tree evidence,
residuals/nonclaims, unrelated-row digest, contract, and single inventory row.

The global `--final` gate remains stronger and intentionally fails while the
independent audit and feasible outcomes remain nonterminal.
