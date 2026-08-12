# Firecracker v1.16.0 macOS Isolation Contract

This ledger owns the three composite isolation records in
[`capabilities.json`](capabilities.json) and the exact Linux isolation leaves
that have terminal public-macOS conclusions. The pinned Firecracker baseline is
commit `d83d72b710361a10294480131377b1b00b163af8`.

Firecracker's Linux jailer, seccomp, namespaces, cgroups, privilege
transitions, and production-host guidance are observable requirements to
evaluate. Their Linux mechanisms are not automatically portable to macOS.
The complete product security model, authority grammar, failure ordering, and
non-goals live in [macOS Host Security Model](../../../docs/security.md).

## Delivered Production Boundary

The direct `bangbang` executable remains uncontained. The production entry
point has one fixed nested topology:

| Code object | Fixed identity and path | Authority |
| --- | --- | --- |
| Outer app | `Bangbang.app`, `dev.bangbang`, `Contents/MacOS/bangbang` | Unsandboxed launcher; no App Sandbox or Hypervisor entitlement in repository-produced bundles |
| Worker app | `Contents/Helpers/BangbangWorker.app`, `dev.bangbang.worker`, `Contents/MacOS/bangbang-worker` | VMM worker; exactly App Sandbox and Hypervisor in the networkless profile, with the separately documented vmnet profile when explicitly selected |

Both code objects use Hardened Runtime. Production assembly signs and inspects
the worker before the outer app, verifies the nested bundle, publishes through
a private no-clobber staging transaction, and excludes integration-only grant
probes. Ad-hoc signing supports local validation; it is not Developer ID,
notarization, or authenticated distribution evidence.

The launcher derives the worker from its own fixed bundle layout, validates
static and suspended live code, and spawns with default-close descriptor
inheritance. It passes only standard streams plus the fixed lifecycle, grant,
vsock-broker, vhost-user-broker, and retained-block-control endpoints. The
worker receives a marker-only environment and enters an identity-checked
private working directory before public processing.

The closed lifecycle-v5 session authenticates peer PID, credentials, process
session, code identity, random session identity, direction and sequence. It
binds exact resource limits and one immutable vmnet authority, requires an
empty or populated grant transaction before `Proceed`, and reports bounded
path-free readiness and terminal outcomes. Daemon mode keeps one same-code
supervisor and publishes its PID only after worker readiness is acknowledged.

## Trust and Resource Authority

The fixed launcher, package metadata, and nested worker are trusted product
components. API/CLI input, guest input, host paths, resource contents, and HVF
exits remain untrusted. Errors expose stable categories rather than paths,
identities, bookmark bytes, signing output, or worker payloads.

Contained mode authorizes sealed/container resources plus one bounded startup
grant batch. The launcher opens and validates sources without following
symlinks, records descriptor identity and access, rejects aliases, and prepares
the complete batch before spawn. The worker revalidates every descriptor and
publishes authority only after an exact atomic commit.

The closed grant roles cover:

- read-only config, metadata, kernel, initrd, snapshot state/memory/root, and
  snapshot-pager stream inputs;
- repeatable read-only or read-write block and pmem backings;
- singleton write-only logger, metrics, and serial sinks;
- create-children API, vsock, and snapshot-output directories; and
- repeatable connect-only vhost-user socket directories.

Mutable-directory authority is anchor- and identity-bound. API and vsock
listeners are published exclusively through the signed worker boundary;
contained guest-initiated vsock and vhost-user connects use separate closed
launcher facets that exchange only the validated selector and connected stream
descriptor. Snapshot publication records exact staging identities so cleanup
removes only an owned inode and preserves replacements.

Networkless production rejects positive vmnet authority before worker spawn.
The explicit vmnet profile binds the documented entitlement dictionary,
application/team relationship, bridge allowlist, and active-interface maximum.
It does not claim repository-owned signing credentials or certified external
vmnet connectivity.

The exact implementation and security rules are maintained in:

