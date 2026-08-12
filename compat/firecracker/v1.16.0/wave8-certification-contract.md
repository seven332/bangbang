# Firecracker v1.16 Wave 8 platform-feasible certification

This contract is the final source-tree authority for #1348's platform-feasible
Firecracker v1.16.0 delivery. It certifies one capability transition owned by
#1881 on the native Apple Silicon/Hypervisor.framework target. It does not
close or reclassify outcomes that still require external privilege or
credentials.

The checked machine-readable authority is
[`wave8-certification-audit.json`](wave8-certification-audit.json). Its strict
Rust model, validator, certifier, and adversarial tests reject partial
transitions, representative-sample closure, stale or unrelated evidence, and
unreviewed platform exclusions.

| Capability | Delivery | Disposition |
| --- | --- | --- |
| `semantic.cross-capability:state-errors-metrics-security-and-snapshots` | #1881 | `implemented-and-verified` |

## Certified interaction matrix

The matrix has seven domains:

1. lifecycle state;
2. API errors;
3. logger and metrics observability;
4. containment and resource authority;
5. devices;
6. networking and MMDS; and
7. snapshots and restore.

The validator derives every canonical combination from this fixed domain order
and requires all 21 unordered pairs. A stored pair count cannot satisfy the
gate. Each pair instead names every selected leaf scenario whose exact domain
membership covers it, and each scenario resolves to a fixed tracked path and
literal function anchor.

Four existing product leaves provide the complete selected matrix:

| Scenario | Boundary | Certified role |
| --- | --- | --- |
| `portable-snapshot-serialization` | Portable API-server unit boundary | Serializes lifecycle/API/MMDS/periodic work with synchronous snapshot creation, observes cancellation, and leaves no escaped artifact. |
| `signed-direct-live-patch` | Signed direct VMM over real HVF | Combines lifecycle idempotency, strict failure-atomic errors, logger/metrics, device and network/MMDS live patch, capture-ready snapshot state, and terminal cleanup. |
| `signed-production-snapshot-containment` | Signed launcher plus App Sandbox worker over real HVF | Combines grant/resource containment, lifecycle and telemetry, devices, network/MMDS, native-v2 continuation, failure handling, and cleanup. |
| `signed-production-claim-rejection` | Signed production launch boundary | Binds wrong or missing typed boot claims to redacted public errors and proves the resource pair is not consumed. |

The first, second, and third scenarios overlap intentionally. The portable
scenario is the deterministic concurrency/cancellation oracle; the direct
scenario proves the public VMM path; the production scenario proves the real
grant and App Sandbox boundary. The fourth scenario supplies the otherwise
missing API-error/resource-authority pair. A broad passing test from a
different role cannot replace one of these anchors.

This is completeness for the fixed reviewed interaction model, not a proof of
all possible runtime interleavings. Existing focused logger, metrics,
fatal-exit, cross-process snapshot, configuration-failure, and device tests
remain supporting evidence and continue to run in the repository test matrix.

## Wave 8 platform exclusion re-challenge

Wave 8 independently partitions every proven-platform-impossible record into
four exact public-mechanism reviews at the #1881 phase. Each record must also retain its own
upstream contract, public platform evidence, credible alternatives, stable
behavior, focused tests, compatibility and security text, and Challenge link
in the capability inventory.

| Mechanism | Records | Current result |
| --- | ---: | --- |
| X86 CPUID and MSR identities | 13 | Public arm64 Hypervisor.framework has no identity-preserving x86 CPUID leaf/subleaf/register or MSR selector API. Silent acceptance, cross-architecture translation, and a different emulator/backend change the request. |
| Linux KVM feature and CPU-template identities | 7 | HVF feature and system registers do not preserve Linux KVM capability numbers, vCPU-init feature-word identity, or a Neoverse V1 source model. Private mappings, mask reinterpretation, and a different source CPU change the contract. |
| Exact Linux hugetlbfs `2M` memory | 2 | Current public XNU accepts its superpage selector only for x86; arm and arm64 SPTM pmap state that superpages are unsupported, and the public arm64 allocation probe returns `KERN_INVALID_ARGUMENT`. Alignment, batching, and HVF's 4/16-KiB IPA granules do not supply Firecracker's hugetlbfs pool, backing, `MAP_NORESERVE`/`SIGBUS`, balloon, dirty, and restore semantics. |
| Linux seccomp, cgroup, network-namespace, and PID-namespace mechanisms | 8 | Public macOS/XNU has no equivalent Linux syscall/action/controller/namespace identity. App Sandbox, rlimits, launchd/QoS, Network Extension, vmnet, Endpoint Security, private APIs, and a Linux sidecar have different security and process contracts. |

The checked authority pins the current Firecracker sources, Apple developer
interfaces, and public XNU `xnu-12377.121.6` sources used by these reviews.
Future platform evidence that changes a mechanism conclusion requires a fresh
ID-by-ID Challenge and inventory update; the family ledger alone cannot make a
record impossible.

## Exact retained external boundary

