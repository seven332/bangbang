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
endpoint to descriptor 3, an unnamed startup-grant datagram endpoint to
descriptor 4, and one dormant socket-broker datagram endpoint to descriptor 5.
The launcher dynamically validates the live
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
`Proceed`. No endpoint, argument, identity byte, or resource grant is stored
there at that gate. After authorization, socket publication may add at most the
two fixed strict role/child/socket-identity ownership records. Worker EOF cleanup covers
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
closing the launcher's copy is cleanup rather than revocation. The private
namespace itself grants no resource authority; its optional fixed socket
records carry only role, safe child, and device/inode cleanup evidence.

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

Drive and pmem roles are repeatable, so their explicit grant ID is the only
selection key; no role-based or device-name lookup exists. A contained
pre-boot `PUT` first validates complete action, lifecycle, device ID, root and
ordering rules, path shape, and limiter state. It then claims the exact
`DriveBacking` or `PmemBacking` role with read-only/read-write access derived
from the validated device configuration, constructs the device-specific
backing from the transferred file, and atomically commits public configuration
plus private per-ID ownership. Mismatch, malformed/missing/consumed references,
backing validation, or candidate validation preserve the previous config and
backing. A successful same-ID ordinary-path `PUT` deliberately drops prepared
grant ownership and retains deferred path opening.

Startup uses one move-only aggregate for boot files and exact-ID block/pmem
backings. It preflights every private entry for prior consumption before moving
anything, follows public device order rather than map order, and rejects
provided IDs absent from configuration. Prepared entries become consumed when
moved; a later VM-start failure therefore requires a fresh same-ID `PUT` for
each affected device, while unrelated entries are not partially moved by an
early preflight failure.

After `Ready`, the immutable startup batch can still contain unused grants.
Only the existing path-changing `PATCH /drives/{id}` consumes one: lifecycle
and full updated-config validation precede the exact claim, the opened backing
is passed to the active session, and public config commits only after the
handler swap. If the later active transition fails, the prior active
device/config remains but the claimed grant is one-time consumed. Path-free
drive updates and pmem limiter `PATCH` claim nothing and retain the active
backing; pmem has no live backing replacement. Authorized `GET /vm/config`
responses may contain submitted tags. Faults, errors, logs, and nested debug
output exclude tags, IDs, paths, descriptor identity, and contents.

Logger, metrics, and serial consume singleton exact-ID `WriteOnly` grants only
after complete lifecycle and input validation. The worker adopts each existing
regular file without reopening its tag, preserves kernel-enforced write-only
access, and sets and verifies append/nonblocking status. Logger path-free
updates retain the installed sink and claim nothing; a path-bearing update
commits the replacement sink and fields together. Metrics rejects repeat
initialization before claim and retains the adopted sink through initial,
periodic, explicit, and terminal transactions.

Serial retains one prepared output beside the committed configuration. A clear
or replacement drops that ownership; startup moves it once through the shared
resource aggregate and marks it consumed. A later startup failure requires a
validated serial reconfiguration before retry. Direct paths keep their current
creation/FIFO behavior and logger/metrics-versus-serial open timing. Pending,
replaced, active, cancelled, and terminal files close by ordinary cooperative
ownership; no hard revocation is claimed.

Snapshot describe/state/memory inputs use the same exact file-reference grammar
with distinct singleton read-only roles. Early description duplicates and
inspects only the granted regular file. Load duplicates state only for bounded
decode, discovers any persisted grant-tagged root selector, then atomically
takes every tagged state, memory, and read-only `DriveBacking` input. The
prepared state, anonymous memory, and supplied root file complete restore
without reopening a submitted or persisted tag. Direct and mixed ordinary
members retain pathname adapters. Snapshot input grants are one-time after the
atomic take; wrong, missing, duplicate, or mismatched authority consumes none.

Snapshot outputs use `bangbang-grant:<GrantId>/<SnapshotOutputChild>`, where
the child is one 1–255 byte UTF-8 component with no NUL or `/` and is not `.` or
`..`. `SnapshotOutputDirectory` is repeatable across distinct grants and one
retained grant may serve distinct state/memory children and later create
requests. Complete request/profile validation precedes adoption. Publication
creates staging and final files relative to retained anchors, preserves
exclusive memory-first/state-last commit and typed orphan/durability behavior,
and never reopens the bookmark-resolved path. App Sandbox scope still depends
on the authorized directory remaining reachable at its granted pathname;
moving that directory after scope activation can make descriptor-relative
writes fail.

For each granted staging inode, the worker durably writes one strict private
record containing only artifact kind, exact directory identity, the bounded
random component, and exact file identity. Normal publication or conclusive
worker cleanup clears it. After worker exit, the launcher matches the record to
its retained exact output anchor and unlinks only a current-user regular `0600`,
single-link device/inode match; missing or replaced entries are preserved before
the record and namespace are cleared. A hard death between file creation and
recording, or simultaneous uncatchable launcher/worker death, can still leave
residue. Darwin offers no identity-conditional unlink primitive that closes
those windows.

API and vsock sockets use the distinct exact contained reference
`bangbang-grant:<GrantId>/<SocketChild>`. The child is one 1–64 byte ASCII
`[A-Za-z0-9._-]` component other than `.` or `..`; malformed, traversal,
separator, control, non-ASCII, missing, mismatched, or consumed values fail
without ambient fallback. Direct mode preserves identical bytes as ordinary
paths. Claims require the exact singleton `CreateChildren` role/access and
complete validation first. No-API mode consumes no API-directory authority;
vsock claims retain their scope and anchor through deferred startup, and a
rejected replacement preserves both prior public configuration and private
ownership.

