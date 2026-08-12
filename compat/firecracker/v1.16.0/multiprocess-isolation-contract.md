# Multiprocess Isolation Contract

This contract certifies the exact Firecracker v1.16.0 multiprocess isolation
outcome owned by #1914. It closes only
`semantic.isolation:multiprocess-concurrency-redaction-and-failure-atomicity`.
It does not turn broad production-host guidance, Linux mechanisms, or
operator-owned deployment policy into bangbang product claims.

## Pinned source identity

| Source | Manifest identity | Pinned path | Git blob | Reviewed scope |
| --- | --- | --- | --- | --- |
| Firecracker design | `corpus:design` | `docs/design.md` | `143fef76410e4f7e45b32d3986e0d78eedf5175a` | Process-per-VM topology, malicious-vCPU containment, process constraints, third-party grants, and per-VM fairness |
| Production host setup | `corpus:production-host` | `docs/prod-host-setup.md` | `8939b56a965963d8df1c44c583dcd38361197347` | Overwatcher, jailer-equivalent constraints, trusted inputs, dedicated identities, resource controls, and the single-tenant process boundary |

The machine authority records 13 ordered source clauses. Missing, duplicated,
reordered, or invented clauses fail validation. The two source blobs are
validated through the checked source manifest, so this result cannot silently
float to a newer upstream document.

## Ordered obligation mapping

| Order | Upstream obligation | Checked macOS outcome |
| --- | --- | --- |
| 1 | Different-customer workloads on one machine | One signed launcher owns one signed worker and one VM namespace; concurrent authorities are noninterchangeable. The unique-credential extra layer remains the terminal limit below. |
| 2 | Simultaneous microVMs are bounded by available host resources | Independent invocations run concurrently; total host capacity and admission remain operator-owned. |
| 3 | Each Firecracker process encapsulates one and only one microVM | `launch_prepared` creates exactly one worker for one launch, and the worker owns one locked namespace and VM lifecycle. |
| 4 | Malicious vCPU threads remain inside nested trust zones | HVF, App Sandbox, the signed process boundary, authenticated lifecycle, and typed authority provide the applicable macOS barriers; Linux and unique-identity limits remain explicit. |
| 5 | Defense in depth constrains the VMM at process level | Fixed code, App Sandbox, closed descriptors/environment, private state, exact limits, and supervision are composed with the already-terminal jailer results. |
| 6 | After jail setup, resources arrive only from a privileged third-party | The launcher validates and transfers typed descriptors; staged batches commit atomically, reject aliases, and do not reopen tagged path strings. |
| 7 | Each microVM can receive fair resource controls | Exact file-size/file-descriptor limits and per-device authority are implemented; host scheduling and workload policy remain operator choices. |
| 8 | An external overwatcher detects an unresponsive process and sends SIGKILL | The operator owns liveness detection. Bangbang's surviving endpoint performs bounded cancellation, reap, and identity-safe recovery; automatic restart is not implied. |
| 9 | Production uses the jailer or equal/more restrictive constraints | The terminal aggregate jailer authority supplies the applicable macOS result while preserving every challenged Linux or fixed-topology limit. |
| 10 | Jailer inputs and parent paths are trusted | Fixed bundle code is authenticated, caller paths are validated before typed claims, publication is no-clobber, and cleanup preserves replacements. Cross-launcher path allocation remains operator-owned. |
| 11 | Dedicated nonprivileged identity, with unique `uid` and `gid` per instance as an extra layer | Current non-root identity, App Sandbox, and per-session authority are implemented. Positive arbitrary/unique per-instance credentials compose the terminal #1905 platform limit instead of becoming a false claim. |
| 12 | Operators select workload-specific resource controls | Bangbang enforces its exact inherited limits and typed grants; general host admission, scheduling, and cgroup-equivalent policy remain independently owned. |
| 13 | One process corresponds to one tenant workload | The process-per-VM and concurrent noninterchangeability evidence implements the applicable boundary, subject to the explicit same-bundle-sibling and unique-credential nonclaims. |

## Evidence profiles

Eight closed profiles connect every source clause and residual to current-tree
implementation and validation anchors:

| Profile | Result |
| --- | --- |
| Process-per-VM boundary | One launcher, one fixed worker, one namespace, and one real sandboxed HVF guest |
| Lifecycle identity and redaction | Exact session/sequence state, kernel peer checks, malformed-bootstrap rejection, and value-free diagnostics |
| Atomic resource authority | Typed one-time descriptor adoption, batch commit/rollback, and mismatch-before-mutation |
| Crash cancellation and recovery | One absolute grant deadline, signal cancellation, both single-endpoint death orders, and bounded next-launch recovery after dual death |
| Replacement-safe publication | Exact ownership records and cleanup that removes only the recorded inode |
| Concurrent noninterchangeability | Same-ID concurrent grant/restore sessions cannot exchange authority, and one crashed peer does not terminate another |
| Terminal identity limit | The challenged fixed-topology uid/gid result is composed without claiming positive arbitrary credentials |
| Operator boundary | Host capacity, overwatching, cross-launcher paths, restart, and deployment policy stay with the operator |

All evidence references must be tracked, local, anchored, unique, sorted, and
equal to the validator's closed profile sets. This prevents a broad prose claim
from replacing executable or implementation evidence.

## Residual classification

The old summary grouped several broad phrases as unfinished implementation.
The pinned upstream obligations do not require a general dynamic resource
broker, arbitrary post-Ready races, or hard revocation. Those are broader
product ideas owned by other scopes, not missing parts of this capability.

| Residual phrase | Terminal classification |
| --- | --- |
| General dynamic-resource races | Generic nonrequirement for the pinned multiprocess obligation; typed startup and runtime transactions remain atomic |
| Hard revocation | Generic nonrequirement; no hard-revocation claim is made |
| Snapshot create before its ownership record | Implementation-specific nonclaim; bounded recorded recovery and replacement safety are the asserted outcome |
| Residue after simultaneous uncatchable death of both endpoints | Implementation-specific nonclaim; bounded next-launch recovery is asserted, immediate zero residue is not |
| Malicious same-container/same-bundle sibling | Composed terminal platform limit through the accepted App Sandbox/current-identity topology and #1905 |
| Automatic restart/reconnect | Operator-owned nonrequirement; upstream recommends SIGKILL by an external overwatcher, not an in-product restart service |
| Global cross-launcher path coordination | Operator-owned nonrequirement; callers allocate and synchronize intentionally shared host paths |

## Closed nonclaims

This certification does not claim Linux jailer mechanism parity, a general
dynamic resource broker, hard revocation, an immediate-zero snapshot-create
window, immediate-zero residue after dual SIGKILL, malicious same-bundle
sibling isolation, positive unique uid/gid per instance, automatic
restart/reconnect, global cross-launcher path allocation, or production vmnet
and host deployment.

In particular, a simultaneous uncatchable loss of launcher and worker can
leave an exact empty recorded namespace until the bounded recovery owner runs.
That is compatible with the checked outcome and is not rewritten as immediate
cleanup. Likewise, the terminal uid/gid result remains a negative fixed-topology
platform result rather than positive unique-identity support.

## Inventory transition

| Capability | Delivery | Disposition |
| --- | --- | --- |
| `semantic.isolation:multiprocess-concurrency-redaction-and-failure-atomicity` | #1914 | `implemented-and-verified` |

The predecessor is exactly `379/3/3/33`; the successor is exactly
`380/3/2/33`. A checked digest pins every unrelated inventory record. The only
changed disposition is the row above.

At the exact `381/3/1/33` host-resource successor, the checked unrelated-row
digest advances to include #1916 while this #1914 row and its original
transition remain unchanged.

`corpus:production-host` remains independently audit-required, as do
`corpus:network-setup` and
`semantic.network:virtio-net-vmnet-policy-and-connectivity`. #1916 separately
certifies host-resource authority/brokerage; the sole remaining #1351 feasible
record is the aggregate jailer/seccomp/macOS-containment outcome. This scoped
result does not retroactively certify that later row.

## Terminal multiprocess isolation outcome

The complete applicable outcome is therefore one fixed launcher and one fixed
worker per VM; authenticated and redacted lifecycle state; failure-atomic typed
authority; replacement-safe publication; bounded cancellation, reap, and crash
recovery; and noninterchangeable concurrent sessions. Terminal platform limits
and operator responsibilities are composed at their exact boundaries.

No new root helper, service, entitlement, topology, sudo-dependent product
path, dynamic broker, or restart loop is introduced by this certification.
The result reuses the already executed signed process tests and can be audited
without elevated privileges.

## Certification

The scoped terminal gate is:

```console
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --multiprocess-isolation-final
```

It composes ordinary delivery validation, the canonical authority, exact
source blobs and clauses, terminal dependencies, current-tree evidence,
residual classifications, the unrelated-record digest, and the single checked
inventory row. The ordinary delivery command also validates the authority.

The global `--final` gate remains stronger and intentionally fails until every
independent audit, feasible, and platform decision in the full inventory is
terminal.
