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

## Remaining Feasible Isolation Work

The following remain under delivery issue
[#1351](https://github.com/seven332/bangbang/issues/1351):

- general dynamic post-Ready brokerage and hard revocation;
- broader external vmnet connectivity, cleanup, and per-VM network policy;
- arbitrary uid/gid transition, configurable chroot ownership, and any
  installer-owned or elevated service needed to support them;
- automatic restart/reconnect and long-lived broker/service policy;
- cross-filesystem socket publication; and
- Developer ID/team possession, notarization, launch constraints, and release
  policy.

These gaps keep the three composite isolation records
`missing-platform-feasible`:

- `semantic.isolation:host-resource-authority-and-brokerage`
- `semantic.isolation:jailer-seccomp-and-macos-containment-outcomes`
- `semantic.isolation:multiprocess-concurrency-redaction-and-failure-atomicity`

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

## Evidence Map

| Claim | Implementation | Validation |
| --- | --- | --- |
| Bundle assembly, signing, fixed layout, and exclusive publication | `crates/launcher/src/package.rs`, `crates/launcher/src/macos/code_sign.rs`, `crates/launcher/src/macos/publish.rs` | `crates/launcher/tests/production_bundle_e2e.rs`, `scripts/build-production-bundle.sh` |
| Suspended worker validation, lifecycle, limits, daemon supervision, and cleanup | `crates/launcher/src/supervisor.rs`, `crates/launcher/src/macos/spawn.rs`, `crates/session/src/lib.rs` | launcher/session unit tests and signed production-bundle cases |
| Typed startup grants and contained resource consumers | `crates/launcher/src/grant_manifest.rs` and the owning VMM/device consumers | focused grant tests plus signed direct and production device/snapshot matrices |
| Stable Linux-mechanism exclusions | launcher policy parser and process CLI handling | focused unit/process tests, signed production-bundle pre-mutation cases, [compatibility](../../../docs/firecracker-compatibility.md#runtime-isolation-platform-exclusions), and [security](../../../docs/security.md#certified-linux-runtime-isolation-exclusions) |

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
tool and does not enforce runtime seccomp. The three composite records remain
nonterminal until #1351's broader feasible work is complete.