At the Wave 8 transition, the inventory was exactly 377 implemented, eight
audit-required, three missing-platform-feasible, and 30
proven-platform-impossible records. The eleven nonterminal outcomes are an
exact historical external-evidence partition:

- six #1373 audit-required outcomes: `corpus:jailer`,
  `corpus:production-host`, `jailer/chroot-base-dir`, `jailer/gid`,
  `jailer/uid`, and `jailer/run`;
- two #1378 audit-required outcomes: `corpus:network-setup` and
  `semantic.network:virtio-net-vmnet-policy-and-connectivity`; and
- three #1351 missing-platform-feasible isolation semantics covering host
  resource authority, the jailer/containment result, and multiprocess
  concurrency/redaction/failure atomicity.

The #1373 set requires a same-host executor that has both root authority and
real HVF execution. The #1378 set requires caller-owned Apple-approved signing
and profile authority plus an isolated vmnet fixture. Hosted sudo without HVF,
local HVF without noninteractive root, missing credentials, a skipped test, or
an unexecuted harness does not satisfy either gate. These are feasible external
handoffs and are not platform-impossible classifications.

That same-host execution gate has since run on the controlled Apple Silicon
host. The [elevated bootstrap evidence](elevated-bootstrap-evidence.md) records
#1373's successful unsandboxed root control and repeated direct-worker chroot
denial, plus #1884 and #1885 follow-ups. In #1884, the same unchrooted signed
worker completed real HVF create/destroy; after the launcher entered an exact
root containing the complete signed bundle and current dyld, `posix_spawn`
returned success but the worker exited before the first application record.
#1885 separately completed worker-first and launcher-second public credential
transitions without chroot for mapped ordinary and SDK-maximum unmapped classes;
zero remained retained-root/no-drop. Target runtime/resources, lifecycle/API,
guest/HVF continuation, and final uid/gid disposition were still unmeasured at
that checkpoint. The six #1373 rows therefore remain `audit-required` in the
immutable Wave 8 snapshot. No later evidence or disposition PR rewrites that
checked historical boundary.

#1904 subsequently exercised the accepted product topology at the same exact
macOS 26.5.2 / SDK 26.5 capable-host boundary. Fixed `/private/tmp` target-root
construction, independent endpoint authority, permanent transition,
launcher-created target-session adoption, representative grants, and terminal
lifecycle all completed. After launcher loss, the mandatory App Sandbox worker
moved outside the root but its descriptor-relative removal of the exact empty
inner session returned `permission-denied`; exact outer cleanup was `busy`.
The unsandboxed launcher recovery path then removed the same objects and the
wrapper's scan found zero residue. This proves that the worker cannot own the
required independent cleanup in the fixed no-helper topology.

The exact post-Wave-8 jailer uid/gid successor is `377/6/3/32`. It moves only
`tool-argument:jailer/gid` and `tool-argument:jailer/uid` from audit-required to
proven-platform-impossible. At that phase the nine nonterminal outcomes were:

- four #1373 audit-required outcomes: `corpus:jailer`,
  `corpus:production-host`, `jailer/chroot-base-dir`, and `jailer/run`;
- two #1378 audit-required outcomes: `corpus:network-setup` and
  `semantic.network:virtio-net-vmnet-policy-and-connectivity`; and
- three #1351 missing-platform-feasible isolation semantics.

That inventory retained four #1373 rows, not six #1373 rows.
The two terminal uid/gid exclusions require their own pinned upstream
contracts, #1904 platform evidence, five rejected alternatives, stable public
rejection, focused tests, compatibility/security documentation, and #1905
Challenge result. They do not complete the broader jailer or production-host
records.

The later configurable-chroot successor is exactly `377/5/3/33`: 377
implemented, five audit-required, three missing-platform-feasible, and 33
proven-platform-impossible. It moves only
`tool-argument:jailer/chroot-base-dir`. The eight nonterminal outcomes at that
phase were:

- three #1373 audit-required outcomes: `corpus:jailer`,
  `corpus:production-host`, and `jailer/run`;
- the same two #1378 audit-required outcomes; and
- the same three #1351 missing-platform-feasible isolation semantics.

The chroot exclusion separately requires pinned Firecracker root construction,
Apple public root/runtime/IPC evidence, the #1373/#1884 controls, reviewed
alternatives, a stable pre-value rejection, focused portable and signed tests,
compatibility/security documentation, and #1908 Challenge authority. It
preserves public chroot and spawn-root inheritance, the unknown inherited child
sub-cause, and the aggregate records' independent nonterminal state.

## Exact aggregate-jailer successor

The aggregate-jailer successor is exactly `379/3/3/33`: 379
implemented, three audit-required, three missing-platform-feasible, and 33
proven-platform-impossible. It moves only `corpus:jailer` and
`tool-operation:jailer/run` to implemented-and-verified. The six remaining
nonterminal outcomes are:

- one #1373 audit-required outcome: `corpus:production-host`;
- the same two #1378 audit-required outcomes; and
- the same three #1351 missing-platform-feasible isolation semantics.