One short-lived default-close instance of the signed worker receives only a
parent-authenticated control endpoint and the exact private namespace anchor,
binds a fixed role-specific staging name there, validates and transfers one
owner-only listener, and is reaped before readiness or VM-start success. The
main worker records strict role/child/socket identity, requires the namespace
and grant anchor to share a filesystem, and publishes exclusively with
fd-relative `renameatx_np(RENAME_EXCL)`. It retains scope, anchor, supplied
listener, and identity-aware cleanup. Existing, replaced, cross-filesystem, or
identity-mismatched targets fail closed and value-redacted.

The supplied API listener serves outside-container clients only after
publication. The supplied vsock main listener accepts host-initiated traffic.
Guest-initiated traffic uses the inherited descriptor-5 facet, which remains
dormant otherwise. One `Activate` fixes exact peer PID, lifecycle SessionId,
first sequence, retained `VsockSocketDirectory` anchor, cwd identity, and safe
child. Thereafter the worker sends only monotonic sequences plus a `u32` port;
the launcher constructs relative `<SocketChild>_<port>`, checks the socket
target before and after a nonblocking connect, and returns at most one validated
AF_UNIX stream descriptor. It receives no guest payload, grant ID, bookmark,
resolved path, arbitrary child, or general selector. Closed framing, exact
rights counts, shutdown/EOF, and lifecycle loss fail closed. No code object,
long-lived helper, or `network.client` entitlement is added.

Normal worker cleanup unlinks only a still-matching socket. After worker exit,
the launcher can read only the two strict records and use its retained matching
role anchors plus the fixed private staging names for the same strict
owner/mode/link/device/inode check before clearing the records and namespace.
Launcher-first and worker-first death preserve that cooperative ordering and
replaced targets. Simultaneous uncatchable death may leave a stale external
socket name and private ownership record because Darwin has no
unlink-on-final-close facility; later automatic recovery remains limited to
empty session namespaces.

The following remain feasible work owned by #1351:

- general dynamic post-Ready delivery and any hard-revocation broker;
- cross-filesystem socket publication;
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
  retention of only lifecycle/grant/dormant-broker endpoints, and malformed/incompatible
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
- worker-first and launcher-first namespace cleanup, empty both-killed bounded
  stale recovery, and two concurrent API sessions remaining independent when one
  worker dies;
- both sealed and external-grant config/metadata/kernel/initrd inputs plus
  repeatable read-only/read-write block and pmem inputs starting real sandboxed
  HVF guests through no-API production launches and ending successfully through
  PSCI `SYSTEM_OFF`;
- delayed API-time atomic boot adoption retaining the opened file identities
  after pathname replacement and returning the authorized references from
  `GET /vm/config`;
- invalid-command-line, wrong-role, and missing boot requests preserving the
  prior public configuration, with redacted grant faults and no consumption of
  the valid pair;
- delayed exact role/access device claims, wrong-role/wrong-access/malformed and
  duplicate-use failure, same-ID rollback, authorized block/pmem tags, and
  limiter-only updates that retain backing ownership;
- source pathname replacement after launcher preparation followed by real guest
  writable block I/O and pmem marker read/flush persistence only in the
  launcher-opened objects;
- a read-only transferred block backing rejecting a real guest write while its
  opened file remains unchanged; and
- preauthorized after-start block replacement synchronized by guest
  virtio-mem ready/grow/shrink markers so later writes reach the already-opened
  replacement object rather than the planted pathname;
- startup-CLI/config-file and delayed-API logger/metrics/serial adoption by the
  normal worker, including source-path replacement, append sentinels, redacted
  mismatch and one-time failures, initial/terminal metrics, and real guest
  console bytes written only through the launcher-opened descriptors; and
- two simultaneous output-grant sessions using identical GrantIds but isolated
  registries, mutually exclusive logger filters, independent metrics/serial
  files, and unchanged planted replacement paths;
- an exact API-directory claim publishing an owner-only listener outside the
  container, serving a real client after readiness, and reaping its transient
  signed binder before exposure;
- a delayed exact vsock-directory claim publishing and supplying the main
  listener while leaving only launcher plus worker with unchanged exact
  entitlements after startup;
- a real guest reaching two distinct host ports through only the fixed launcher
  facet and connected stream descriptors, with no guest payload crossing it;
  and
- a real host completing deterministic 1-MiB bidirectional transfer and both
  write-half-close/EOF directions through the supplied granted main listener,
  followed by identity-owned API/vsock socket and namespace cleanup; and
- launcher-first and worker-first abrupt death after granted API pathname
  replacement, with both surviving cleanup owners preserving the replacement
  while clearing only the matching strict record and session namespace;
- external granted native-v1 create into separate directories, retained-anchor
  reuse for a second pair, same-GrantId concurrent-session isolation, bounded
  early description, and two fresh descriptor-bound state/memory/root restores
  with explicit and automatic resume through guest `SYSTEM_OFF`; and
- worker-first death after a durably recorded snapshot staging inode, with the
  launcher removing the exact inode or preserving a same-name replacement while
  clearing the private record and session namespace.

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
the singleton startup inputs/outputs, repeatable block/pmem consumers, and
singleton API/vsock directories plus the fixed port-only vsock facet, and
snapshot describe/state/memory/root/output consumers with exact crash cleanup,
is real but does not complete any of those composite records because general
dynamic brokerage/hard revocation, network,
Linux-outcome, and deployment work remains. The broad `jailer`, `seccomp`,
`seccompiler`, and `production-host` corpus records remain `audit-required`.
Neither this audit nor the executable evidence is direct Firecracker jailer
parity.
