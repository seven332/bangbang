# Firecracker v1.16.0 macOS isolation contract

This document is the human-owned audit for the three composite isolation
records in [`capabilities.json`](capabilities.json). The pinned Firecracker
baseline is commit `d83d72b710361a10294480131377b1b00b163af8`.
Firecracker's Linux jailer, seccomp, namespaces, cgroups, privilege transitions,
resource ownership, and production-host guidance are upstream outcomes to
evaluate; their implementation mechanisms are not directly portable to macOS.

## Delivered production boundary

The direct `bangbang` executable remains uncontained. The additive production
entry point has one immutable topology shared by the package tool and launcher:

| Code object | Fixed identity and path | Authority |
| --- | --- | --- |
| Outer app | `Bangbang.app`, `dev.bangbang`, `Contents/MacOS/bangbang` | Unsandboxed launcher; no App Sandbox or Hypervisor entitlement in the package produced by this repository. |
| Worker app | `Contents/Helpers/BangbangWorker.app`, `dev.bangbang.worker`, `Contents/MacOS/bangbang-worker` | VMM worker; exactly App Sandbox and Hypervisor entitlements in the package produced by this repository. |

Both code objects use Hardened Runtime. The package tool signs the worker first
and the outer app last with one supplied identity, inspects each result, then
strictly verifies the nested bundle. The default identity `-` is ad-hoc local
validation, not authenticated provenance, Developer ID possession, or
notarization evidence.

Production assembly in
[`package.rs`](../../../crates/launcher/src/package.rs) uses a private mode-0700
staging tree beside an absent final destination. It accepts only the fixed
checked-in metadata and bounded regular-file test resources without symlinks.
Publication uses a same-volume exclusive rename implemented in
[`publish.rs`](../../../crates/launcher/src/macos/publish.rs); it never replaces
or merges an existing final app. Failure cleanup owns only the unpublished
staging tree. The normal
[`build-production-bundle.sh`](../../../scripts/build-production-bundle.sh)
wrapper exposes no resource overlay.

Runtime layout validation in
[`layout.rs`](../../../crates/launcher/src/layout.rs) derives the worker only
from the launcher's own exact location and rejects missing, nonregular, or
symlinked fixed entries. Security.framework validation in
[`code_sign.rs`](../../../crates/launcher/src/macos/code_sign.rs) applies strict,
all-architecture, nested, and symlink-restriction checks plus compiled
identifier requirements. It then reads the signed entitlement dictionaries and
requires no outer entitlements plus exactly App Sandbox and Hypervisor Boolean
true values on the worker, and requires the Hardened Runtime signature flag on
both code objects. This rejects unsigned modification of the published package
at rest. It neither anchors a certificate/team nor prevents a
same-user attacker from replacing the whole package with separately validly
signed code, and it is not atomic against mutation between validation and
execution. Kernel launch constraints and authenticated distribution policy are
not claimed.

The launcher in [`supervisor.rs`](../../../crates/launcher/src/supervisor.rs)
passes every worker argument byte in order, inherits standard streams, owns one
child, forwards `SIGINT` and `SIGTERM`, preserves ordinary exit status, and maps
other signaled-child termination to `128 + signal`. Its macOS kqueue path in
[`supervise.rs`](../../../crates/launcher/src/macos/supervise.rs) processes
signal events in an event batch before reaping and never signals after reap, so
a reused PID cannot receive a late forwarded signal.

## Trust and resource authority

The outer launcher, fixed package metadata, and signed nested executable are
trusted product components. Guest memory and device input, API requests, CLI
host paths, configuration contents, and HVF exits remain untrusted inputs to the
worker. Product errors expose stable categories rather than package paths,
signing identities, platform-tool output, or worker payloads.

Contained mode currently authorizes only app-container paths and resources
sealed into the worker bundle before signing. The normal product embeds no guest
resources. The launcher does not open, validate, transfer, or revoke kernel,
initrd, disk, snapshot, vsock, observability, vmnet, or API-socket resources for
the worker. Operators may still use the direct uncontained executable for the
existing host-path surface, but that mode is not evidence for the production
containment records.

The following remain feasible work owned by #1351:

- an authenticated launcher/worker session and readiness contract;
- security-scoped external-file grants or descriptor transfer with
  resource-specific authority, bounds, replacement detection, and cleanup;
- vmnet entitlement/provisioning and per-VM network policy;
- launcher/worker failure matrices, parent-death behavior, and restart policy;
- exact macOS outcome mapping for jailer, seccomp, namespace, cgroup,
  privilege, resource-limit, and production-host requirements;
- Developer ID/team possession, notarization, launch constraints, and release
  policy.

## Executable validation

[`production_bundle_e2e.rs`](../../../crates/launcher/tests/production_bundle_e2e.rs)
runs only through
[`run-integration-tests.sh`](../../../scripts/run-integration-tests.sh). The
runner builds the release binaries, assembles and signs the real fixed bundle,
and compiles the disabled-by-default target before an unsupported CI host may
skip execution. Supported Apple Silicon execution proves:

- exact identifiers, entitlement separation, Hardened Runtime, and strict
  recursive signature validity;
- unchanged help/output and representative nonzero worker status forwarding;
- rejection before worker output when a private bundle copy has a missing or
  modified worker;
- path-redacted App Sandbox denial for an outside config file;
- container API readiness, `SIGINT` and `SIGTERM` forwarding, successful
  terminal status, and owned-socket cleanup; and
- a test-only sealed kernel/initrd/config starting a real sandboxed HVF guest
  through the launcher and ending successfully through PSCI `SYSTEM_OFF`.

Readiness events and bounded deadlines replace fixed sleeps. Destructive cases
operate on private copies, so later checks continue to use the canonical signed
bundle.

## Inventory disposition

This slice changes the following records from `audit-required` to
`missing-platform-feasible`, with #1351 as the delivery issue:

- `semantic.isolation:host-resource-authority-and-brokerage`
- `semantic.isolation:jailer-seccomp-and-macos-containment-outcomes`
- `semantic.isolation:multiprocess-concurrency-redaction-and-failure-atomicity`

The delivered containment/package/supervisor subset above is real but does not
complete any of those composite records. The broad `jailer`, `seccomp`,
`seccompiler`, and `production-host` corpus records remain `audit-required`.
Neither this audit nor the executable evidence is direct Firecracker jailer
parity.
