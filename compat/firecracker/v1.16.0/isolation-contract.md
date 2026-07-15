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
wrapper explicitly builds without default features and exposes no resource
overlay. The integration-only grant exerciser is therefore absent from normal
product bundles; an all-features development binary is not a shippable package.

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
signed code. Kernel launch constraints and authenticated distribution policy
are not claimed. The session layer separately validates the actual suspended
worker process and repeats that live-code check after bootstrap, so the launch
authorization is not based only on a pre-spawn pathname check.

The launcher in [`supervisor.rs`](../../../crates/launcher/src/supervisor.rs)
passes every worker argument byte in order and preserves ordinary environment
entries while replacing one private bootstrap marker. The Darwin wrapper in
[`spawn.rs`](../../../crates/launcher/src/macos/spawn.rs) uses
`POSIX_SPAWN_CLOEXEC_DEFAULT | POSIX_SPAWN_START_SUSPENDED`, explicitly retains
each open standard stream, and duplicates only an unnamed lifecycle stream
endpoint to descriptor 3 plus an unnamed startup-grant datagram endpoint to
descriptor 4. The launcher dynamically validates the live
worker while suspended, resumes only the private bootstrap, reads one bounded
reserved `Hello`, verifies the now-child-attributed peer PID/credentials,
revalidates live code, and only then sends a random session identity in `Start`.

[`bangbang-session`](../../../crates/session/src/lib.rs) defines the closed
lifecycle-v2 binary contract. Frames have fixed magic/version/reserved fields, a 256-bit
identity, exact per-direction sequence numbers, fixed payload shapes, and a
4096-byte cap. Replay, sequence gaps, cross-session or wrong-role messages,
malformed/unknown/oversized/truncated data, and invalid lifecycle transitions
fail with one redacted category. State is monotonic through `Hello`, `Start`,
`Prepared`, exact `GrantsAccepted`, `Proceed`, `Starting`, optional committed API/no-API `Ready`, one
graceful `Cancel`, and path-free `Terminal`. The worker verifies matching
effective credentials and `LOCAL_PEERPID == getppid()` before and after the
gate. App Sandbox denies its Security.framework lookup of the parent, so only
the launcher code-validates its peer; this asymmetry is part of the contract.
`Hello`, `Start`, the grant transaction, and `Proceed` have absolute five-second
deadlines, and `Terminal` or EOF starts a five-second owned-process exit grace.

Grant-channel v1 uses one complete AF_UNIX datagram per record with a 1024-byte
application cap, independent random 128-bit BatchId, exact lifecycle SessionId
and sequence, closed record kind, payload length, reserved fields, and declared
descriptor count. `Begin` declares exact counts, file/directory records carry at
most one SCM_RIGHTS descriptor, bookmark fragments are contiguous, and `Commit`
must reproduce the declaration. The worker immediately owns every delivered fd,
rejects payload/control truncation or malformed ancillary data, restores
FD_CLOEXEC, independently checks access/status flags and fstat identity, and
poisons the whole staged batch on any inconsistency. No authority is visible
until Commit moves everything into one bounded session registry. Even an empty
batch requires an exact acknowledgment before `Proceed`.

The worker creates and locks one exact mode-0700 empty namespace beneath its
fixed container temp root. `Prepared` reports only device/inode. The launcher
independently derives the root and checks exact name, type, owner, mode,
device/inode, emptiness, and live lock before grant acknowledgment and
`Proceed`. No endpoint, argument,
identity bytes, or resource grant is stored there. Worker EOF cleanup covers
launcher-first death; launcher cleanup covers worker-first death; a later worker
scans at most 128 entries and removes only valid empty unlocked identity-stable
residue when both were killed. Same-identifier workers share container
authority, so this is cooperative replacement-safe ownership rather than
malicious-sibling isolation.

## Trust and resource authority

The outer launcher, fixed package metadata, and signed nested executable are
trusted product components. Guest memory and device input, API requests, CLI
host paths, configuration contents, and HVF exits remain untrusted inputs to the
worker. Product errors expose stable categories rather than package paths,
signing identities, platform-tool output, or worker payloads.

Contained mode authorizes app-container and sealed-bundle paths plus one explicit
bounded startup grant batch. The normal product embeds no guest resources. An
argv-position-one envelope names one strict manifest; otherwise worker argument
bytes remain unchanged. The launcher reads the manifest once, walks every
absolute source path component without following symlinks or accepting
`.`/`..`, opens existing regular files/directories with exact access, records
type/device/inode/status, rejects aliases, and prepares the entire RAII batch
before spawn. Paths, IDs, identity values, bookmark bytes, and contents remain
out of diagnostics.

The closed roles cover read-only startup config/metadata, kernel/initrd and
snapshot inputs; repeatable read-only/read-write drive and pmem backing;
write-only logger/metrics/serial sinks; and create-children API/vsock/snapshot
output directories. Regular-file authority is descriptor-only. Each mutable
directory combines an anchor descriptor with a bounded freshly minted ordinary
implicit bookmark. The worker explicitly starts scope, requires exact resolved
anchor identity and access, and balances scope on every exit. The platform stale
bit is private and never sufficient by itself for acceptance or rejection;
concrete resolution/scope/identity/access validation decides. Operator-supplied
or persisted bookmark bytes are unsupported.