- [Production Bundle and Signed Worker Boundary](../../../docs/security.md#production-bundle-and-signed-worker-boundary)
- [Startup Grant Authority](../../../docs/security.md#startup-grant-authority)
- [vmnet Host Policy Boundary](../../../docs/security.md#vmnet-host-policy-boundary)
- [Multi-Process Operation](../../../docs/security.md#multi-process-operation)

## Remaining External Isolation Work

The following independent outcomes remain under delivery issue
[#1351](https://github.com/seven332/bangbang/issues/1351):

- broader external vmnet connectivity, cleanup, and per-VM network policy under
  #1378;
- positive production vmnet execution and approved credentials under #1378;
  and
- Developer ID/team possession, notarization, launch constraints, and release
  policy.

General dynamic brokerage, hard revocation, cross-filesystem atomic socket
publication, and automatic restart are explicit nonclaims or operator-owned
product choices rather than unimplemented Firecracker compatibility producers.

The final jailer/seccomp/macOS containment composition is terminal under
#1918. No isolation record remains `missing-platform-feasible`; the three
external records above remain `audit-required`.

## Terminal jailer uid/gid platform limit

Firecracker's public `--uid` and `--gid` contract accepts arbitrary numeric
targets and installs them after privileged jail setup. Bangbang retains only
the current non-root caller identity in the production launcher/worker
topology. A root caller requesting either retained root or a permanent numeric
transition is rejected by shared launch-policy validation with the fixed
redacted `invalid production launch policy` result before session creation, worker spawn,
grants, publication, or guest work.

The terminal limit is not based on the absence of Darwin credential syscalls.
The controlled #1904 experiment completed exact `/private/tmp` target-root
construction, independent launcher/worker authority, permanent credential
transition, launcher-created target-session adoption, grants, and ordinary
lifecycle. After simulated launcher loss, however, the mandatory App Sandbox
worker moved outside the target root but received `permission-denied` while
removing its exact empty inner session. Exact empty-only outer cleanup was
therefore `busy`; only the surviving unsandboxed launcher recovery path could
converge. The final scan found zero residue, and the elevation wrapper only
scanned the product root.

Passing descriptors earlier does not grant the unchanged sandboxed worker the
required pathname-mutation authority. A privileged helper/service, a new
entitlement or sandbox extension, an account/configurable path, early unlinking
that abandons linked foreground recovery, or wrapper/launcher-only cleanup
changes the fixed accepted topology or fails independent worker cleanup.
Accordingly `tool-argument:jailer/uid` and `tool-argument:jailer/gid` are exact
`proven-platform-impossible` leaves. This conclusion does not complete
`corpus:production-host`; it did not by itself complete `corpus:jailer` or the
aggregate `jailer/run`, which #1912 later certifies through the independent
whole-operation authority below.

## Terminal jailer configurable-chroot platform limit

Firecracker's `--chroot-base-dir` selects the parent of its constructed jail
root and participates in Linux mount-namespace, bind-mount, `pivot_root`,
old-root detachment, chroot, resource, and exec behavior. Public macOS chroot
and spawn-root inheritance exist, but they do not supply Linux mechanism
parity.

The exact signed evidence covers both public orderings. In #1373 the
unsandboxed launcher enters the validated root while the mandatory App Sandbox
+ Hypervisor worker receives permission denied from direct chroot after
validating and entering the directory by descriptor. In #1884 the launcher
stages the complete fixed signed bundle plus current dyld, enters and
reattests the root, and receives a child PID; the worker exits before its first
authenticated Ready record in every repeated and concurrent case. The
unchrooted control completes real HVF create/destroy, but the exact inherited
child sub-cause remains unknown.

Apple gives IPC-using RootDirectory jobs no supported guarantee without a
system-identical library stack, while its no-bootstrap-IPC precaution conflicts
with mandatory App Sandbox/vmnet behavior. A shared-cache/system-root copy,
helper/service, entitlement change, private API, or cwd/descriptor alias is
host-coupled, changes the fixed topology, or changes the requested semantics.
The product therefore rejects the exact pre-delimiter option before consuming
its value with
`unsupported Firecracker jailer isolation argument on macOS: --chroot-base-dir`.
The conclusion is limited to the supported fixed topology and does not complete
`corpus:production-host`; it did not by itself complete the two jailer
aggregate records.

## Certified Linux runtime isolation exclusions

The exact Firecracker mechanisms below have terminal public-macOS conclusions.
They are not claims that the narrower production boundary is Linux-equivalent:

| Firecracker v1.16 contract | Current public macOS conclusion | Rejected aliases |
| --- | --- | --- |
| Default, empty, or custom `vmm`/`api`/`vcpu` classic-BPF programs installed per Linux thread with `PR_SET_NO_NEW_PRIVS` and `seccomp(SECCOMP_SET_MODE_FILTER)` | No public macOS syscall or API installs the requested per-thread filter map. `--no-seccomp` and `--seccomp-filter` are rejected before filter-path access, configuration-file access, VMM/backend construction, readiness, or socket publication. `corpus:seccomp` is terminal with them; offline artifact compilation remains separately implemented. | App Sandbox is fixed signed resource policy; private Seatbelt is unsupported; Endpoint Security is privileged event monitoring; parsing a BPF artifact without installation is not enforcement. |
| `--cgroup`, `--cgroup-version`, and `--parent-cgroup` select Linux v1/v2 hierarchies, write arbitrary controller files, enable/inherit parents, and attach the PID through `tasks` or `cgroup.procs` | macOS exposes no generic controller filesystem, hierarchy version, delegation, parent placement, or attach identity. Exact/attached/separated forms are fixed named rejections before grants, profile/staging, session creation, spawn, or publication. | Darwin rlimits are scalar inherited process limits; App Sandbox, launchd resource classes, nice, and QoS do not provide cgroup identity or controller semantics. |
| `--netns PATH` opens a Linux namespace handle with no-follow and calls `setns(CLONE_NEWNET)` before later jail setup | macOS exposes no path-named host-process network namespace join. The path is never opened and the fixed named rejection precedes all launcher mutation. | Network Extension is an entitled VPN extension; vmnet configures guest networking; App Sandbox network policy does not select a host network stack by path. |
| `--new-pid-ns` calls `clone(CLONE_NEWPID)` and makes the first child PID 1 inside a nested process view | macOS exposes no nested PID namespace or remapped PID 1 contract. The fixed named rejection precedes session or worker creation. | Process groups, sessions, supervision, and Endpoint Security retain host PID visibility and identity. |

`JailerIsolationArgument` is a closed public enum whose name and diagnostic
surfaces expose only fixed argument names. Rejections occur while parsing the
pre-delimiter policy, before grants, staging, session creation, spawn,
publication, or worker output; post-delimiter worker arguments remain opaque.

## Terminal aggregate jailer outcome

#1912 certifies the complete observable pinned jailer grammar and operation
without weakening any terminal mechanism conclusion. The checked
[`jailer-aggregate-audit.json`](jailer-aggregate-audit.json) is a bijection over
all 13 argument leaves, the 16 ordered operations in `docs/jailer.md`, its
seven document sections, nine exact local evidence profiles, and eleven closed
nonclaims. It pins the upstream document, parser, environment, and chroot
source blobs and rejects missing, duplicate, reordered, unknown, stale-anchor,
or unrelated evidence.

The applicable macOS operation is the existing fixed topology: policy
validation precedes mutation; the nested worker is separately signed and
statically/live validated; default-close spawn passes a marker-only environment
and the closed descriptor allowlist; private no-clobber state, exact
`RLIMIT_NOFILE`/`RLIMIT_FSIZE`, launcher-owned ID/timing injection, real HVF
execution, foreground or detached supervision, cancellation, concurrency, and
replacement-safe cleanup are composed as one run. A fixed immutable signed
worker is the code-integrity outcome; this is not a claim of a literal per-run
executable copy or absence of shared read-only code pages.

The exact uid/gid, configurable-chroot, cgroup, network-namespace, and PID-
namespace leaves above remain terminal platform limits. Linux device nodes,
arbitrary operator-trusted paths, external vmnet connectivity, production-host
deployment, Developer ID/notarization, and restart/service policy remain
explicit nonclaims or independently owned work. The exact transition is
`377/5/3/33` to `379/3/3/33`; only `corpus:jailer` and
`tool-operation:jailer/run` move to `implemented-and-verified`, and a checked
digest pins every unrelated inventory record.

The complete human mapping and scoped command are in the
[aggregate jailer contract](jailer-aggregate-contract.md).

## Terminal multiprocess isolation outcome

#1914 separately certifies
`semantic.isolation:multiprocess-concurrency-redaction-and-failure-atomicity`
from the 13 exact multiprocess clauses in the pinned Firecracker design and
production-host documents. One launcher/worker/VM boundary, authenticated and
redacted lifecycle state, failure-atomic typed grants, replacement-safe
publication, bounded crash recovery, and noninterchangeable concurrent
sessions provide the applicable outcome. The checked mapping distinguishes
operator-owned overwatching, workload policy, and host-path allocation from
product obligations and composes the terminal uid/gid result without claiming
positive unique credentials or malicious same-bundle sibling isolation.

The exact transition is `379/3/3/33` to `380/3/2/33`; the complete authority,
residual classification, nonclaims, and scoped command are in the
[multiprocess isolation contract](multiprocess-isolation-contract.md).

## Terminal host-resource authority outcome

#1916 separately certifies
`semantic.isolation:host-resource-authority-and-brokerage` from 30 ordered
clauses in the pinned design, jailer, network-setup, and production-host
documents. Strict no-follow preflight, the exact 17-role/five-access grant
surface, failure-atomic descriptor and anchored-directory adoption, fixed
session broker facets, transactional storage/snapshot authority, output and
device bounds, replacement-safe cleanup, cancellation, and concurrent
noninterchangeability provide the applicable fixed macOS result.

The checked mapping adds the Wave 7-owned `corpus:design` source, preserves
#1378's positive vmnet evidence as independent, composes the terminal Linux
mechanism conclusions, and classifies general brokerage, hard revocation,
cross-filesystem publication, host allocation, restart, and deployment at
their exact nonclaim/operator/external boundaries.

The exact transition is `380/3/2/33` to `381/3/1/33`; the complete authority,
resource map, residual classification, nonclaims, and scoped command are in
the [host-resource authority contract](host-resource-authority-contract.md).

## Terminal jailer/seccomp containment outcome

#1918 certifies
`semantic.isolation:jailer-seccomp-and-macos-containment-outcomes` from 46
ordered clauses in the pinned design, jailer, production-host, seccomp, and
seccompiler documents. It composes the immutable signed App Sandbox/HVF
worker topology, authenticated lifecycle, closed environment and descriptors,
private namespace, limits, typed resource authority, redaction, supervision,
cleanup, cancellation, and concurrent noninterchangeability. Portable
seccompiler generation is real, but macOS Linux-filter installation is not
claimed; all exact Linux mechanism identities retain their terminal platform
limits.

Positive vmnet execution and approved credentials remain under #1378, while
deployment and broad host/operator policy remain in `corpus:production-host`.
The exact transition is `381/3/1/33` to `382/3/0/33`; the complete source map,
dependencies, residual classifications, nonclaims, and scoped command are in
the [jailer/seccomp containment contract](jailer-seccomp-containment-contract.md).

## Evidence Map

| Claim | Implementation | Validation |
| --- | --- | --- |
| Bundle assembly, signing, fixed layout, and exclusive publication | `crates/launcher/src/package.rs`, `crates/launcher/src/macos/code_sign.rs`, `crates/launcher/src/macos/publish.rs` | `crates/launcher/tests/production_bundle_e2e.rs`, `scripts/build-production-bundle.sh` |
| Suspended worker validation, lifecycle, limits, daemon supervision, and cleanup | `crates/launcher/src/supervisor.rs`, `crates/launcher/src/macos/spawn.rs`, `crates/session/src/lib.rs` | launcher/session unit tests and signed production-bundle cases |
| Typed startup grants and contained resource consumers | `crates/launcher/src/grant_manifest.rs` and the owning VMM/device consumers | focused grant tests plus signed direct and production device/snapshot matrices |
| Stable Linux-mechanism exclusions | launcher policy parser and process CLI handling | focused unit/process tests, signed production-bundle pre-mutation cases, [compatibility](../../../docs/firecracker-compatibility.md#runtime-isolation-platform-exclusions), and [security](../../../docs/security.md#certified-linux-runtime-isolation-exclusions) |
| Stable jailer uid/gid platform limit | `crates/launcher/src/launch_policy.rs`, `crates/launcher/src/macos/daemon.rs`, and `crates/launcher/src/supervisor.rs` | launch-identity unit tests, signed exact-help/policy validation, the [controlled platform result](elevated-bootstrap-evidence.md#product-uidgid-runtime-root-platform-gate), [compatibility](../../../docs/firecracker-compatibility.md#jailer-uidgid-platform-limit), and [security](../../../docs/security.md#jailer-uidgid-fixed-topology-platform-limit) |
| Stable jailer configurable-chroot platform limit | `crates/launcher/src/error.rs` and `crates/launcher/src/launch_policy.rs` | closed parser unit tests, signed pre-mutation rejection, the [controlled platform result](elevated-bootstrap-evidence.md#configurable-chroot-fixed-topology-platform-limit), [compatibility](../../../docs/firecracker-compatibility.md#jailer-configurable-chroot-platform-limit), and [security](../../../docs/security.md#jailer-configurable-chroot-fixed-topology-platform-limit) |

All signed production cases run through
[`scripts/run-integration-tests.sh`](../../../scripts/run-integration-tests.sh);
the maintained command selection is in
[Testing Guide](../../../docs/testing.md#running-tests). The normal bundle and
integration-only probe bundle remain distinct, and unsupported hosts may skip
execution only through the wrapper's explicit policy.

## Inventory Disposition

The checked inventory is authoritative for the exact isolation leaves,
evidence references, and dispositions. Linux seccomp, cgroup, network
namespace, and PID namespace records are terminal only for the named stable
macOS exclusions. The offline seccompiler is a separate implemented artifact
tool and does not enforce runtime seccomp. The final composition is terminal
under #1918. The uid/gid
argument leaves and configurable chroot are separately terminal at the fixed
topology above. The aggregate jailer corpus and run records are separately
terminal under #1912, and the multiprocess aggregate is terminal under #1914;
the host-resource authority is terminal under #1916. `corpus:production-host`
and the two network/vmnet records retain their independent audit-required
states.