The checked [aggregate jailer contract](jailer-aggregate-contract.md) requires
the complete pinned jailer corpus, 13 ordered argument leaves, 16 ordered
operation steps, seven corpus sections, current-tree evidence, and an exact
digest of every unrelated inventory record. It composes five implemented
argument leaves with eight terminal fixed-topology exclusions without changing
any leaf disposition. It does not claim Linux mechanism parity,
production-host deployment, external vmnet connectivity, or completion of the
three isolation composites at that historical phase.

## Exact multiprocess-isolation successor

The multiprocess-isolation successor is exactly `380/3/2/33`: 380
implemented, three audit-required, two missing-platform-feasible, and 33
proven-platform-impossible. It moves only
`semantic.isolation:multiprocess-concurrency-redaction-and-failure-atomicity`
to implemented-and-verified. The five remaining nonterminal outcomes are one
#1373 audit-required outcome, the same two #1378 audit-required outcomes, and
two #1351 missing-platform-feasible isolation semantics. The checked
[multiprocess isolation contract](multiprocess-isolation-contract.md) pins 13
ordered design/production-host clauses, current-tree evidence, terminal
dependencies, residual classifications, and every explicit nonclaim.

## Exact host-resource-authority successor

The host-resource-authority successor is exactly `381/3/1/33`: 381
implemented, three audit-required, one missing-platform-feasible, and 33
proven-platform-impossible. It moves only
`semantic.isolation:host-resource-authority-and-brokerage` to
implemented-and-verified and repairs its Wave 7-owned `corpus:design` source
mapping. The four remaining nonterminal outcomes are one #1373
audit-required outcome, the same two #1378 audit-required outcomes, and the
final #1351 jailer/seccomp/macOS-containment semantic. The checked
[host-resource authority contract](host-resource-authority-contract.md) pins
30 clauses, the exact 17-role/five-access surface, current-tree evidence,
terminal and external dependencies, residuals, and every nonclaim.

## Exact jailer/seccomp containment and production-host successors

The containment successor is exactly `382/3/0/33`: 382 implemented,
three audit-required, zero missing-platform-feasible, and 33
proven-platform-impossible. It moves only
`semantic.isolation:jailer-seccomp-and-macos-containment-outcomes` to
implemented-and-verified and repairs its Wave 7-owned `corpus:design` source
mapping. The three remaining nonterminal outcomes are the one production-host
and two #1378 network/vmnet audit-required records. The checked
[containment contract](jailer-seccomp-containment-contract.md) pins five
sources, 46 clauses, terminal and external dependencies, current-tree
evidence, residual classifications, and every nonclaim.

The current production-host successor is exactly `383/2/0/33`: #1920 moves
only `corpus:production-host` to implemented-and-verified through complete
31-clause source accounting. The two #1378 network/vmnet records remain
audit-required, and all 33 platform exclusions remain unchanged.

The direct #1348 delivery-parent policy retains #1351 open and requires the
other nine preceding parents complete. #1371, #1373, #1374, #1375, and #1378
remain explicit open external branches. The offline validator checks this
declarative identity policy but never claims a live GitHub query.

## Certification commands and historical composition

The scoped terminal command is:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --wave8-final
```

It composes every Wave 7 component authority, the phase-aware Wave 7
certifier, and the Wave 8 authority. The Wave 7 artifact remains historical at
376/9/3/30 with one Wave 8 handoff. Its certifier accepts that exact historical
phase, the exact one-row Wave 8 successor at 377/8/3/30, or the exact uid/gid-
only successor at 377/6/3/32, or the exact configurable-chroot-only successor
at 377/5/3/33, the exact aggregate-jailer successor at 379/3/3/33, or the exact
multiprocess-isolation successor at 380/3/2/33, the exact
host-resource-authority successor at 381/3/1/33, or the exact containment
successor at 382/3/0/33, or the exact production-host successor at
383/2/0/33; it
rejects unrelated, count-preserving identity swaps, and partial drift.

The global `--final` mode remains stronger and intentionally fails while the
two #1378 audit-required external outcomes remain. Neither Wave
8 nor any exact later successor weakens that completion gate.

The checked command is reproducible and networkless. Live GitHub hierarchy,
assignment, review threads, pull-request checks, remote branches, merge state,
and default-branch identity are mutable delivery evidence. The PR workflow
must verify them on the reviewed head and record the PR head, merge commit,
merged-main OID, inventory totals, source comparison, signed execution, and CI
result in #1881 and #1348 after merge.

The full portable, Apple-target, signed Apple Silicon, source comparison, Kani,
review, CI, and merged-main command sequence remains canonical in
[`docs/testing.md`](../../../docs/testing.md). This contract does not duplicate
its operator setup.

## Nonclaims

This certification does not claim:

- completion of the retained root/HVF or approved-vmnet evidence;
- Linux KVM or Firecracker binary identity;
- arbitrary guest support or distinct-host snapshot portability;
- portable performance parity or thresholds;
- whole-system formal correctness or all possible runtime interleavings;
- a private API, privileged fallback, entitlement, credential, or Linux
  sidecar supplied by the repository; or
- live GitHub state from the offline Rust validator.

Any newly discovered product interaction gap must be delivered through a
separate challenged producer issue before this authority can certify it.