Commit creates a redacted, session-owned, bounded registry whose adoption is
one-time by exact ID, role, and access. Mismatch never falls back to an ambient
path. Unadopted authority drops on cancellation, terminal, disconnect,
bootstrap failure, or process exit. SCM_RIGHTS duplicates kernel references, so
closing the launcher's copy is cleanup rather than revocation. The empty private
namespace still stores no resource data.

Contained mode recognizes only the exact, case-sensitive
`bangbang-grant:<GrantId>` form. Startup config and metadata claim their
singleton read-only descriptors before bounded parsing. Kernel and optional
initrd claims are validated and removed together when boot-source configuration
is applied, stored beside the public configuration, and consumed once during
boot without reopening their tag strings. Malformed, missing, mismatched,
or already-consumed tagged claims fail without changing VM configuration and
without path or role fallback. Mixed boot sources claim only tagged members and
leave ordinary members on deferred pathname opening. `GET /vm/config` may
return the authorized references; diagnostics remain redacted. Direct mode
treats the same text as an ordinary pathname.

Preflight failures before boot descriptor consumption remain retryable. Once
boot consumes a singleton grant, a later boot failure requires a fresh
contained launch and grant batch unless the boot source is successfully
replaced with ordinary paths. Cancellation, terminal exit, and disconnect
synchronize with the file authority and invalidate pending claims; already
adopted descriptor references remain cooperatively owned rather than
hard-revocable. Operators may still use the direct uncontained executable for
the broader existing host-path surface, but that mode is not evidence for the
production containment records.

The following remain feasible work owned by #1351:

- consumer adoption for block/pmem, API/vsock/observability, and snapshot
  resources;
- dynamic post-Ready delivery and any hard-revocation broker;
- vmnet entitlement/provisioning and per-VM network policy;
- automatic restart/reconnect and any long-lived broker/service policy;
- exact macOS outcome mapping for jailer, seccomp, namespace, cgroup,
  privilege, resource-limit, and production-host requirements;
- Developer ID/team possession, notarization, launch constraints, and release
  policy.

## Executable validation

[`production_bundle_e2e.rs`](../../../crates/launcher/tests/production_bundle_e2e.rs)
runs only through
[`run-integration-tests.sh`](../../../scripts/run-integration-tests.sh). The
runner first builds, assembles, and signs the normal no-default-feature release
bundle. It then builds a visibly marked integration-only bundle with the
`grant-integration-probe` feature and compiles the disabled-by-default target
before an unsupported CI host may skip execution. Supported Apple Silicon
execution proves:

- exact identifiers, entitlement separation, Hardened Runtime, and strict
  recursive signature validity;
- unchanged help/output and representative nonzero worker status forwarding;
- rejection before worker output when a private bundle copy has a missing or
  modified worker;
- default-close removal of a deliberately inheritable unexpected descriptor,
  retention of only lifecycle/grant endpoints, and malformed/incompatible
  bootstrap rejection before public processing;
- path-redacted App Sandbox denial for an outside config file;
- structured container API/no-API readiness, one-session `SIGINT`/`SIGTERM`
  cancellation, successful terminal status, and owned-socket cleanup;
- mandatory empty-batch acknowledgment, exact read-only and write-only fd
  enforcement, mutable-directory scope with outside-parent denial, typed
  mismatch rollback, redaction, signal cancellation during staging, and one
  absolute grant deadline;
- grant-bearing worker-first/launcher-first cleanup and two simultaneous
  sessions with noninterchangeable authority, plus behavioral proof that the
  normal bundle contains no test exerciser;
- worker-first and launcher-first namespace cleanup, both-killed bounded stale
  recovery, and two concurrent API sessions remaining independent when one
  worker dies;
- both sealed and external-grant config/metadata/kernel/initrd inputs starting
  real sandboxed HVF guests through no-API production launches and ending
  successfully through PSCI `SYSTEM_OFF`;
- delayed API-time atomic boot adoption retaining the opened file identities
  after pathname replacement and returning the authorized references from
  `GET /vm/config`; and
- invalid-command-line, wrong-role, and missing boot requests preserving the
  prior public configuration, with redacted grant faults and no consumption of
  the valid pair.

Readiness events and bounded deadlines replace fixed sleeps. Destructive cases
operate on private copies, so later checks continue to use the canonical signed
bundle.

## Inventory disposition

The following records remain `missing-platform-feasible`, with #1351 as the
delivery issue, because each still aggregates later resource or Linux-outcome
work:

- `semantic.isolation:host-resource-authority-and-brokerage`
- `semantic.isolation:jailer-seccomp-and-macos-containment-outcomes`
- `semantic.isolation:multiprocess-concurrency-redaction-and-failure-atomicity`

The delivered package/session/grant/fd/crash subset, including exact adoption by
the four startup-input consumers, is real but does not complete any of those
composite records because remaining consumers, dynamic-broker, network,
Linux-outcome, and deployment work remains. The broad `jailer`, `seccomp`,
`seccompiler`, and `production-host` corpus records remain `audit-required`.
Neither this audit nor the executable evidence is direct Firecracker jailer
parity.
